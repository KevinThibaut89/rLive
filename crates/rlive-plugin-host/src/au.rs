//! Audio Unit (v2 API) hosting on macOS.
//!
//! AUv3 extensions are reachable through the same API: AudioToolbox bridges them.

use crate::{HostError, HostNotification, PluginDescriptor, PluginHandle, PluginInstance, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use objc2_app_kit::NSView;
use objc2_audio_toolbox::*;
use objc2_core_audio_types::*;
use objc2_core_foundation::{
    CFArray, CFData, CFPropertyList, CFPropertyListCreateData, CFPropertyListCreateWithData,
    CFPropertyListFormat, CFRetained, CFString,
};
use objc2_foundation::{NSBundle, NSSize, NSString, NSURL};
use rlive_core::{MidiMessage, ParamInfo, ParamKind, PluginFormat, ProcessContext, Processor};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const NO_ERR: i32 = 0;

fn fourcc(v: u32) -> String {
    v.to_be_bytes()
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

fn parse_fourcc(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    (b.len() == 4).then(|| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn description_from_id(id: &str) -> Option<AudioComponentDescription> {
    let mut parts = id.split(':');
    let (t, s, m) = (parts.next()?, parts.next()?, parts.next()?);
    Some(AudioComponentDescription {
        componentType: parse_fourcc(t)?,
        componentSubType: parse_fourcc(s)?,
        componentManufacturer: parse_fourcc(m)?,
        componentFlags: 0,
        componentFlagsMask: 0,
    })
}

/// Enumerate installed instruments and effects.
pub fn scan() -> Vec<PluginDescriptor> {
    let mut out = Vec::new();
    for ty in [
        kAudioUnitType_MusicDevice,
        kAudioUnitType_Effect,
        kAudioUnitType_MusicEffect,
        kAudioUnitType_Generator,
    ] {
        let mut query = AudioComponentDescription {
            componentType: ty,
            componentSubType: 0,
            componentManufacturer: 0,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        let mut comp: AudioComponent = std::ptr::null_mut();
        loop {
            comp = unsafe { AudioComponentFindNext(comp, NonNull::from(&mut query)) };
            if comp.is_null() {
                break;
            }
            let mut d = query;
            unsafe { AudioComponentGetDescription(comp, NonNull::from(&mut d)) };
            let mut name_ref: *const CFString = std::ptr::null();
            let full_name = unsafe {
                if AudioComponentCopyName(comp, NonNull::from(&mut name_ref)) == NO_ERR
                    && !name_ref.is_null()
                {
                    let s = CFRetained::from_raw(NonNull::new_unchecked(name_ref as *mut CFString));
                    s.to_string()
                } else {
                    String::from("Unknown")
                }
            };
            let (vendor, name) = match full_name.split_once(": ") {
                Some((v, n)) => (v.to_string(), n.to_string()),
                None => (fourcc(d.componentManufacturer), full_name),
            };
            let mut version = 0u32;
            unsafe { AudioComponentGetVersion(comp, NonNull::from(&mut version)) };
            out.push(PluginDescriptor {
                format: PluginFormat::AudioUnit,
                id: format!(
                    "{}:{}:{}",
                    fourcc(d.componentType),
                    fourcc(d.componentSubType),
                    fourcc(d.componentManufacturer)
                ),
                name,
                vendor,
                version: format!(
                    "{}.{}.{}",
                    version >> 16,
                    (version >> 8) & 0xff,
                    version & 0xff
                ),
                path: Default::default(),
                is_instrument: ty == kAudioUnitType_MusicDevice,
            });
        }
    }
    out
}

/// Transport state the AU can query through host callbacks during render.
#[derive(Default)]
struct HostTiming {
    tempo: std::cell::Cell<f64>,
    beat: std::cell::Cell<f64>,
    playing: std::cell::Cell<bool>,
    numerator: std::cell::Cell<u32>,
    denominator: std::cell::Cell<u32>,
    sample_pos: std::cell::Cell<f64>,
}

struct AuInstance {
    unit: AudioUnit,
    sample_rate: f64,
    timing: Box<HostTiming>,
}

unsafe impl Send for AuInstance {}
unsafe impl Sync for AuInstance {}

impl Drop for AuInstance {
    fn drop(&mut self) {
        unsafe {
            AudioUnitUninitialize(self.unit);
            AudioComponentInstanceDispose(self.unit);
        }
        let _ = &self.timing;
    }
}

unsafe extern "C-unwind" fn beat_and_tempo(
    user: *mut c_void,
    beat: *mut f64,
    tempo: *mut f64,
) -> i32 {
    let t = &*(user as *const HostTiming);
    if !beat.is_null() {
        *beat = t.beat.get();
    }
    if !tempo.is_null() {
        *tempo = t.tempo.get();
    }
    NO_ERR
}

unsafe extern "C-unwind" fn musical_time(
    user: *mut c_void,
    delta_to_next_beat: *mut u32,
    numerator: *mut f32,
    denominator: *mut u32,
    downbeat: *mut f64,
) -> i32 {
    let t = &*(user as *const HostTiming);
    let beat = t.beat.get();
    let num = t.numerator.get().max(1) as f64;
    if !delta_to_next_beat.is_null() {
        *delta_to_next_beat = 0;
    }
    if !numerator.is_null() {
        *numerator = num as f32;
    }
    if !denominator.is_null() {
        *denominator = t.denominator.get().max(1);
    }
    if !downbeat.is_null() {
        *downbeat = (beat / num).floor() * num;
    }
    NO_ERR
}

unsafe extern "C-unwind" fn transport_state(
    user: *mut c_void,
    is_playing: *mut u8,
    changed: *mut u8,
    sample_pos: *mut f64,
    cycling: *mut u8,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> i32 {
    let t = &*(user as *const HostTiming);
    if !is_playing.is_null() {
        *is_playing = t.playing.get() as u8;
    }
    if !changed.is_null() {
        *changed = 0;
    }
    if !sample_pos.is_null() {
        *sample_pos = t.sample_pos.get();
    }
    if !cycling.is_null() {
        *cycling = 0;
    }
    if !cycle_start.is_null() {
        *cycle_start = 0.0;
    }
    if !cycle_end.is_null() {
        *cycle_end = 0.0;
    }
    NO_ERR
}

fn stereo_format(sample_rate: f64) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: sample_rate,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat
            | kAudioFormatFlagIsPacked
            | kAudioFormatFlagIsNonInterleaved,
        mBytesPerPacket: 4,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4,
        mChannelsPerFrame: 2,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

unsafe fn set_property<T>(unit: AudioUnit, id: u32, scope: u32, element: u32, value: &T) -> i32 {
    AudioUnitSetProperty(
        unit,
        id,
        scope,
        element,
        value as *const T as *const c_void,
        std::mem::size_of::<T>() as u32,
    )
}

/// Input audio handed to effect AUs through the render callback.
struct InputData {
    left: Vec<f32>,
    right: Vec<f32>,
}

unsafe extern "C-unwind" fn input_callback(
    refcon: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    _ts: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    io_data: *mut AudioBufferList,
) -> i32 {
    let input = &mut *(refcon.as_ptr() as *mut InputData);
    if io_data.is_null() {
        return NO_ERR;
    }
    let n = (frames as usize).min(input.left.len());
    let list = io_data as *mut StereoBufferList;
    let count = (*list).number_buffers.min(2) as usize;
    for (i, src) in [&mut input.left, &mut input.right]
        .into_iter()
        .enumerate()
        .take(count)
    {
        let buf = &mut (*list).buffers[i];
        if buf.mData.is_null() {
            buf.mData = src.as_mut_ptr() as *mut c_void;
        } else {
            std::ptr::copy_nonoverlapping(src.as_ptr(), buf.mData as *mut f32, n);
        }
        buf.mDataByteSize = (n * 4) as u32;
    }
    NO_ERR
}

/// `AudioBufferList` with room for two buffers (the C struct is variable-length).
#[repr(C)]
struct StereoBufferList {
    number_buffers: u32,
    buffers: [AudioBuffer; 2],
}

pub fn instantiate(
    desc: &PluginDescriptor,
    sample_rate: f64,
    max_block: usize,
) -> Result<PluginInstance> {
    let mut query = description_from_id(&desc.id)
        .ok_or_else(|| HostError::NotFound(desc.id.clone(), Default::default()))?;
    let comp = unsafe { AudioComponentFindNext(std::ptr::null_mut(), NonNull::from(&mut query)) };
    if comp.is_null() {
        return Err(HostError::NotFound(desc.id.clone(), Default::default()));
    }
    let mut unit: AudioComponentInstance = std::ptr::null_mut();
    let status = unsafe { AudioComponentInstanceNew(comp, NonNull::from(&mut unit)) };
    if status != NO_ERR || unit.is_null() {
        return Err(HostError::Init(format!(
            "AudioComponentInstanceNew failed ({status})"
        )));
    }
    let inst = Arc::new(AuInstance {
        unit,
        sample_rate,
        timing: Box::new(HostTiming {
            tempo: 120.0.into(),
            numerator: 4.into(),
            denominator: 4.into(),
            ..Default::default()
        }),
    });
    let is_effect = query.componentType != kAudioUnitType_MusicDevice
        && query.componentType != kAudioUnitType_Generator;
    let fmt = stereo_format(sample_rate);
    let max_frames = max_block as u32;
    let mut input = Box::new(InputData {
        left: vec![0.0; max_block],
        right: vec![0.0; max_block],
    });
    unsafe {
        set_property(
            unit,
            kAudioUnitProperty_StreamFormat,
            kAudioUnitScope_Output,
            0,
            &fmt,
        );
        if is_effect {
            set_property(
                unit,
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Input,
                0,
                &fmt,
            );
            let cb = AURenderCallbackStruct {
                inputProc: Some(input_callback),
                inputProcRefCon: &mut *input as *mut InputData as *mut c_void,
            };
            set_property(
                unit,
                kAudioUnitProperty_SetRenderCallback,
                kAudioUnitScope_Input,
                0,
                &cb,
            );
        }
        set_property(
            unit,
            kAudioUnitProperty_MaximumFramesPerSlice,
            kAudioUnitScope_Global,
            0,
            &max_frames,
        );
        let callbacks = HostCallbackInfo {
            hostUserData: &*inst.timing as *const HostTiming as *mut c_void,
            beatAndTempoProc: Some(beat_and_tempo),
            musicalTimeLocationProc: Some(musical_time),
            transportStateProc: Some(transport_state),
            transportStateProc2: None,
        };
        set_property(
            unit,
            kAudioUnitProperty_HostCallbacks,
            kAudioUnitScope_Global,
            0,
            &callbacks,
        );
        let status = AudioUnitInitialize(unit);
        if status != NO_ERR {
            return Err(HostError::Activate(format!(
                "AudioUnitInitialize failed ({status})"
            )));
        }
    }

    let mut handle = AuHandle {
        inst: inst.clone(),
        desc: desc.clone(),
        params: Vec::new(),
        editor: None,
    };
    handle.rescan_params();
    let processor = AuProcessor {
        inst,
        input,
        output: [vec![0.0; max_block], vec![0.0; max_block]],
        pending: Vec::with_capacity(256),
        is_effect,
        sample_time: 0.0,
        max_block,
    };
    Ok(PluginInstance {
        handle: Box::new(handle),
        processor: Box::new(processor),
    })
}

// ------------------------------------------------------------------ handle

pub struct AuHandle {
    inst: Arc<AuInstance>,
    desc: PluginDescriptor,
    params: Vec<ParamInfo>,
    editor: Option<crate::mac::EditorWindow>,
}

impl AuHandle {
    fn rescan_params(&mut self) {
        self.params.clear();
        let unit = self.inst.unit;
        unsafe {
            let mut size = 0u32;
            if AudioUnitGetPropertyInfo(
                unit,
                kAudioUnitProperty_ParameterList,
                kAudioUnitScope_Global,
                0,
                &mut size,
                std::ptr::null_mut(),
            ) != NO_ERR
                || size == 0
            {
                return;
            }
            let mut ids = vec![0u32; size as usize / 4];
            if AudioUnitGetProperty(
                unit,
                kAudioUnitProperty_ParameterList,
                kAudioUnitScope_Global,
                0,
                NonNull::new_unchecked(ids.as_mut_ptr() as *mut c_void),
                NonNull::from(&mut size),
            ) != NO_ERR
            {
                return;
            }
            for id in ids {
                let mut info: AudioUnitParameterInfo = std::mem::zeroed();
                let mut isize = std::mem::size_of::<AudioUnitParameterInfo>() as u32;
                if AudioUnitGetProperty(
                    unit,
                    kAudioUnitProperty_ParameterInfo,
                    kAudioUnitScope_Global,
                    id,
                    NonNull::new_unchecked(&mut info as *mut _ as *mut c_void),
                    NonNull::from(&mut isize),
                ) != NO_ERR
                {
                    continue;
                }
                let name = if info
                    .flags
                    .contains(AudioUnitParameterOptions::Flag_HasCFNameString)
                    && !info.cfNameString.is_null()
                {
                    let s = (*info.cfNameString).to_string();
                    if info
                        .flags
                        .contains(AudioUnitParameterOptions::Flag_CFNameRelease)
                    {
                        drop(CFRetained::from_raw(NonNull::new_unchecked(
                            info.cfNameString as *mut CFString,
                        )));
                    }
                    s
                } else {
                    crate::cbuf_to_string(&info.name)
                };
                let kind = if info.unit == AudioUnitParameterUnit::Boolean {
                    ParamKind::Toggle
                } else if info.unit == AudioUnitParameterUnit::Indexed {
                    let labels = self.value_strings(id);
                    if labels.is_empty() {
                        ParamKind::Stepped {
                            labels: (info.minValue as i64..=info.maxValue as i64)
                                .map(|v| v.to_string())
                                .collect(),
                        }
                    } else {
                        ParamKind::Stepped { labels }
                    }
                } else {
                    ParamKind::Continuous { skew: 1.0 }
                };
                self.params.push(ParamInfo {
                    id,
                    name,
                    min: info.minValue as f64,
                    max: info.maxValue as f64,
                    default: info.defaultValue as f64,
                    unit: String::new(),
                    kind,
                });
            }
        }
    }

    fn value_strings(&self, id: u32) -> Vec<String> {
        unsafe {
            let mut arr: *const CFArray = std::ptr::null();
            let mut size = std::mem::size_of::<*const CFArray>() as u32;
            if AudioUnitGetProperty(
                self.inst.unit,
                kAudioUnitProperty_ParameterValueStrings,
                kAudioUnitScope_Global,
                id,
                NonNull::new_unchecked(&mut arr as *mut _ as *mut c_void),
                NonNull::from(&mut size),
            ) != NO_ERR
                || arr.is_null()
            {
                return Vec::new();
            }
            let arr = CFRetained::from_raw(NonNull::new_unchecked(arr as *mut CFArray));
            (0..arr.count())
                .map(|i| {
                    let s = arr.value_at_index(i) as *const CFString;
                    if s.is_null() {
                        String::new()
                    } else {
                        (*s).to_string()
                    }
                })
                .collect()
        }
    }

    fn cocoa_view(&self) -> Option<Retained<NSView>> {
        unsafe {
            let unit = self.inst.unit;
            let mut size = 0u32;
            if AudioUnitGetPropertyInfo(
                unit,
                kAudioUnitProperty_CocoaUI,
                kAudioUnitScope_Global,
                0,
                &mut size,
                std::ptr::null_mut(),
            ) != NO_ERR
                || (size as usize) < std::mem::size_of::<AudioUnitCocoaViewInfo>()
            {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            if AudioUnitGetProperty(
                unit,
                kAudioUnitProperty_CocoaUI,
                kAudioUnitScope_Global,
                0,
                NonNull::new_unchecked(buf.as_mut_ptr() as *mut c_void),
                NonNull::from(&mut size),
            ) != NO_ERR
            {
                return None;
            }
            let info = &*(buf.as_ptr() as *const AudioUnitCocoaViewInfo);
            let url = CFRetained::from_raw(info.mCocoaAUViewBundleLocation);
            let class_name = CFRetained::from_raw(info.mCocoaAUViewClass[0]);
            // CFURL/CFString are toll-free bridged to NSURL/NSString.
            let ns_url = &*(CFRetained::as_ptr(&url).as_ptr() as *const NSURL);
            let bundle = NSBundle::bundleWithURL(ns_url)?;
            let ns_class = NSString::from_str(&class_name.to_string());
            let cls: &AnyClass = bundle.classNamed(&ns_class)?;
            let factory: Option<Retained<AnyObject>> = msg_send![cls, new];
            let factory = factory?;
            // -[AUCocoaUIBase uiViewForAudioUnit:withSize:]; called through objc_msgSend directly
            // because AudioUnit is an opaque C pointer.
            let sel: Sel = sel!(uiViewForAudioUnit:withSize:);
            let send: unsafe extern "C" fn(*mut AnyObject, Sel, AudioUnit, NSSize) -> *mut NSView =
                std::mem::transmute(objc2::ffi::objc_msgSend as *const c_void);
            let view = send(
                Retained::as_ptr(&factory) as *mut AnyObject,
                sel,
                unit,
                NSSize::new(400.0, 300.0),
            );
            Retained::retain(view)
        }
    }
}

impl PluginHandle for AuHandle {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.desc
    }

    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }

    fn param_value(&self, id: u32) -> Option<f64> {
        let mut v: f32 = 0.0;
        let st = unsafe {
            AudioUnitGetParameter(
                self.inst.unit,
                id,
                kAudioUnitScope_Global,
                0,
                NonNull::from(&mut v),
            )
        };
        (st == NO_ERR).then_some(v as f64)
    }

    fn format_value(&self, id: u32, value: f64) -> Option<String> {
        unsafe {
            let mut v = value as f32;
            let mut req = AudioUnitParameterStringFromValue {
                inParamID: id,
                inValue: NonNull::from(&mut v),
                outString: std::ptr::null(),
            };
            let mut size = std::mem::size_of::<AudioUnitParameterStringFromValue>() as u32;
            if AudioUnitGetProperty(
                self.inst.unit,
                kAudioUnitProperty_ParameterStringFromValue,
                kAudioUnitScope_Global,
                0,
                NonNull::new_unchecked(&mut req as *mut _ as *mut c_void),
                NonNull::from(&mut size),
            ) != NO_ERR
                || req.outString.is_null()
            {
                return None;
            }
            let s = CFRetained::from_raw(NonNull::new_unchecked(req.outString as *mut CFString));
            Some(s.to_string())
        }
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        unsafe {
            let mut plist: *const CFPropertyList = std::ptr::null();
            let mut size = std::mem::size_of::<*const CFPropertyList>() as u32;
            if AudioUnitGetProperty(
                self.inst.unit,
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                NonNull::new_unchecked(&mut plist as *mut _ as *mut c_void),
                NonNull::from(&mut size),
            ) != NO_ERR
                || plist.is_null()
            {
                return None;
            }
            let plist = CFRetained::from_raw(NonNull::new_unchecked(plist as *mut CFPropertyList));
            let data = CFPropertyListCreateData(
                None,
                Some(&plist),
                CFPropertyListFormat::BinaryFormat_v1_0,
                0,
                std::ptr::null_mut(),
            )?;
            let len = data.length() as usize;
            let ptr = data.byte_ptr();
            Some(std::slice::from_raw_parts(ptr, len).to_vec())
        }
    }

    fn load_state(&mut self, data: &[u8]) -> bool {
        unsafe {
            let Some(cfdata) = CFData::new(None, data.as_ptr(), data.len() as isize) else {
                return false;
            };
            let Some(plist) = CFPropertyListCreateWithData(
                None,
                Some(&cfdata),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) else {
                return false;
            };
            let ptr = CFRetained::as_ptr(&plist).as_ptr() as *const CFPropertyList;
            let ok = set_property(
                self.inst.unit,
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                &ptr,
            ) == NO_ERR;
            if ok {
                self.rescan_params();
            }
            ok
        }
    }

    fn has_editor(&self) -> bool {
        let mut size = 0u32;
        unsafe {
            AudioUnitGetPropertyInfo(
                self.inst.unit,
                kAudioUnitProperty_CocoaUI,
                kAudioUnitScope_Global,
                0,
                &mut size,
                std::ptr::null_mut(),
            ) == NO_ERR
                && size > 0
        }
    }

    fn is_editor_open(&self) -> bool {
        self.editor.is_some()
    }

    fn open_editor(&mut self) -> Result<()> {
        if self.editor.is_some() {
            return Ok(());
        }
        let view = self.cocoa_view().ok_or(HostError::NoEditor)?;
        let size = view.frame().size;
        let window = crate::mac::EditorWindow::new(&self.desc.name, size.width, size.height, false)
            .ok_or_else(|| HostError::Editor("could not create window".into()))?;
        window.set_content(&view);
        window.show();
        self.editor = Some(window);
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(w) = self.editor.take() {
            w.close();
        }
    }

    fn idle(&mut self) -> Vec<HostNotification> {
        if matches!(&self.editor, Some(w) if !w.is_visible()) {
            self.editor = None;
            return vec![HostNotification::EditorClosed];
        }
        Vec::new()
    }

    fn latency(&self) -> u32 {
        let mut secs: f64 = 0.0;
        let mut size = 8u32;
        unsafe {
            AudioUnitGetProperty(
                self.inst.unit,
                kAudioUnitProperty_Latency,
                kAudioUnitScope_Global,
                0,
                NonNull::new_unchecked(&mut secs as *mut f64 as *mut c_void),
                NonNull::from(&mut size),
            );
        }
        (secs * self.inst.sample_rate) as u32
    }
}

impl Drop for AuHandle {
    fn drop(&mut self) {
        self.close_editor();
    }
}

// ------------------------------------------------------------------ processor

pub struct AuProcessor {
    inst: Arc<AuInstance>,
    input: Box<InputData>,
    output: [Vec<f32>; 2],
    pending: Vec<(u32, f32)>,
    is_effect: bool,
    sample_time: f64,
    max_block: usize,
}

unsafe impl Send for AuProcessor {}

static WARNED: AtomicBool = AtomicBool::new(false);

impl Processor for AuProcessor {
    fn activate(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        let unit = self.inst.unit;
        let frames = ctx.audio.frames().min(self.max_block);
        let t = ctx.transport;
        let timing = &self.inst.timing;
        timing.tempo.set(t.tempo);
        timing.beat.set(t.beats());
        timing.playing.set(t.playing);
        timing.numerator.set(t.time_signature.numerator as u32);
        timing.denominator.set(t.time_signature.denominator as u32);
        timing.sample_pos.set(t.sample_pos as f64);

        unsafe {
            for &(id, v) in &self.pending {
                AudioUnitSetParameter(unit, id, kAudioUnitScope_Global, 0, v, 0);
            }
            self.pending.clear();
            if !self.is_effect {
                for e in ctx.events {
                    let (bytes, _) = e.message.to_bytes();
                    if matches!(e.message, MidiMessage::AllNotesOff) {
                        for ch in 0..16u32 {
                            MusicDeviceMIDIEvent(unit, 0xb0 | ch, 123, 0, e.offset);
                        }
                    } else {
                        MusicDeviceMIDIEvent(
                            unit,
                            bytes[0] as u32,
                            bytes[1] as u32,
                            bytes[2] as u32,
                            e.offset,
                        );
                    }
                }
            }
        }

        let left = &mut ctx.audio.left[..frames];
        let right = &mut ctx.audio.right[..frames];
        if self.is_effect {
            self.input.left[..frames].copy_from_slice(left);
            self.input.right[..frames].copy_from_slice(right);
        }
        let mut list = StereoBufferList {
            number_buffers: 2,
            buffers: [
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: (frames * 4) as u32,
                    mData: self.output[0].as_mut_ptr() as *mut c_void,
                },
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: (frames * 4) as u32,
                    mData: self.output[1].as_mut_ptr() as *mut c_void,
                },
            ],
        };
        let mut ts: AudioTimeStamp = unsafe { std::mem::zeroed() };
        ts.mSampleTime = self.sample_time;
        ts.mFlags = AudioTimeStampFlags::SampleTimeValid;
        let mut flags = AudioUnitRenderActionFlags(0);
        let status = unsafe {
            AudioUnitRender(
                unit,
                &mut flags,
                NonNull::from(&mut ts),
                0,
                frames as u32,
                NonNull::new_unchecked(&mut list as *mut StereoBufferList as *mut AudioBufferList),
            )
        };
        self.sample_time += frames as f64;
        if status != NO_ERR {
            if !WARNED.swap(true, Ordering::Relaxed) {
                // Logging allocates, but this happens once per process at most.
                log::warn!("AudioUnitRender failed: {status}");
            }
            return;
        }
        // The AU may have substituted its own buffers.
        let out_l =
            unsafe { std::slice::from_raw_parts(list.buffers[0].mData as *const f32, frames) };
        let out_r =
            unsafe { std::slice::from_raw_parts(list.buffers[1].mData as *const f32, frames) };
        if self.is_effect {
            left.copy_from_slice(out_l);
            right.copy_from_slice(out_r);
        } else {
            for i in 0..frames {
                left[i] += out_l[i];
                right[i] += out_r[i];
            }
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if self.pending.len() < self.pending.capacity() {
            self.pending.push((id, value as f32));
        }
    }

    fn reset(&mut self) {
        unsafe {
            AudioUnitReset(self.inst.unit, kAudioUnitScope_Global, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_roundtrip() {
        assert_eq!(fourcc(kAudioUnitType_Effect), "aufx");
        assert_eq!(parse_fourcc("aufx"), Some(kAudioUnitType_Effect));
    }

    /// Apple's built-in AUDelay ships with every macOS install.
    #[test]
    fn apple_delay_instantiates_and_processes() {
        let found = scan();
        let delay = found
            .iter()
            .find(|d| d.id == "aufx:dely:appl")
            .expect("AUDelay should be installed");
        let mut inst = instantiate(delay, 48_000.0, 512).unwrap();
        assert!(!inst.handle.params().is_empty());
        let transport = rlive_core::TransportInfo::default();
        let mut q = rlive_core::ParamOutQueue::with_capacity(8);
        let mut l = vec![0.5f32; 512];
        let mut r = vec![0.5f32; 512];
        inst.processor.process(&mut ProcessContext {
            audio: rlive_core::AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            events: &[],
            transport: &transport,
            param_out: &mut q,
        });
        assert!(l.iter().all(|s| s.is_finite()));
        let state = inst.handle.save_state().expect("AU state");
        assert!(inst.handle.load_state(&state));
    }
}
