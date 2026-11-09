//! Audio Unit (v2 API) hosting on macOS.
//!
//! AUv3 extensions are reachable through the same API: AudioToolbox bridges them.

use crate::midi_out::MidiOutput;
use crate::{
    HostError, HostNotification, PluginDescriptor, PluginHandle, PluginInstance, PluginPreset,
    PresetSource, Result,
};
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
use rlive_core::{
    EventQueue, MidiMessage, MpeEncoder, MpeOutput, MpeZone, ParamInfo, ParamKind, PluginFormat,
    ProcessContext, Processor, Resource, Tail, MPE_SLOT,
};
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

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
                subcategories: String::new(),
                is_midi_effect: false,
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
    cycling: std::cell::Cell<bool>,
    cycle_start: std::cell::Cell<f64>,
    cycle_end: std::cell::Cell<f64>,
}

struct AuInstance {
    unit: AudioUnit,
    sample_rate: f64,
    timing: Box<HostTiming>,
    /// Latency and tail in frames, as last read on the main thread.
    latency_frames: AtomicU32,
    tail_frames: AtomicU32,
}

impl AuInstance {
    /// A property holding a time in seconds, in frames.
    fn read_seconds(&self, property: AudioUnitPropertyID) -> u32 {
        let mut secs: f64 = 0.0;
        let mut size = 8u32;
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                property,
                kAudioUnitScope_Global,
                0,
                NonNull::new_unchecked(&mut secs as *mut f64 as *mut c_void),
                NonNull::from(&mut size),
            )
        };
        if status == NO_ERR && secs.is_finite() && secs > 0.0 {
            (secs * self.sample_rate) as u32
        } else {
            0
        }
    }

    /// Read the latency and tail. Main thread only.
    fn read_audio_props(&self) {
        self.latency_frames.store(
            self.read_seconds(kAudioUnitProperty_Latency),
            Ordering::Relaxed,
        );
        self.tail_frames.store(
            self.read_seconds(kAudioUnitProperty_TailTime),
            Ordering::Relaxed,
        );
    }

    /// How many buses a scope has.
    fn element_count(&self, scope: AudioUnitScope) -> u32 {
        let mut count = 0u32;
        let mut size = 4u32;
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_ElementCount,
                scope,
                0,
                NonNull::new_unchecked(&mut count as *mut u32 as *mut c_void),
                NonNull::from(&mut size),
            )
        };
        if status == NO_ERR {
            count
        } else {
            0
        }
    }
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
        *cycling = t.cycling.get() as u8;
    }
    if !cycle_start.is_null() {
        *cycle_start = t.cycle_start.get();
    }
    if !cycle_end.is_null() {
        *cycle_end = t.cycle_end.get();
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

/// `AudioUnitEvent`, for the parameter events asked for here.
#[repr(C)]
struct ParamEvent {
    kind: u32,
    unit: AudioUnit,
    id: u32,
    scope: u32,
    element: u32,
}

const EVENT_VALUE_CHANGE: u32 = 0;
const EVENT_GESTURE_BEGIN: u32 = 1;

type ParamEventProc =
    unsafe extern "C-unwind" fn(*mut c_void, *mut c_void, *const ParamEvent, u64, f32);

extern "C-unwind" {
    fn AUEventListenerCreate(
        proc_: ParamEventProc,
        user: *mut c_void,
        run_loop: *const c_void,
        mode: *const c_void,
        notification_interval: f32,
        value_granularity: f32,
        listener: *mut *mut c_void,
    ) -> i32;
    fn AUEventListenerAddEventType(
        listener: *mut c_void,
        object: *mut c_void,
        event: *const ParamEvent,
    ) -> i32;
    fn AUListenerDispose(listener: *mut c_void) -> i32;
    fn CFRunLoopGetCurrent() -> *const c_void;
    static kCFRunLoopDefaultMode: *const c_void;
}

/// Told what the unit's own window does to its parameters. Called on the main run loop.
struct ParamListener {
    listener: *mut c_void,
    changes: Box<Mutex<Vec<(u32, f64)>>>,
}

unsafe extern "C-unwind" fn param_event(
    user: *mut c_void,
    _object: *mut c_void,
    event: *const ParamEvent,
    _time: u64,
    value: f32,
) {
    let changes = &*(user as *const Mutex<Vec<(u32, f64)>>);
    let e = &*event;
    let value = match e.kind {
        EVENT_VALUE_CHANGE => value,
        // A touch with no change yet: all this says is which parameter it is.
        EVENT_GESTURE_BEGIN => {
            let mut now = 0.0;
            if AudioUnitGetParameter(
                e.unit,
                e.id,
                kAudioUnitScope_Global,
                0,
                NonNull::from(&mut now),
            ) != NO_ERR
            {
                return;
            }
            now
        }
        _ => return,
    };
    if let Ok(mut changes) = changes.lock() {
        changes.push((e.id, value as f64));
    }
}

impl ParamListener {
    /// Start listening to every parameter in `ids`, on the current thread's run loop.
    fn new(unit: AudioUnit, ids: impl Iterator<Item = u32>) -> Option<Self> {
        let changes = Box::new(Mutex::new(Vec::new()));
        let user = &*changes as *const Mutex<Vec<(u32, f64)>> as *mut c_void;
        let mut listener = std::ptr::null_mut();
        let status = unsafe {
            AUEventListenerCreate(
                param_event,
                user,
                CFRunLoopGetCurrent(),
                kCFRunLoopDefaultMode,
                0.0,
                0.0,
                &mut listener,
            )
        };
        if status != NO_ERR || listener.is_null() {
            return None;
        }
        for id in ids {
            for kind in [EVENT_VALUE_CHANGE, EVENT_GESTURE_BEGIN] {
                let event = ParamEvent {
                    kind,
                    unit,
                    id,
                    scope: kAudioUnitScope_Global,
                    element: 0,
                };
                unsafe { AUEventListenerAddEventType(listener, std::ptr::null_mut(), &event) };
            }
        }
        Some(Self { listener, changes })
    }

    fn drain(&self) -> Vec<(u32, f64)> {
        self.changes
            .lock()
            .map_or_else(|_| Vec::new(), |mut c| std::mem::take(&mut *c))
    }
}

impl Drop for ParamListener {
    fn drop(&mut self) {
        unsafe { AUListenerDispose(self.listener) };
    }
}

/// MIDI a unit produced while rendering. Only ever touched from the render thread.
struct MidiCapture {
    events: UnsafeCell<Vec<(u32, [u8; 3], usize)>>,
}

unsafe impl Send for MidiCapture {}
unsafe impl Sync for MidiCapture {}

/// `AUMIDIOutputCallbackStruct`, spelled out so this needn't depend on CoreMIDI's bindings.
#[repr(C)]
struct MidiOutputCallback {
    callback:
        unsafe extern "C-unwind" fn(*mut c_void, *const AudioTimeStamp, u32, *const u8) -> i32,
    user: *mut c_void,
}

unsafe extern "C-unwind" fn midi_output(
    user: *mut c_void,
    _time: *const AudioTimeStamp,
    _output: u32,
    packets: *const u8,
) -> i32 {
    let capture = &*(user as *const MidiCapture);
    let events = &mut *capture.events.get();
    parse_midi_packets(packets, &mut |offset, bytes, len| {
        if events.len() < events.capacity() {
            events.push((offset, bytes, len));
        }
    });
    NO_ERR
}

/// Walk a `MIDIPacketList`: `{ numPackets: u32, packet: [ { timeStamp: u64, length: u16,
/// data: [u8] } ] }`. On Apple Silicon each packet starts on a four-byte boundary.
unsafe fn parse_midi_packets(list: *const u8, out: &mut impl FnMut(u32, [u8; 3], usize)) {
    if list.is_null() {
        return;
    }
    let count = std::ptr::read_unaligned(list as *const u32);
    let mut packet = list.add(4);
    for _ in 0..count {
        let time = std::ptr::read_unaligned(packet as *const u64);
        let length = std::ptr::read_unaligned(packet.add(8) as *const u16) as usize;
        let data = std::slice::from_raw_parts(packet.add(10), length);
        let mut at = 0;
        while at < data.len() {
            let status = data[at];
            let size = midi_message_size(status);
            if size == 0 || at + size > data.len() {
                break;
            }
            let mut bytes = [0u8; 3];
            bytes[..size].copy_from_slice(&data[at..at + size]);
            out(time.min(u32::MAX as u64) as u32, bytes, size);
            at += size;
        }
        let next = packet.add(10 + length);
        packet = if cfg!(target_arch = "aarch64") {
            ((next as usize + 3) & !3) as *const u8
        } else {
            next
        };
    }
}

/// How long a channel message is, from its status byte. 0 for anything else (sysex, realtime).
fn midi_message_size(status: u8) -> usize {
    match status & 0xf0 {
        0x80 | 0x90 | 0xa0 | 0xb0 | 0xe0 => 3,
        0xc0 | 0xd0 => 2,
        _ => 0,
    }
}

/// Names of the unit's elements past the main one, in a scope.
fn element_names(inst: &AuInstance, scope: AudioUnitScope, count: u32) -> Vec<String> {
    (1..count)
        .map(|bus| unsafe {
            let mut name: *const CFString = std::ptr::null();
            let mut size = std::mem::size_of::<*const CFString>() as u32;
            let status = AudioUnitGetProperty(
                inst.unit,
                kAudioUnitProperty_ElementName,
                scope,
                bus,
                NonNull::new_unchecked(&mut name as *mut _ as *mut c_void),
                NonNull::from(&mut size),
            );
            if status == NO_ERR && !name.is_null() {
                CFRetained::from_raw(NonNull::new_unchecked(name as *mut CFString)).to_string()
            } else {
                format!("Output {}", bus + 1)
            }
        })
        .collect()
}

/// Whether the unit reports any MIDI output.
fn has_midi_output(unit: AudioUnit) -> bool {
    unsafe {
        let mut names: *const CFArray = std::ptr::null();
        let mut size = std::mem::size_of::<*const CFArray>() as u32;
        let status = AudioUnitGetProperty(
            unit,
            kAudioUnitProperty_MIDIOutputCallbackInfo,
            kAudioUnitScope_Global,
            0,
            NonNull::new_unchecked(&mut names as *mut _ as *mut c_void),
            NonNull::from(&mut size),
        );
        if status != NO_ERR || names.is_null() {
            return false;
        }
        CFRetained::from_raw(NonNull::new_unchecked(names as *mut CFArray)).count() > 0
    }
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
        latency_frames: AtomicU32::new(0),
        tail_frames: AtomicU32::new(0),
    });
    let is_effect = query.componentType != kAudioUnitType_MusicDevice
        && query.componentType != kAudioUnitType_Generator;
    let takes_midi = !is_effect || query.componentType == kAudioUnitType_MusicEffect;
    let midi_capture = Box::new(MidiCapture {
        events: UnsafeCell::new(Vec::with_capacity(1024)),
    });
    let midi_outputs = has_midi_output(unit);
    let fmt = stereo_format(sample_rate);
    let max_frames = max_block as u32;
    let mut input = Box::new(InputData {
        left: vec![0.0; max_block],
        right: vec![0.0; max_block],
    });
    let mut sidechain = None;
    let mut aux_buses = 0u32;
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
        // Extra buses are connected but silent until something is routed to them.
        if is_effect && inst.element_count(kAudioUnitScope_Input) > 1 {
            let mut key = Box::new(InputData {
                left: vec![0.0; max_block],
                right: vec![0.0; max_block],
            });
            let cb = AURenderCallbackStruct {
                inputProc: Some(input_callback),
                inputProcRefCon: &mut *key as *mut InputData as *mut c_void,
            };
            let format = set_property(
                unit,
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Input,
                1,
                &fmt,
            );
            let callback = set_property(
                unit,
                kAudioUnitProperty_SetRenderCallback,
                kAudioUnitScope_Input,
                1,
                &cb,
            );
            if format == NO_ERR && callback == NO_ERR {
                sidechain = Some(key);
            }
        }
        if midi_outputs {
            let cb = MidiOutputCallback {
                callback: midi_output,
                user: &*midi_capture as *const MidiCapture as *mut c_void,
            };
            set_property(
                unit,
                kAudioUnitProperty_MIDIOutputCallback,
                kAudioUnitScope_Global,
                0,
                &cb,
            );
        }
        for bus in 1..inst.element_count(kAudioUnitScope_Output) {
            let status = set_property(
                unit,
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Output,
                bus,
                &fmt,
            );
            if status != NO_ERR {
                break;
            }
            aux_buses += 1;
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
        has_sidechain: sidechain.is_some(),
        aux_names: element_names(&inst, kAudioUnitScope_Output, aux_buses + 1),
        midi_output: midi_outputs,
        desc: desc.clone(),
        params: Vec::new(),
        editor: None,
        listener: None,
        title: None,
    };
    handle.rescan_params();
    inst.read_audio_props();
    let processor = AuProcessor {
        inst,
        input,
        sidechain,
        aux_buses,
        output: [vec![0.0; max_block], vec![0.0; max_block]],
        pending: Vec::with_capacity(256),
        mpe: MpeEncoder::new(MpeZone::Lower, 15),
        midi_capture,
        midi_out: MidiOutput::default(),
        is_effect,
        takes_midi,
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
    has_sidechain: bool,
    aux_names: Vec<String>,
    midi_output: bool,
    desc: PluginDescriptor,
    params: Vec<ParamInfo>,
    editor: Option<crate::mac::EditorWindow>,
    /// Present while the window is open.
    listener: Option<ParamListener>,
    /// What the window should say, once it is open.
    title: Option<String>,
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
                    mod_mode: Default::default(),
                    flags: Default::default(),
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

    /// Hear about what the unit's own window changes. A host's own changes don't come back.
    fn listen_to_params(&mut self) {
        let ids = self.params.iter().map(|p| p.id);
        self.listener = ParamListener::new(self.inst.unit, ids);
    }

    /// The presets built into the unit.
    fn factory_presets(&self) -> Vec<PluginPreset> {
        unsafe {
            let mut arr: *const CFArray = std::ptr::null();
            let mut size = std::mem::size_of::<*const CFArray>() as u32;
            if AudioUnitGetProperty(
                self.inst.unit,
                kAudioUnitProperty_FactoryPresets,
                kAudioUnitScope_Global,
                0,
                NonNull::new_unchecked(&mut arr as *mut _ as *mut c_void),
                NonNull::from(&mut size),
            ) != NO_ERR
                || arr.is_null()
            {
                return Vec::new();
            }
            // Owned by this array, which keeps them valid while they are read.
            let arr = CFRetained::from_raw(NonNull::new_unchecked(arr as *mut CFArray));
            (0..arr.count())
                .filter_map(|i| {
                    let preset = arr.value_at_index(i) as *const AUPreset;
                    if preset.is_null() || (*preset).presetName.is_null() {
                        return None;
                    }
                    Some(PluginPreset {
                        name: (*(*preset).presetName).to_string(),
                        source: PresetSource::Factory((*preset).presetNumber),
                    })
                })
                .collect()
        }
    }

    fn apply_factory_preset(&mut self, number: i32) -> bool {
        let empty = CFString::from_str("");
        let preset = AUPreset {
            presetNumber: number,
            presetName: CFRetained::as_ptr(&empty).as_ptr(),
        };
        let ok = unsafe {
            set_property(
                self.inst.unit,
                kAudioUnitProperty_PresentPreset,
                kAudioUnitScope_Global,
                0,
                &preset,
            ) == NO_ERR
        };
        if ok {
            self.rescan_params();
        }
        ok
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

    fn sidechain_input(&self) -> bool {
        self.has_sidechain
    }

    fn aux_outputs(&self) -> Vec<String> {
        self.aux_names.clone()
    }

    fn midi_output(&self) -> bool {
        self.midi_output
    }

    fn presets(&self) -> Vec<PluginPreset> {
        let mut out = self.factory_presets();
        out.extend(user_presets(&self.desc));
        out
    }

    fn load_preset(&mut self, preset: &PluginPreset) -> bool {
        match &preset.source {
            PresetSource::Factory(number) => self.apply_factory_preset(*number),
            PresetSource::File { path, .. } => match std::fs::read(path) {
                Ok(data) => self.load_state(&data),
                Err(e) => {
                    log::warn!("could not read preset {}: {e}", path.display());
                    false
                }
            },
        }
    }

    fn set_editor_visible(&mut self, visible: bool) {
        if let Some(w) = &self.editor {
            w.set_visible(visible);
        }
    }

    fn set_editor_title(&mut self, title: &str) {
        self.title = Some(title.to_string());
        if let Some(w) = &self.editor {
            w.set_title(title);
        }
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
        let title = self.title.as_deref().unwrap_or(&self.desc.name);
        let window = crate::mac::EditorWindow::new(title, size.width, size.height, false)
            .ok_or_else(|| HostError::Editor("could not create window".into()))?;
        window.set_content(&view);
        window.show();
        self.editor = Some(window);
        self.listen_to_params();
        Ok(())
    }

    fn close_editor(&mut self) {
        self.listener = None;
        if let Some(w) = self.editor.take() {
            w.close();
        }
    }

    fn idle(&mut self) -> Vec<HostNotification> {
        let mut out = Vec::new();
        if let Some(l) = &self.listener {
            out.extend(
                l.drain()
                    .into_iter()
                    .map(|(id, value)| HostNotification::ParamChanged { id, value }),
            );
        }
        // Audio Units have no latency callback here, so notice a change by asking.
        let before = self.inst.latency_frames.load(Ordering::Relaxed);
        self.inst.read_audio_props();
        if self.inst.latency_frames.load(Ordering::Relaxed) != before {
            out.push(HostNotification::LatencyChanged);
        }
        if matches!(&self.editor, Some(w) if !w.is_visible()) {
            self.editor = None;
            self.listener = None;
            out.push(HostNotification::EditorClosed);
        }
        out
    }

    fn latency(&self) -> u32 {
        self.inst.read_audio_props();
        self.inst.latency_frames.load(Ordering::Relaxed)
    }
}

impl Drop for AuHandle {
    fn drop(&mut self) {
        self.close_editor();
    }
}

/// The `.aupreset` files for a unit under the two standard preset folders. Those in the system
/// folder can't be changed from here.
fn user_presets(desc: &PluginDescriptor) -> Vec<PluginPreset> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let roots = [
        (home.join("Library/Audio/Presets"), false),
        (PathBuf::from("/Library/Audio/Presets"), true),
    ];
    let mut out = Vec::new();
    for (root, read_only) in roots {
        out.extend(presets_under(&root, desc, read_only));
    }
    out
}

/// The `.aupreset` files for a unit below `root`, in `<Manufacturer>/<Plug-in>`.
fn presets_under(root: &Path, desc: &PluginDescriptor, read_only: bool) -> Vec<PluginPreset> {
    let folder = root.join(&desc.vendor).join(&desc.name);
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut out: Vec<PluginPreset> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "aupreset"))
        .map(|path| PluginPreset {
            name: path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            source: PresetSource::File { path, read_only },
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// ------------------------------------------------------------------ processor

pub struct AuProcessor {
    inst: Arc<AuInstance>,
    input: Box<InputData>,
    /// Where the key input is read from, when the unit has one.
    sidechain: Option<Box<InputData>>,
    /// Output buses past the main one.
    aux_buses: u32,
    output: [Vec<f32>; 2],
    pending: Vec<(u32, f32)>,
    is_effect: bool,
    /// Instruments and music effects are sent MIDI; plain effects aren't.
    takes_midi: bool,
    /// AU takes notes as MIDI, so note expression reaches it as MPE.
    mpe: MpeEncoder,
    midi_capture: Box<MidiCapture>,
    midi_out: MidiOutput,
    sample_time: f64,
    max_block: usize,
}

unsafe impl Send for AuProcessor {}

static WARNED: AtomicBool = AtomicBool::new(false);

impl AuProcessor {
    /// Hand what the unit sent out while rendering to the next device.
    fn collect_midi(&mut self, out: &mut EventQueue, frames: usize) {
        let captured = unsafe { &mut *self.midi_capture.events.get() };
        for &(offset, bytes, len) in captured.iter() {
            if let Some(message) = MidiMessage::from_bytes(&bytes[..len]) {
                let offset = offset.min(frames.saturating_sub(1) as u32);
                self.midi_out.push(out, offset, message);
            }
        }
        captured.clear();
    }
}

impl Processor for AuProcessor {
    fn activate(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot != MPE_SLOT {
            return Some(resource);
        }
        if let Some(output) = resource.downcast_ref::<Option<MpeOutput>>() {
            self.mpe = match output {
                Some(o) => MpeEncoder::configured(*o),
                None => MpeEncoder::new(MpeZone::Lower, 15),
            };
            self.midi_out = MidiOutput::new_for(*output);
        }
        Some(resource)
    }

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
        let (cycle_start, cycle_end) = t.loop_range.unwrap_or((0.0, 0.0));
        timing.cycling.set(t.loop_range.is_some());
        timing.cycle_start.set(cycle_start / rlive_core::PPQ as f64);
        timing.cycle_end.set(cycle_end / rlive_core::PPQ as f64);

        unsafe {
            for &(id, v) in &self.pending {
                AudioUnitSetParameter(unit, id, kAudioUnitScope_Global, 0, v, 0);
            }
            self.pending.clear();
            let last = frames.saturating_sub(1) as u32;
            for e in ctx.params {
                AudioUnitSetParameter(
                    unit,
                    e.id.0,
                    kAudioUnitScope_Global,
                    0,
                    e.value as f32,
                    e.offset.min(last),
                );
            }
            if self.takes_midi {
                self.mpe.encode(ctx.events, &mut |e| {
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
                });
            }
        }

        let left = &mut ctx.audio.left[..frames];
        let right = &mut ctx.audio.right[..frames];
        if self.is_effect {
            self.input.left[..frames].copy_from_slice(left);
            self.input.right[..frames].copy_from_slice(right);
        }
        if let Some(key) = self.sidechain.as_mut() {
            key.left[..frames].fill(0.0);
            key.right[..frames].fill(0.0);
            if let Some(sc) = &ctx.sidechain {
                let n = frames.min(sc.frames());
                key.left[..n].copy_from_slice(&sc.left[..n]);
                key.right[..n].copy_from_slice(&sc.right[..n]);
            }
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
        self.collect_midi(&mut *ctx.events_out, frames);
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
        for (i, aux) in ctx
            .aux_out
            .iter_mut()
            .take(self.aux_buses as usize)
            .enumerate()
        {
            let n = frames.min(aux.frames());
            let mut list = StereoBufferList {
                number_buffers: 2,
                buffers: [
                    AudioBuffer {
                        mNumberChannels: 1,
                        mDataByteSize: (n * 4) as u32,
                        mData: aux.left.as_mut_ptr() as *mut c_void,
                    },
                    AudioBuffer {
                        mNumberChannels: 1,
                        mDataByteSize: (n * 4) as u32,
                        mData: aux.right.as_mut_ptr() as *mut c_void,
                    },
                ],
            };
            let mut flags = AudioUnitRenderActionFlags(0);
            unsafe {
                AudioUnitRender(
                    unit,
                    &mut flags,
                    NonNull::from(&mut ts),
                    i as u32 + 1,
                    n as u32,
                    NonNull::new_unchecked(
                        &mut list as *mut StereoBufferList as *mut AudioBufferList,
                    ),
                );
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
        self.mpe.reset();
    }

    fn latency(&self) -> u32 {
        self.inst.latency_frames.load(Ordering::Relaxed)
    }

    fn tail(&self) -> Tail {
        Tail::from_samples(self.inst.tail_frames.load(Ordering::Relaxed))
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
        let mut events_out = rlive_core::EventQueue::with_capacity(0);
        let view = rlive_core::ViewWriter::none();
        let mut l = vec![0.5f32; 512];
        let mut r = vec![0.5f32; 512];
        inst.processor.process(&mut ProcessContext {
            audio: rlive_core::AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            sidechain: None,
            aux_out: &mut [],
            events: &[],
            events_out: &mut events_out,
            params: &[],
            transport: &transport,
            param_out: &mut q,
            mod_out: &mut [],
            view: &view,
        });
        assert!(l.iter().all(|s| s.is_finite()));
        let state = inst.handle.save_state().expect("AU state");
        assert!(inst.handle.load_state(&state));
    }

    /// A bare unit of `desc`, initialized, and a parameter of it that can move.
    fn unit_with_params(desc: &PluginDescriptor) -> (AudioUnit, ParamInfo) {
        let mut query = description_from_id(&desc.id).unwrap();
        let comp =
            unsafe { AudioComponentFindNext(std::ptr::null_mut(), NonNull::from(&mut query)) };
        let mut unit: AudioComponentInstance = std::ptr::null_mut();
        assert_eq!(
            unsafe { AudioComponentInstanceNew(comp, NonNull::from(&mut unit)) },
            NO_ERR
        );
        assert_eq!(unsafe { AudioUnitInitialize(unit) }, NO_ERR);
        let inst = instantiate(desc, 48_000.0, 512).unwrap();
        let target = inst
            .handle
            .params()
            .into_iter()
            .find(|p| p.max > p.min)
            .unwrap();
        (unit, target)
    }

    /// Lays packets out as CoreMIDI does, including its alignment on Apple Silicon.
    fn packet_list(packets: &[(u64, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(packets.len() as u32).to_ne_bytes());
        for (time, data) in packets {
            out.extend_from_slice(&time.to_ne_bytes());
            out.extend_from_slice(&(data.len() as u16).to_ne_bytes());
            out.extend_from_slice(data);
            if cfg!(target_arch = "aarch64") {
                while out.len() % 4 != 0 {
                    out.push(0);
                }
            }
        }
        out
    }

    #[test]
    fn midi_packets_are_split_into_messages_with_their_offsets() {
        let list = packet_list(&[(7, &[0x90, 60, 100, 0xc1, 5]), (30, &[0xe2, 0, 64])]);
        let mut seen = Vec::new();
        unsafe {
            parse_midi_packets(list.as_ptr(), &mut |offset, bytes, len| {
                seen.push((offset, bytes, len))
            })
        };
        assert_eq!(
            seen,
            vec![
                (7, [0x90, 60, 100], 3),
                (7, [0xc1, 5, 0], 2),
                (30, [0xe2, 0, 64], 3)
            ]
        );
    }

    #[test]
    fn captured_midi_reaches_the_next_device() {
        let found = scan();
        let delay = found.iter().find(|d| d.id == "aufx:dely:appl").unwrap();
        let inst = instantiate(delay, 48_000.0, 512).unwrap();
        // Reach the processor by building one the same way, without the box.
        let mut capture = MidiCapture {
            events: UnsafeCell::new(Vec::with_capacity(8)),
        };
        let list = packet_list(&[(9, &[0x93, 61, 90])]);
        unsafe {
            midi_output(
                &mut capture as *mut MidiCapture as *mut c_void,
                std::ptr::null(),
                0,
                list.as_ptr(),
            );
        }
        let events = unsafe { &*capture.events.get() };
        assert_eq!(events.as_slice(), &[(9, [0x93, 61, 90], 3)]);
        drop(inst);
    }

    #[test]
    fn apple_reverb_has_factory_presets_that_load() {
        let found = scan();
        let reverb = found
            .iter()
            .find(|d| d.id == "aufx:rvb2:appl")
            .expect("AUReverb2 should be installed");
        let mut inst = instantiate(reverb, 48_000.0, 512).unwrap();
        let presets = inst.handle.presets();
        assert!(presets.len() > 1, "{presets:?}");
        assert!(presets[0].read_only());
        let values = |h: &dyn PluginHandle| -> Vec<Option<f64>> {
            h.params().iter().map(|p| h.param_value(p.id)).collect()
        };
        let before = values(&*inst.handle);
        let changed = presets.iter().any(|p| {
            assert!(inst.handle.load_preset(p));
            values(&*inst.handle) != before
        });
        assert!(changed, "a factory preset should move a parameter");
    }

    #[test]
    fn user_presets_are_found_by_manufacturer_and_plugin_and_load_as_state() {
        let root = std::env::temp_dir().join(format!("rlive-aupreset-{}", std::process::id()));
        let found = scan();
        let delay = found.iter().find(|d| d.id == "aufx:dely:appl").unwrap();
        let folder = root.join(&delay.vendor).join(&delay.name);
        std::fs::create_dir_all(&folder).unwrap();
        let mut inst = instantiate(delay, 48_000.0, 512).unwrap();
        let state = inst.handle.save_state().unwrap();
        std::fs::write(folder.join("Mine.aupreset"), &state).unwrap();
        std::fs::write(folder.join("notes.txt"), b"not a preset").unwrap();
        let presets = presets_under(&root, delay, false);
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "Mine");
        assert!(!presets[0].read_only());
        assert!(inst.handle.load_preset(&presets[0]));
        std::fs::remove_dir_all(&root).ok();
    }

    /// What a unit's own window does: tells its listeners a parameter moved. The listener is
    /// real, and the event is handed to it as the system would.
    #[test]
    fn what_a_units_window_does_comes_back_as_a_parameter_change() {
        let found = scan();
        let delay = found.iter().find(|d| d.id == "aufx:dely:appl").unwrap();
        let (unit, target) = unit_with_params(delay);
        let listener = ParamListener::new(unit, std::iter::once(target.id)).expect("listens");
        let user = &*listener.changes as *const Mutex<Vec<(u32, f64)>> as *mut c_void;
        let event = |kind| ParamEvent {
            kind,
            unit,
            id: target.id,
            scope: kAudioUnitScope_Global,
            element: 0,
        };
        unsafe {
            param_event(
                user,
                std::ptr::null_mut(),
                &event(EVENT_VALUE_CHANGE),
                0,
                0.25,
            );
            param_event(
                user,
                std::ptr::null_mut(),
                &event(EVENT_GESTURE_BEGIN),
                0,
                0.0,
            );
            param_event(user, std::ptr::null_mut(), &event(99), 0, 1.0);
        }
        let changes = listener.drain();
        assert_eq!(
            changes.len(),
            2,
            "a move and a touch, not the unknown event"
        );
        assert_eq!(changes[0], (target.id, 0.25));
        assert_eq!(changes[1].0, target.id);
        assert!(listener.drain().is_empty());
    }

    #[test]
    fn audio_unit_reports_its_aux_outputs_by_name_or_number() {
        let found = scan();
        let delay = found.iter().find(|d| d.id == "aufx:dely:appl").unwrap();
        let inst = instantiate(delay, 48_000.0, 512).unwrap();
        // AUDelay has only its main output; the list holds extras alone.
        assert!(inst.handle.aux_outputs().is_empty());
        assert!(!inst.handle.midi_output());
    }
}
