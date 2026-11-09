//! VST3 plugin hosting (via the MIT-licensed `vst3` bindings).

use crate::midi_out::MidiOutput;
use crate::{
    bundle_binary, cbuf_to_string, wire_note_id, HostError, HostNotification, PluginDescriptor,
    PluginHandle, PluginInstance, PortBuffers, Result,
};
use rlive_core::{
    EventQueue, ExprKind, MidiEvent, MidiMessage, MpeEncoder, MpeOutput, NoteId, ParamFlags,
    ParamInfo, ParamKind, PluginFormat, ProcessContext, Processor, Resource, Tail, MPE_SLOT,
};
use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

type AttrID = *const c_char;

#[cfg(target_arch = "aarch64")]
const LINUX_ARCH_DIR: &str = "aarch64-linux";
#[cfg(not(target_arch = "aarch64"))]
const LINUX_ARCH_DIR: &str = "x86_64-linux";

// ------------------------------------------------------------------ helpers

fn tuid_to_hex(tuid: &TUID) -> String {
    tuid.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

fn hex_to_tuid(s: &str) -> Option<TUID> {
    if s.len() != 32 {
        return None;
    }
    let mut t: TUID = [0; 16];
    for (i, item) in t.iter_mut().enumerate() {
        *item = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()? as c_char;
    }
    Some(t)
}

fn string128_to_string(s: &String128) -> String {
    let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len])
}

fn copy_to_string128(src: &str, dst: &mut String128) {
    let mut n = 0;
    for (d, c) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = c;
        n += 1;
    }
    dst[n.min(127)] = 0;
}

fn iid_ptr<I: Interface>() -> FIDString {
    I::IID.as_ptr() as FIDString
}

// ------------------------------------------------------------------ library

struct VstLibrary {
    /// `None` only while dropping.
    factory_ptr: Option<ComPtr<IPluginFactory>>,
    exit: Option<unsafe extern "system" fn() -> bool>,
    #[cfg(target_os = "macos")]
    _bundle: Option<objc2_core_foundation::CFRetained<objc2_core_foundation::CFBundle>>,
    _lib: libloading::Library,
}

impl VstLibrary {
    fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory_ptr.as_ref().expect("factory alive")
    }
}

unsafe impl Send for VstLibrary {}
unsafe impl Sync for VstLibrary {}

impl Drop for VstLibrary {
    fn drop(&mut self) {
        // Release the factory before the module exits.
        self.factory_ptr = None;
        if let Some(exit) = self.exit {
            unsafe { exit() };
        }
    }
}

type LibraryCache = Mutex<Vec<(PathBuf, Weak<VstLibrary>)>>;
static LIBRARIES: LibraryCache = Mutex::new(Vec::new());

fn load_library(path: &Path) -> Result<Arc<VstLibrary>> {
    let mut cache = LIBRARIES.lock().unwrap();
    cache.retain(|(_, w)| w.strong_count() > 0);
    if let Some(lib) = cache
        .iter()
        .find(|(p, _)| p == path)
        .and_then(|(_, w)| w.upgrade())
    {
        return Ok(lib);
    }
    let binary = bundle_binary(path, LINUX_ARCH_DIR);
    let lib = unsafe { libloading::Library::new(&binary) }
        .map_err(|e| HostError::Load(binary.clone(), e.to_string()))?;

    type EntryFn = unsafe extern "system" fn(*mut c_void) -> bool;
    type ExitFn = unsafe extern "system" fn() -> bool;
    type FactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;

    let sym = |names: &[&[u8]]| -> Option<*const c_void> {
        names
            .iter()
            .find_map(|n| unsafe { lib.get::<*const c_void>(*n).ok().map(|s| *s) })
    };

    #[cfg(target_os = "macos")]
    let bundle = {
        use objc2_core_foundation::{CFBundle, CFString, CFURLPathStyle, CFURL};
        let url = CFURL::with_file_system_path(
            None,
            Some(&CFString::from_str(&path.to_string_lossy())),
            CFURLPathStyle::CFURLPOSIXPathStyle,
            true,
        );
        url.and_then(|u| CFBundle::new(None, Some(&u)))
    };

    let (entry, exit): (Option<*const c_void>, Option<*const c_void>) = if cfg!(target_os = "macos")
    {
        (
            sym(&[b"bundleEntry\0", b"BundleEntry\0"]),
            sym(&[b"bundleExit\0", b"BundleExit\0"]),
        )
    } else if cfg!(target_os = "windows") {
        (sym(&[b"InitDll\0"]), sym(&[b"ExitDll\0"]))
    } else {
        (sym(&[b"ModuleEntry\0"]), sym(&[b"ModuleExit\0"]))
    };

    if let Some(entry) = entry {
        #[cfg(target_os = "macos")]
        let arg = bundle.as_ref().map_or(std::ptr::null_mut(), |b| {
            objc2_core_foundation::CFRetained::as_ptr(b).as_ptr() as *mut c_void
        });
        #[cfg(not(target_os = "macos"))]
        let arg = std::ptr::null_mut();
        let ok = unsafe {
            if cfg!(target_os = "windows") {
                let f: ExitFn = std::mem::transmute(entry);
                f()
            } else {
                let f: EntryFn = std::mem::transmute(entry);
                f(arg)
            }
        };
        if !ok {
            return Err(HostError::Init("module entry returned false".into()));
        }
    }
    let get_factory: FactoryFn = match sym(&[b"GetPluginFactory\0"]) {
        Some(f) => unsafe { std::mem::transmute::<*const c_void, FactoryFn>(f) },
        None => return Err(HostError::NoEntry("GetPluginFactory".into())),
    };
    let factory = unsafe { ComPtr::from_raw(get_factory()) }
        .ok_or_else(|| HostError::Init("GetPluginFactory returned null".into()))?;
    let lib = Arc::new(VstLibrary {
        factory_ptr: Some(factory),
        exit: exit.map(|e| unsafe { std::mem::transmute::<*const c_void, ExitFn>(e) }),
        #[cfg(target_os = "macos")]
        _bundle: bundle,
        _lib: lib,
    });
    cache.push((path.to_path_buf(), Arc::downgrade(&lib)));
    Ok(lib)
}

/// List the audio processor classes in a `.vst3` bundle.
pub fn scan(path: &Path) -> Result<Vec<PluginDescriptor>> {
    let lib = load_library(path)?;
    let f = lib.factory();
    let mut out = Vec::new();
    unsafe {
        let mut finfo: PFactoryInfo = std::mem::zeroed();
        f.getFactoryInfo(&mut finfo);
        let factory_vendor = cbuf_to_string(&finfo.vendor);
        let f2 = f.cast::<IPluginFactory2>();
        for i in 0..f.countClasses() {
            let mut info: PClassInfo = std::mem::zeroed();
            if f.getClassInfo(i, &mut info) != kResultOk {
                continue;
            }
            if cbuf_to_string(&info.category) != AUDIO_MODULE_CLASS {
                continue;
            }
            let (mut vendor, mut version, mut sub) =
                (factory_vendor.clone(), String::new(), String::new());
            if let Some(f2) = &f2 {
                let mut info2: PClassInfo2 = std::mem::zeroed();
                if f2.getClassInfo2(i, &mut info2) == kResultOk {
                    let v = cbuf_to_string(&info2.vendor);
                    if !v.is_empty() {
                        vendor = v;
                    }
                    version = cbuf_to_string(&info2.version);
                    sub = cbuf_to_string(&info2.subCategories);
                }
            }
            out.push(PluginDescriptor {
                format: PluginFormat::Vst3,
                id: tuid_to_hex(&info.cid),
                name: cbuf_to_string(&info.name),
                vendor,
                version,
                path: path.to_path_buf(),
                is_instrument: sub.split('|').any(|s| s == "Instrument"),
                subcategories: sub,
                is_midi_effect: false,
            });
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ host COM objects

/// In-memory `IBStream`.
#[derive(Default)]
struct MemoryStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, num_read: *mut int32) -> tresult {
        let data = self.data.borrow();
        let pos = self.pos.get();
        let n = (num_bytes.max(0) as usize).min(data.len().saturating_sub(pos));
        std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer as *mut u8, n);
        self.pos.set(pos + n);
        if !num_read.is_null() {
            *num_read = n as int32;
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_written: *mut int32,
    ) -> tresult {
        let mut data = self.data.borrow_mut();
        let pos = self.pos.get();
        let n = num_bytes.max(0) as usize;
        if data.len() < pos + n {
            data.resize(pos + n, 0);
        }
        std::ptr::copy_nonoverlapping(buffer as *const u8, data.as_mut_ptr().add(pos), n);
        self.pos.set(pos + n);
        if !num_written.is_null() {
            *num_written = n as int32;
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let base = match mode as u32 {
            m if m == IBStream_::IStreamSeekMode_::kIBSeekSet => 0,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekCur => self.pos.get() as i64,
            _ => len,
        };
        let new = (base + pos).clamp(0, len);
        self.pos.set(new as usize);
        if !result.is_null() {
            *result = new;
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if !pos.is_null() {
            *pos = self.pos.get() as i64;
        }
        kResultOk
    }
}

fn new_stream(data: Vec<u8>) -> ComWrapper<MemoryStream> {
    ComWrapper::new(MemoryStream {
        data: RefCell::new(data),
        pos: Cell::new(0),
    })
}

fn stream_ptr(s: &ComWrapper<MemoryStream>) -> *mut IBStream {
    s.as_com_ref::<IBStream>()
        .map_or(std::ptr::null_mut(), |r| r.as_ptr())
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

#[derive(Default)]
struct HostAttributes {
    map: RefCell<HashMap<String, Attr>>,
}

impl Class for HostAttributes {
    type Interfaces = (IAttributeList,);
}

unsafe fn attr_key(id: AttrID) -> Option<String> {
    (!id.is_null()).then(|| CStr::from_ptr(id).to_string_lossy().into_owned())
}

impl IAttributeListTrait for HostAttributes {
    unsafe fn setInt(&self, id: AttrID, value: int64) -> tresult {
        let Some(k) = attr_key(id) else {
            return kInvalidArgument;
        };
        self.map.borrow_mut().insert(k, Attr::Int(value));
        kResultOk
    }
    unsafe fn getInt(&self, id: AttrID, value: *mut int64) -> tresult {
        match attr_key(id).and_then(|k| match self.map.borrow().get(&k) {
            Some(Attr::Int(v)) => Some(*v),
            _ => None,
        }) {
            Some(v) => {
                *value = v;
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        let Some(k) = attr_key(id) else {
            return kInvalidArgument;
        };
        self.map.borrow_mut().insert(k, Attr::Float(value));
        kResultOk
    }
    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        match attr_key(id).and_then(|k| match self.map.borrow().get(&k) {
            Some(Attr::Float(v)) => Some(*v),
            _ => None,
        }) {
            Some(v) => {
                *value = v;
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        let Some(k) = attr_key(id) else {
            return kInvalidArgument;
        };
        let mut v = Vec::new();
        let mut p = string;
        while !p.is_null() && *p != 0 {
            v.push(*p);
            p = p.add(1);
        }
        self.map.borrow_mut().insert(k, Attr::Str(v));
        kResultOk
    }
    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: uint32) -> tresult {
        let Some(k) = attr_key(id) else {
            return kInvalidArgument;
        };
        let map = self.map.borrow();
        let Some(Attr::Str(s)) = map.get(&k) else {
            return kResultFalse;
        };
        let cap = size_in_bytes as usize / 2;
        if cap == 0 {
            return kResultFalse;
        }
        let n = s.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(s.as_ptr(), string, n);
        *string.add(n) = 0;
        kResultOk
    }
    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size: uint32) -> tresult {
        let Some(k) = attr_key(id) else {
            return kInvalidArgument;
        };
        let bytes = std::slice::from_raw_parts(data as *const u8, size as usize).to_vec();
        self.map.borrow_mut().insert(k, Attr::Bin(bytes));
        kResultOk
    }
    unsafe fn getBinary(&self, id: AttrID, data: *mut *const c_void, size: *mut uint32) -> tresult {
        let Some(k) = attr_key(id) else {
            return kInvalidArgument;
        };
        let map = self.map.borrow();
        let Some(Attr::Bin(b)) = map.get(&k) else {
            return kResultFalse;
        };
        // The pointer stays valid as long as the attribute is not overwritten (VST3 contract).
        *data = b.as_ptr() as *const c_void;
        *size = b.len() as uint32;
        kResultOk
    }
}

struct HostMessage {
    id: RefCell<Vec<u8>>,
    attributes: ComWrapper<HostAttributes>,
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id.borrow().as_ptr() as FIDString
    }
    unsafe fn setMessageID(&self, id: FIDString) {
        let mut v = if id.is_null() {
            Vec::new()
        } else {
            CStr::from_ptr(id).to_bytes().to_vec()
        };
        v.push(0);
        *self.id.borrow_mut() = v;
    }
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

fn tuid_eq(a: *const TUID, iid: &[u8; 16]) -> bool {
    !a.is_null() && unsafe { (*a).iter().zip(iid).all(|(x, y)| *x as u8 == *y) }
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        copy_to_string128("rLive", &mut *name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if tuid_eq(cid, &IMessage::IID) && tuid_eq(iid, &IMessage::IID) {
            let msg = ComWrapper::new(HostMessage {
                id: RefCell::new(vec![0]),
                attributes: ComWrapper::new(HostAttributes::default()),
            });
            if let Some(p) = msg.to_com_ptr::<IMessage>() {
                *obj = p.into_raw() as *mut c_void;
                return kResultOk;
            }
        }
        if tuid_eq(cid, &IAttributeList::IID) && tuid_eq(iid, &IAttributeList::IID) {
            let attrs = ComWrapper::new(HostAttributes::default());
            if let Some(p) = attrs.to_com_ptr::<IAttributeList>() {
                *obj = p.into_raw() as *mut c_void;
                return kResultOk;
            }
        }
        *obj = std::ptr::null_mut();
        kResultFalse
    }
}

#[derive(Default)]
struct HandlerState {
    edits: Mutex<Vec<(u32, f64)>>,
    restart: AtomicI32,
}

struct ComponentHandler {
    state: Arc<HandlerState>,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.state.edits.lock().unwrap().push((id, value));
        kResultOk
    }
    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.state.restart.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct PlugFrame {
    resize: Arc<Mutex<Option<ViewRect>>>,
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kInvalidArgument;
        }
        *self.resize.lock().unwrap() = Some(*new_size);
        kResultTrue
    }
}

// --- audio-thread COM objects (single-threaded use only)

struct ParamQueue {
    id: Cell<ParamID>,
    points: UnsafeCell<Vec<(int32, ParamValue)>>,
}

unsafe impl Send for ParamQueue {}
unsafe impl Sync for ParamQueue {}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }
    unsafe fn getPointCount(&self) -> int32 {
        (*self.points.get()).len() as int32
    }
    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        match (&*self.points.get()).get(index as usize) {
            Some(&(o, v)) => {
                *offset = o;
                *value = v;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let points = &mut *self.points.get();
        if points.len() >= points.capacity() {
            return kResultFalse;
        }
        points.push((offset, value));
        if !index.is_null() {
            *index = points.len() as int32 - 1;
        }
        kResultOk
    }
}

struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    ptrs: Vec<*mut IParamValueQueue>,
    used: Cell<usize>,
}

unsafe impl Send for ParamChanges {}
unsafe impl Sync for ParamChanges {}

impl ParamChanges {
    fn new(capacity: usize, points: usize) -> Self {
        let queues: Vec<_> = (0..capacity)
            .map(|_| {
                ComWrapper::new(ParamQueue {
                    id: Cell::new(0),
                    points: UnsafeCell::new(Vec::with_capacity(points)),
                })
            })
            .collect();
        let ptrs = queues
            .iter()
            .map(|q| q.as_com_ref::<IParamValueQueue>().unwrap().as_ptr())
            .collect();
        Self {
            queues,
            ptrs,
            used: Cell::new(0),
        }
    }

    /// Queue a change. If the parameter's points are full, the last point takes the value: its
    /// timing is off, but the final value still reaches the plugin.
    fn add_or_merge(&self, id: ParamID, offset: i32, value: f64) {
        let mut index = 0;
        let q = unsafe { self.addParameterData(&id, &mut index) };
        if q.is_null() {
            return;
        }
        let queue = &self.queues[index as usize];
        let added = unsafe { queue.addPoint(offset, value, std::ptr::null_mut()) };
        if added != kResultOk {
            if let Some(last) = unsafe { (&mut *queue.points.get()).last_mut() } {
                last.1 = value;
            }
        }
    }

    fn clear(&self) {
        for q in &self.queues[..self.used.get()] {
            unsafe { (*q.points.get()).clear() };
        }
        self.used.set(0);
    }
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used.get() as int32
    }
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        if (index as usize) < self.used.get() {
            self.ptrs[index as usize]
        } else {
            std::ptr::null_mut()
        }
    }
    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        let id = *id;
        let used = self.used.get();
        if let Some(i) = self.queues[..used].iter().position(|q| q.id.get() == id) {
            *index = i as int32;
            return self.ptrs[i];
        }
        if used >= self.queues.len() {
            return std::ptr::null_mut();
        }
        self.queues[used].id.set(id);
        (*self.queues[used].points.get()).clear();
        self.used.set(used + 1);
        *index = used as int32;
        self.ptrs[used]
    }
}

struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

unsafe impl Send for EventList {}
unsafe impl Sync for EventList {}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        (*self.events.get()).len() as int32
    }
    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match (&*self.events.get()).get(index as usize) {
            Some(ev) => {
                *e = *ev;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        let v = &mut *self.events.get();
        if v.len() < v.capacity() {
            v.push(*e);
            kResultOk
        } else {
            kResultFalse
        }
    }
}

// ------------------------------------------------------------------ instance

/// IMidiMapping as a table, so the audio thread can read it: one parameter per channel and
/// controller, or [`NO_MAPPING`].
struct MidiMap {
    params: Vec<AtomicU32>,
}

const NO_MAPPING: u32 = u32::MAX;
const MIDI_CHANNELS: usize = 16;
/// Controllers 0 to 127, then VST3's aftertouch and pitch bend.
const MIDI_CONTROLLERS: usize = ControllerNumbers_::kPitchBend as usize + 1;

impl MidiMap {
    fn new() -> Self {
        Self {
            params: (0..MIDI_CHANNELS * MIDI_CONTROLLERS)
                .map(|_| AtomicU32::new(NO_MAPPING))
                .collect(),
        }
    }

    fn slot(channel: u8, controller: usize) -> usize {
        (channel as usize & 15) * MIDI_CONTROLLERS + controller
    }

    fn get(&self, channel: u8, controller: usize) -> Option<ParamID> {
        match self.params[Self::slot(channel, controller)].load(Ordering::Relaxed) {
            NO_MAPPING => None,
            id => Some(id),
        }
    }

    /// Ask the plugin for its assignments. Main thread only.
    fn read(&self, controller: Option<&ComPtr<IEditController>>) {
        let mapping = controller.and_then(|c| c.cast::<IMidiMapping>());
        for channel in 0..MIDI_CHANNELS {
            for cc in 0..MIDI_CONTROLLERS {
                let mut id: ParamID = 0;
                let found = mapping.as_ref().is_some_and(|m| unsafe {
                    m.getMidiControllerAssignment(0, channel as i16, cc as CtrlNumber, &mut id)
                        == kResultTrue
                });
                self.params[Self::slot(channel as u8, cc)]
                    .store(if found { id } else { NO_MAPPING }, Ordering::Relaxed);
            }
        }
    }

    /// The parameter a controller message moves, and to what, if the plugin maps it.
    fn change(&self, message: MidiMessage) -> Option<(ParamID, f64)> {
        let (channel, controller, value) = match message {
            MidiMessage::ControlChange { channel, cc, value } if cc < 128 => {
                (channel, cc as usize, value as f64 / 127.0)
            }
            MidiMessage::PitchBend { channel, value } => (
                channel,
                ControllerNumbers_::kPitchBend as usize,
                (value as f64 + 1.0) * 0.5,
            ),
            MidiMessage::ChannelPressure { channel, value } => (
                channel,
                ControllerNumbers_::kAfterTouch as usize,
                value as f64,
            ),
            _ => return None,
        };
        Some((self.get(channel, controller)?, value.clamp(0.0, 1.0)))
    }
}

struct Vst3Instance {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    midi_map: MidiMap,
    separate_controller: bool,
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    /// Latency and tail in frames, as last read on the main thread.
    latency_frames: AtomicU32,
    tail_frames: AtomicU32,
    _host: ComWrapper<HostApplication>,
    _handler: ComWrapper<ComponentHandler>,
    _lib: Arc<VstLibrary>,
}

unsafe impl Send for Vst3Instance {}
unsafe impl Sync for Vst3Instance {}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        unsafe {
            self.component.setActive(0);
            if let Some((a, b)) = &self.connections {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(std::ptr::null_mut());
                if self.separate_controller {
                    c.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

impl Vst3Instance {
    /// Read the latency and tail. Main thread only.
    fn read_audio_props(&self) {
        unsafe {
            self.latency_frames
                .store(self.processor.getLatencySamples(), Ordering::Relaxed);
            self.tail_frames
                .store(self.processor.getTailSamples(), Ordering::Relaxed);
        }
    }
}

/// The names of a plugin's extra buses, after its main one.
fn bus_names(component: &ComPtr<IComponent>, dir: BusDirection) -> Vec<String> {
    unsafe {
        let count = component.getBusCount(MediaTypes_::kAudio as MediaType, dir);
        (1..count)
            .map(|i| {
                let mut info: BusInfo = std::mem::zeroed();
                if component.getBusInfo(MediaTypes_::kAudio as MediaType, dir, i, &mut info)
                    == kResultOk
                {
                    let n = info.name.iter().take_while(|c| **c != 0).count();
                    String::from_utf16_lossy(&info.name[..n])
                } else {
                    format!("Output {}", i + 1)
                }
            })
            .collect()
    }
}

fn bus_channels(component: &ComPtr<IComponent>, dir: BusDirection) -> Vec<usize> {
    unsafe {
        let count = component.getBusCount(MediaTypes_::kAudio as MediaType, dir);
        (0..count)
            .map(|i| {
                let mut info: BusInfo = std::mem::zeroed();
                if component.getBusInfo(MediaTypes_::kAudio as MediaType, dir, i, &mut info)
                    == kResultOk
                {
                    info.channelCount.max(0) as usize
                } else {
                    2
                }
            })
            .collect()
    }
}

pub fn instantiate(
    desc: &PluginDescriptor,
    sample_rate: f64,
    max_block: usize,
    buses: crate::PluginBuses,
) -> Result<PluginInstance> {
    let lib = load_library(&desc.path)?;
    let cid = hex_to_tuid(&desc.id)
        .ok_or_else(|| HostError::NotFound(desc.id.clone(), desc.path.clone()))?;
    let host = ComWrapper::new(HostApplication);
    let host_ptr = host
        .as_com_ref::<IHostApplication>()
        .map_or(std::ptr::null_mut(), |r| r.as_ptr() as *mut FUnknown);
    let handler_state = Arc::new(HandlerState::default());
    let handler = ComWrapper::new(ComponentHandler {
        state: handler_state.clone(),
    });

    unsafe {
        let mut obj: *mut c_void = std::ptr::null_mut();
        if lib
            .factory()
            .createInstance(cid.as_ptr(), iid_ptr::<IComponent>(), &mut obj)
            != kResultOk
        {
            return Err(HostError::Init(format!(
                "createInstance failed for {}",
                desc.name
            )));
        }
        let component = ComPtr::<IComponent>::from_raw(obj as *mut IComponent)
            .ok_or_else(|| HostError::Init("null component".into()))?;
        if component.initialize(host_ptr) != kResultOk {
            return Err(HostError::Init("IComponent::initialize failed".into()));
        }
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or_else(|| HostError::Init("component has no IAudioProcessor".into()))?;

        // Controller: either the component itself or a separate class.
        let (controller, separate) = match component.cast::<IEditController>() {
            Some(c) => (Some(c), false),
            None => {
                let mut ccid: TUID = [0; 16];
                let mut ctrl = None;
                if component.getControllerClassId(&mut ccid) == kResultOk {
                    let mut obj: *mut c_void = std::ptr::null_mut();
                    if lib.factory().createInstance(
                        ccid.as_ptr(),
                        iid_ptr::<IEditController>(),
                        &mut obj,
                    ) == kResultOk
                    {
                        if let Some(c) =
                            ComPtr::<IEditController>::from_raw(obj as *mut IEditController)
                        {
                            if c.initialize(host_ptr) == kResultOk {
                                ctrl = Some(c);
                            }
                        }
                    }
                }
                (ctrl, true)
            }
        };

        let mut connections = None;
        if let (true, Some(ctrl)) = (separate, &controller) {
            if let (Some(a), Some(b)) = (
                component.cast::<IConnectionPoint>(),
                ctrl.cast::<IConnectionPoint>(),
            ) {
                a.connect(b.as_ptr());
                b.connect(a.as_ptr());
                connections = Some((a, b));
            }
        }
        if let Some(ctrl) = &controller {
            let stream = new_stream(Vec::new());
            if component.getState(stream_ptr(&stream)) == kResultOk {
                stream.pos.set(0);
                ctrl.setComponentState(stream_ptr(&stream));
            }
            if let Some(h) = handler.as_com_ref::<IComponentHandler>() {
                ctrl.setComponentHandler(h.as_ptr());
            }
        }

        // Buses: stereo main in/out, activate main audio + event buses.
        let n_in = component.getBusCount(
            MediaTypes_::kAudio as MediaType,
            BusDirections_::kInput as BusDirection,
        );
        let n_out = component.getBusCount(
            MediaTypes_::kAudio as MediaType,
            BusDirections_::kOutput as BusDirection,
        );
        let mut in_arr: Vec<SpeakerArrangement> = (0..n_in)
            .map(|i| {
                let mut a: SpeakerArrangement = SpeakerArr::kStereo;
                processor.getBusArrangement(BusDirections_::kInput as BusDirection, i, &mut a);
                if i == 0 {
                    SpeakerArr::kStereo
                } else {
                    a
                }
            })
            .collect();
        let mut out_arr: Vec<SpeakerArrangement> = (0..n_out)
            .map(|i| {
                let mut a: SpeakerArrangement = SpeakerArr::kStereo;
                processor.getBusArrangement(BusDirections_::kOutput as BusDirection, i, &mut a);
                if i == 0 {
                    SpeakerArr::kStereo
                } else {
                    a
                }
            })
            .collect();
        processor.setBusArrangements(in_arr.as_mut_ptr(), n_in, out_arr.as_mut_ptr(), n_out);
        if n_in > 0 {
            component.activateBus(
                MediaTypes_::kAudio as MediaType,
                BusDirections_::kInput as BusDirection,
                0,
                1,
            );
        }
        if n_out > 0 {
            component.activateBus(
                MediaTypes_::kAudio as MediaType,
                BusDirections_::kOutput as BusDirection,
                0,
                1,
            );
        }
        if buses.sidechain && n_in > 1 {
            component.activateBus(
                MediaTypes_::kAudio as MediaType,
                BusDirections_::kInput as BusDirection,
                1,
                1,
            );
        }
        if buses.aux {
            for bus in 1..n_out {
                component.activateBus(
                    MediaTypes_::kAudio as MediaType,
                    BusDirections_::kOutput as BusDirection,
                    bus,
                    1,
                );
            }
        }
        if component.getBusCount(
            MediaTypes_::kEvent as MediaType,
            BusDirections_::kInput as BusDirection,
        ) > 0
        {
            component.activateBus(
                MediaTypes_::kEvent as MediaType,
                BusDirections_::kInput as BusDirection,
                0,
                1,
            );
        }
        let midi_output = component.getBusCount(
            MediaTypes_::kEvent as MediaType,
            BusDirections_::kOutput as BusDirection,
        ) > 0;
        if midi_output {
            component.activateBus(
                MediaTypes_::kEvent as MediaType,
                BusDirections_::kOutput as BusDirection,
                0,
                1,
            );
        }

        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime as int32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            maxSamplesPerBlock: max_block as int32,
            sampleRate: sample_rate,
        };
        if processor.setupProcessing(&mut setup) != kResultOk {
            return Err(HostError::Activate("setupProcessing failed".into()));
        }
        if component.setActive(1) != kResultOk {
            return Err(HostError::Activate("setActive failed".into()));
        }

        let inputs = bus_channels(&component, BusDirections_::kInput as BusDirection);
        let outputs = bus_channels(&component, BusDirections_::kOutput as BusDirection);
        let aux_names = bus_names(&component, BusDirections_::kOutput as BusDirection);
        let has_sidechain = n_in > 1;
        let midi_map = MidiMap::new();
        midi_map.read(controller.as_ref());
        let inst = Arc::new(Vst3Instance {
            component,
            processor,
            controller,
            midi_map,
            separate_controller: separate,
            connections,
            latency_frames: AtomicU32::new(0),
            tail_frames: AtomicU32::new(0),
            _host: host,
            _handler: handler,
            _lib: lib,
        });
        inst.read_audio_props();
        let proc_ = Vst3Processor::new(
            inst.clone(),
            &inputs,
            &outputs,
            max_block,
            sample_rate,
            midi_output,
        );
        let mut handle = Vst3Handle {
            inst,
            has_sidechain,
            aux_names,
            midi_output,
            desc: desc.clone(),
            params: Vec::new(),
            handler: handler_state,
            editor: None,
            title: None,
        };
        handle.rescan_params();
        Ok(PluginInstance {
            handle: Box::new(handle),
            processor: Box::new(proc_),
        })
    }
}

// ------------------------------------------------------------------ handle

#[cfg(target_os = "macos")]
struct Editor {
    view: ComPtr<IPlugView>,
    _frame: ComWrapper<PlugFrame>,
    resize: Arc<Mutex<Option<ViewRect>>>,
    window: crate::mac::EditorWindow,
}

pub struct Vst3Handle {
    inst: Arc<Vst3Instance>,
    has_sidechain: bool,
    aux_names: Vec<String>,
    midi_output: bool,
    desc: PluginDescriptor,
    params: Vec<ParamInfo>,
    handler: Arc<HandlerState>,
    #[cfg(target_os = "macos")]
    editor: Option<Editor>,
    #[cfg(not(target_os = "macos"))]
    editor: Option<()>,
    /// What the window should say, once it is open.
    title: Option<String>,
}

const STATE_MAGIC: &[u8; 4] = b"RLV3";

const PRESET_MAGIC: &[u8; 4] = b"VST3";
const PRESET_HEADER: usize = 48;
const COMPONENT_CHUNK: &[u8; 4] = b"Comp";
const CONTROLLER_CHUNK: &[u8; 4] = b"Cont";

/// The component and controller halves of one of [`Vst3Handle`]'s state blobs.
fn split_state(data: &[u8]) -> Option<(&[u8], &[u8])> {
    if data.len() < 8 || &data[..4] != STATE_MAGIC {
        return None;
    }
    let chunk = |at: usize| -> Option<(&[u8], usize)> {
        let len = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        Some((data.get(at + 4..at + 4 + len)?, at + 4 + len))
    };
    let (comp, next) = chunk(4)?;
    Some((comp, chunk(next).map_or(&[][..], |c| c.0)))
}

fn join_state(comp: &[u8], ctrl: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + comp.len() + ctrl.len());
    out.extend_from_slice(STATE_MAGIC);
    out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
    out.extend_from_slice(comp);
    out.extend_from_slice(&(ctrl.len() as u32).to_le_bytes());
    out.extend_from_slice(ctrl);
    out
}

/// A plug-in's state as a `.vstpreset` file: the header, the chunks, and a list of where they are.
pub fn encode_preset(class_id: &str, state: &[u8]) -> Option<Vec<u8>> {
    if class_id.len() != 32 || !class_id.is_ascii() {
        return None;
    }
    let (comp, ctrl) = split_state(state)?;
    let mut out = Vec::with_capacity(PRESET_HEADER + comp.len() + ctrl.len() + 64);
    out.extend_from_slice(PRESET_MAGIC);
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(class_id.as_bytes());
    out.extend_from_slice(&0i64.to_le_bytes());
    let mut chunks: Vec<(&[u8; 4], i64, i64)> = Vec::new();
    for (id, body) in [(COMPONENT_CHUNK, comp), (CONTROLLER_CHUNK, ctrl)] {
        chunks.push((id, out.len() as i64, body.len() as i64));
        out.extend_from_slice(body);
    }
    let list = out.len() as i64;
    out[40..48].copy_from_slice(&list.to_le_bytes());
    out.extend_from_slice(b"List");
    out.extend_from_slice(&(chunks.len() as i32).to_le_bytes());
    for (id, offset, size) in chunks {
        out.extend_from_slice(id);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    }
    Some(out)
}

/// The class it is for and the state in a `.vstpreset` file.
pub fn decode_preset(file: &[u8]) -> Option<(String, Vec<u8>)> {
    if file.len() < PRESET_HEADER || &file[..4] != PRESET_MAGIC {
        return None;
    }
    let class_id = std::str::from_utf8(file.get(8..40)?).ok()?.to_string();
    let list = i64::from_le_bytes(file.get(40..48)?.try_into().ok()?) as usize;
    if file.get(list..list + 4)? != b"List" {
        return None;
    }
    let count = i32::from_le_bytes(file.get(list + 4..list + 8)?.try_into().ok()?).max(0) as usize;
    let (mut comp, mut ctrl) = (None, &[][..]);
    for i in 0..count {
        let at = list + 8 + i * 20;
        let id = file.get(at..at + 4)?;
        let offset = i64::from_le_bytes(file.get(at + 4..at + 12)?.try_into().ok()?) as usize;
        let size = i64::from_le_bytes(file.get(at + 12..at + 20)?.try_into().ok()?) as usize;
        let body = file.get(offset..offset.checked_add(size)?)?;
        if id == COMPONENT_CHUNK {
            comp = Some(body);
        } else if id == CONTROLLER_CHUNK {
            ctrl = body;
        }
    }
    Some((class_id, join_state(comp?, ctrl)))
}

impl Vst3Handle {
    fn rescan_params(&mut self) {
        self.params.clear();
        let Some(ctrl) = &self.inst.controller else {
            return;
        };
        unsafe {
            for i in 0..ctrl.getParameterCount() {
                let mut info: ParameterInfo = std::mem::zeroed();
                if ctrl.getParameterInfo(i, &mut info) != kResultOk {
                    continue;
                }
                let hidden = info.flags & ParameterInfo_::ParameterFlags_::kIsHidden != 0;
                let kind = if info.stepCount == 1 {
                    ParamKind::Toggle
                } else if (2..=64).contains(&info.stepCount) {
                    // Stepped values are normalized; expose them as indices 0..=steps.
                    ParamKind::Continuous { skew: 1.0 }
                } else {
                    ParamKind::Continuous { skew: 1.0 }
                };
                let mut name = string128_to_string(&info.title);
                if name.is_empty() {
                    name = string128_to_string(&info.shortTitle);
                }
                self.params.push(ParamInfo {
                    id: info.id,
                    name,
                    min: 0.0,
                    max: 1.0,
                    default: info.defaultNormalizedValue,
                    unit: string128_to_string(&info.units),
                    kind,
                    mod_mode: Default::default(),
                    flags: ParamFlags::for_plugin(hidden),
                });
            }
        }
    }
}

impl PluginHandle for Vst3Handle {
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

    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }

    fn param_value(&self, id: u32) -> Option<f64> {
        let ctrl = self.inst.controller.as_ref()?;
        Some(unsafe { ctrl.getParamNormalized(id) })
    }

    fn format_value(&self, id: u32, value: f64) -> Option<String> {
        let ctrl = self.inst.controller.as_ref()?;
        let mut s: String128 = [0; 128];
        unsafe {
            (ctrl.getParamStringByValue(id, value, &mut s) == kResultOk)
                .then(|| string128_to_string(&s))
        }
    }

    fn on_param_set(&mut self, id: u32, value: f64) {
        if let Some(ctrl) = &self.inst.controller {
            unsafe { ctrl.setParamNormalized(id, value) };
        }
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let comp = new_stream(Vec::new());
        let ctrl = new_stream(Vec::new());
        unsafe {
            if self.inst.component.getState(stream_ptr(&comp)) != kResultOk {
                return None;
            }
            if let Some(c) = &self.inst.controller {
                c.getState(stream_ptr(&ctrl));
            }
        }
        let comp = comp.data.borrow();
        let ctrl = ctrl.data.borrow();
        let mut out = Vec::with_capacity(12 + comp.len() + ctrl.len());
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(&comp);
        out.extend_from_slice(&(ctrl.len() as u32).to_le_bytes());
        out.extend_from_slice(&ctrl);
        Some(out)
    }

    fn load_state(&mut self, data: &[u8]) -> bool {
        if data.len() < 8 || &data[..4] != STATE_MAGIC {
            return false;
        }
        let read_chunk = |at: usize| -> Option<(&[u8], usize)> {
            let len = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
            Some((data.get(at + 4..at + 4 + len)?, at + 4 + len))
        };
        let Some((comp, next)) = read_chunk(4) else {
            return false;
        };
        let ctrl = read_chunk(next).map(|c| c.0).unwrap_or(&[]);
        unsafe {
            let s = new_stream(comp.to_vec());
            if self.inst.component.setState(stream_ptr(&s)) != kResultOk {
                return false;
            }
            if let Some(c) = &self.inst.controller {
                let s = new_stream(comp.to_vec());
                c.setComponentState(stream_ptr(&s));
                if !ctrl.is_empty() {
                    let s = new_stream(ctrl.to_vec());
                    c.setState(stream_ptr(&s));
                }
            }
        }
        self.rescan_params();
        true
    }

    fn has_editor(&self) -> bool {
        cfg!(target_os = "macos") && self.inst.controller.is_some()
    }

    fn is_editor_open(&self) -> bool {
        self.editor.is_some()
    }

    #[cfg(target_os = "macos")]
    fn open_editor(&mut self) -> Result<()> {
        if self.editor.is_some() {
            return Ok(());
        }
        let ctrl = self.inst.controller.as_ref().ok_or(HostError::NoEditor)?;
        unsafe {
            let view = ComPtr::<IPlugView>::from_raw(ctrl.createView(c"editor".as_ptr()))
                .ok_or(HostError::NoEditor)?;
            if view.isPlatformTypeSupported(kPlatformTypeNSView) != kResultTrue {
                return Err(HostError::Editor("plugin does not support NSView".into()));
            }
            let mut rect: ViewRect = std::mem::zeroed();
            view.getSize(&mut rect);
            let (w, h) = (
                (rect.right - rect.left).max(100) as f64,
                (rect.bottom - rect.top).max(100) as f64,
            );
            let resizable = view.canResize() == kResultTrue;
            let title = self.title.as_deref().unwrap_or(&self.desc.name);
            let window = crate::mac::EditorWindow::new(title, w, h, resizable)
                .ok_or_else(|| HostError::Editor("could not create window".into()))?;
            let resize = Arc::new(Mutex::new(None));
            let frame = ComWrapper::new(PlugFrame {
                resize: resize.clone(),
            });
            if let Some(f) = frame.as_com_ref::<IPlugFrame>() {
                view.setFrame(f.as_ptr());
            }
            if view.attached(window.ns_view(), kPlatformTypeNSView) != kResultOk {
                view.setFrame(std::ptr::null_mut());
                window.close();
                return Err(HostError::Editor("IPlugView::attached failed".into()));
            }
            window.show();
            self.editor = Some(Editor {
                view,
                _frame: frame,
                resize,
                window,
            });
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn open_editor(&mut self) -> Result<()> {
        Err(HostError::Editor(
            "VST3 editors are only supported on macOS for now".into(),
        ))
    }

    fn close_editor(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(e) = self.editor.take() {
            unsafe {
                e.view.removed();
                e.view.setFrame(std::ptr::null_mut());
            }
            e.window.close();
        }
    }

    fn set_editor_visible(&mut self, visible: bool) {
        #[cfg(target_os = "macos")]
        if let Some(e) = &self.editor {
            e.window.set_visible(visible);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = visible;
    }

    fn set_editor_title(&mut self, title: &str) {
        self.title = Some(title.to_string());
        #[cfg(target_os = "macos")]
        if let Some(e) = &self.editor {
            e.window.set_title(title);
        }
    }

    fn idle(&mut self) -> Vec<HostNotification> {
        let mut out: Vec<HostNotification> = self
            .handler
            .edits
            .lock()
            .unwrap()
            .drain(..)
            .map(|(id, value)| HostNotification::ParamChanged { id, value })
            .collect();
        let flags = self.handler.restart.swap(0, Ordering::AcqRel);
        if flags & (RestartFlags_::kParamTitlesChanged | RestartFlags_::kParamValuesChanged) != 0 {
            self.rescan_params();
            out.push(HostNotification::ParamsRescanned);
        }
        if flags & RestartFlags_::kMidiCCAssignmentChanged != 0 {
            self.inst.midi_map.read(self.inst.controller.as_ref());
        }
        if flags & RestartFlags_::kLatencyChanged != 0 {
            self.inst.read_audio_props();
            out.push(HostNotification::LatencyChanged);
        }
        if flags & (RestartFlags_::kReloadComponent | RestartFlags_::kIoChanged) != 0 {
            out.push(HostNotification::RestartRequested);
        }
        #[cfg(target_os = "macos")]
        {
            let mut closed = false;
            if let Some(e) = &self.editor {
                if let Some(mut r) = e.resize.lock().unwrap().take() {
                    e.window
                        .resize((r.right - r.left) as f64, (r.bottom - r.top) as f64);
                    unsafe { e.view.onSize(&mut r) };
                }
                closed = !e.window.is_visible();
            }
            if closed {
                self.close_editor();
                out.push(HostNotification::EditorClosed);
            }
        }
        out
    }

    fn latency(&self) -> u32 {
        self.inst.read_audio_props();
        self.inst.latency_frames.load(Ordering::Relaxed)
    }
}

impl Drop for Vst3Handle {
    fn drop(&mut self) {
        self.close_editor();
    }
}

// ------------------------------------------------------------------ processor

pub struct Vst3Processor {
    inst: Arc<Vst3Instance>,
    started: bool,
    inputs: Vec<PortBuffers>,
    outputs: Vec<PortBuffers>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    in_changes: ComWrapper<ParamChanges>,
    out_changes: ComWrapper<ParamChanges>,
    events: ComWrapper<EventList>,
    out_events: ComWrapper<EventList>,
    in_changes_ptr: *mut IParameterChanges,
    out_changes_ptr: *mut IParameterChanges,
    events_ptr: *mut IEventList,
    /// Null when the plugin has no MIDI output.
    out_events_ptr: *mut IEventList,
    context: vst3::Steinberg::Vst::ProcessContext,
    is_instrument: bool,
    max_block: usize,
    sample_rate: f64,
    /// Set with MPE Mode on: notes go out per channel, and expression as mapped controllers.
    mpe: Option<MpeEncoder>,
    midi_out: MidiOutput,
}

unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    fn new(
        inst: Arc<Vst3Instance>,
        inputs: &[usize],
        outputs: &[usize],
        max_block: usize,
        sample_rate: f64,
        midi_output: bool,
    ) -> Self {
        let mut inputs: Vec<PortBuffers> = inputs
            .iter()
            .map(|&c| PortBuffers::new(c, max_block))
            .collect();
        let mut outputs: Vec<PortBuffers> = outputs
            .iter()
            .map(|&c| PortBuffers::new(c, max_block))
            .collect();
        let mk = |p: &mut PortBuffers| AudioBusBuffers {
            numChannels: p.count() as int32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: p.ptrs.as_mut_ptr(),
            },
        };
        let in_buses = inputs.iter_mut().map(mk).collect();
        let out_buses = outputs.iter_mut().map(mk).collect();
        // Enough points for a change at every control step of the longest block.
        let points = (max_block / 16).max(16);
        let in_changes = ComWrapper::new(ParamChanges::new(512, points));
        let out_changes = ComWrapper::new(ParamChanges::new(128, 16));
        let events = ComWrapper::new(EventList {
            events: UnsafeCell::new(Vec::with_capacity(1024)),
        });
        let in_changes_ptr = in_changes
            .as_com_ref::<IParameterChanges>()
            .unwrap()
            .as_ptr();
        let out_changes_ptr = out_changes
            .as_com_ref::<IParameterChanges>()
            .unwrap()
            .as_ptr();
        let events_ptr = events.as_com_ref::<IEventList>().unwrap().as_ptr();
        let out_events = ComWrapper::new(EventList {
            events: UnsafeCell::new(Vec::with_capacity(1024)),
        });
        let out_events_ptr = if midi_output {
            out_events.as_com_ref::<IEventList>().unwrap().as_ptr()
        } else {
            std::ptr::null_mut()
        };
        Self {
            is_instrument: inputs.is_empty(),
            inst,
            started: false,
            inputs,
            outputs,
            in_buses,
            out_buses,
            in_changes,
            out_changes,
            events,
            out_events,
            in_changes_ptr,
            out_changes_ptr,
            events_ptr,
            out_events_ptr,
            context: unsafe { std::mem::zeroed() },
            max_block,
            sample_rate,
            mpe: None,
            midi_out: MidiOutput::default(),
        }
    }

    /// One event of the block, in the place VST3 wants it: as an event, or as a mapped parameter.
    fn add_event(&self, message: MidiMessage, offset: int32) {
        if let Some((id, value)) = self.inst.midi_map.change(message) {
            self.in_changes.add_or_merge(id, offset, value);
        } else if let Some(ev) = to_vst3_event(message, offset) {
            let events = unsafe { &mut *self.events.events.get() };
            if events.len() < events.capacity() {
                events.push(ev);
            }
        }
    }

    /// Hand what the plugin sent out during this block to the next device.
    fn collect_events(&mut self, out: &mut EventQueue, frames: usize) {
        let events = unsafe { &mut *self.out_events.events.get() };
        for e in events.iter() {
            let offset = (e.sampleOffset.max(0) as u32).min(frames.saturating_sub(1) as u32);
            if let Some(message) = from_vst3_event(e) {
                self.midi_out.push(out, offset, message);
            }
        }
        events.clear();
    }
}

/// An event from a plugin as a MIDI message. `None` for what has no MIDI form.
fn from_vst3_event(e: &Event) -> Option<MidiMessage> {
    use Event_::EventTypes_ as T;
    let id = |raw: int32| raw.max(0) as NoteId;
    let wire = |v: i16| (v.max(0) as u8) & 127;
    unsafe {
        match e.r#type as u32 {
            T::kNoteOnEvent => {
                let n = e.__field0.noteOn;
                Some(MidiMessage::NoteOn {
                    channel: n.channel.max(0) as u8 & 15,
                    key: wire(n.pitch),
                    velocity: n.velocity,
                    note_id: id(n.noteId),
                    tuning: n.tuning / 100.0,
                })
            }
            T::kNoteOffEvent => {
                let n = e.__field0.noteOff;
                Some(MidiMessage::NoteOff {
                    channel: n.channel.max(0) as u8 & 15,
                    key: wire(n.pitch),
                    velocity: n.velocity,
                    note_id: id(n.noteId),
                })
            }
            T::kPolyPressureEvent => {
                let n = e.__field0.polyPressure;
                Some(MidiMessage::NoteExpr {
                    channel: n.channel.max(0) as u8 & 15,
                    key: wire(n.pitch),
                    note_id: id(n.noteId),
                    kind: ExprKind::Pressure,
                    value: n.pressure,
                })
            }
            T::kLegacyMIDICCOutEvent => {
                let c = e.__field0.midiCCOut;
                let channel = c.channel.max(0) as u8 & 15;
                match c.controlNumber as u32 {
                    n if n < 128 => Some(MidiMessage::ControlChange {
                        channel,
                        cc: n as u8,
                        value: c.value as u8 & 127,
                    }),
                    ControllerNumbers_::kAfterTouch => Some(MidiMessage::ChannelPressure {
                        channel,
                        value: (c.value as u8 & 127) as f32 / 127.0,
                    }),
                    ControllerNumbers_::kPitchBend => {
                        let lsb = (c.value as u16) & 127;
                        let msb = (c.value2 as u16) & 127;
                        let raw = (msb << 7 | lsb) as f32;
                        Some(MidiMessage::PitchBend {
                            channel,
                            value: ((raw - 8192.0) / 8191.0).clamp(-1.0, 1.0),
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// A MIDI message as a VST3 event. `None` for what VST3 has no event for: controllers and
/// pitch bend go through IMidiMapping parameters in VST3, which [`MidiMap`] takes care of.
fn to_vst3_event(message: MidiMessage, offset: int32) -> Option<Event> {
    let mut ev: Event = unsafe { std::mem::zeroed() };
    ev.busIndex = 0;
    ev.sampleOffset = offset;
    ev.flags = Event_::EventFlags_::kIsLive as u16;
    match message {
        MidiMessage::NoteOn {
            channel,
            key,
            velocity,
            note_id,
            tuning,
        } => {
            ev.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
            ev.__field0.noteOn = NoteOnEvent {
                channel: channel as i16,
                pitch: key as i16,
                // VST3 tunes in cents; our tuning is in semitones.
                tuning: tuning * 100.0,
                velocity,
                length: 0,
                noteId: wire_note_id(note_id),
            };
        }
        MidiMessage::NoteOff {
            channel,
            key,
            velocity,
            note_id,
        } => {
            ev.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
            ev.__field0.noteOff = NoteOffEvent {
                channel: channel as i16,
                pitch: key as i16,
                velocity,
                noteId: wire_note_id(note_id),
                tuning: 0.0,
            };
        }
        // Poly pressure is a native VST3 event, and works without an id too.
        MidiMessage::NoteExpr {
            channel,
            key,
            note_id,
            kind: ExprKind::Pressure,
            value,
        } => {
            ev.r#type = Event_::EventTypes_::kPolyPressureEvent as u16;
            ev.__field0.polyPressure = PolyPressureEvent {
                channel: channel as i16,
                pitch: key as i16,
                pressure: value,
                noteId: wire_note_id(note_id),
            };
        }
        // Note expression must name a note, so it can't apply without an id.
        MidiMessage::NoteExpr { note_id: 0, .. } => return None,
        MidiMessage::NoteExpr {
            note_id,
            kind,
            value,
            ..
        } => {
            ev.r#type = Event_::EventTypes_::kNoteExpressionValueEvent as u16;
            let (type_id, normalized) = match kind {
                // Tuning is normalised over +-120 semitones, with 0.5 meaning none.
                ExprKind::Pitch => (
                    NoteExpressionTypeIDs_::kTuningTypeID,
                    0.5 + value as f64 / 240.0,
                ),
                _ => (NoteExpressionTypeIDs_::kBrightnessTypeID, value as f64),
            };
            ev.__field0.noteExpressionValue = NoteExpressionValueEvent {
                typeId: type_id,
                noteId: wire_note_id(note_id),
                value: normalized.clamp(0.0, 1.0),
            };
        }
        _ => return None,
    }
    Some(ev)
}

#[cfg(test)]
mod event_tests {
    use super::*;

    fn on(key: u8, note_id: rlive_core::NoteId) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 2,
            key,
            velocity: 1.0,
            note_id,
            tuning: 1.5,
        }
    }

    #[test]
    fn note_ids_and_tuning_reach_note_on() {
        let ev = to_vst3_event(on(60, 77), 9).unwrap();
        assert_eq!(ev.sampleOffset, 9);
        assert_eq!(ev.r#type, Event_::EventTypes_::kNoteOnEvent as u16);
        unsafe {
            assert_eq!(ev.__field0.noteOn.noteId, 77);
            assert_eq!(ev.__field0.noteOn.pitch, 60);
            assert_eq!(ev.__field0.noteOn.channel, 2);
            assert_eq!(ev.__field0.noteOn.tuning, 150.0);
        }
    }

    #[test]
    fn an_unassigned_id_becomes_minus_one() {
        let ev = to_vst3_event(on(60, 0), 0).unwrap();
        unsafe { assert_eq!(ev.__field0.noteOn.noteId, -1) };
    }

    #[test]
    fn note_off_carries_its_id_and_release_velocity() {
        let ev = to_vst3_event(
            MidiMessage::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0.25,
                note_id: 5,
            },
            0,
        )
        .unwrap();
        assert_eq!(ev.r#type, Event_::EventTypes_::kNoteOffEvent as u16);
        unsafe {
            assert_eq!(ev.__field0.noteOff.noteId, 5);
            assert_eq!(ev.__field0.noteOff.velocity, 0.25);
        }
    }

    fn expr(note_id: rlive_core::NoteId, kind: ExprKind, value: f32) -> MidiMessage {
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id,
            kind,
            value,
        }
    }

    #[test]
    fn pitch_is_normalised_tuning_expression() {
        let ev = to_vst3_event(expr(5, ExprKind::Pitch, 12.0), 0).unwrap();
        assert_eq!(
            ev.r#type,
            Event_::EventTypes_::kNoteExpressionValueEvent as u16
        );
        unsafe {
            assert_eq!(ev.__field0.noteExpressionValue.noteId, 5);
            assert_eq!(
                ev.__field0.noteExpressionValue.typeId,
                NoteExpressionTypeIDs_::kTuningTypeID
            );
            assert!((ev.__field0.noteExpressionValue.value - 0.55).abs() < 1e-9);
        }
    }

    #[test]
    fn slide_is_brightness_expression() {
        let ev = to_vst3_event(expr(5, ExprKind::Slide, 0.75), 0).unwrap();
        unsafe {
            assert_eq!(
                ev.__field0.noteExpressionValue.typeId,
                NoteExpressionTypeIDs_::kBrightnessTypeID
            );
            assert_eq!(ev.__field0.noteExpressionValue.value, 0.75);
        }
    }

    #[test]
    fn pressure_is_poly_pressure_even_without_an_id() {
        let ev = to_vst3_event(expr(0, ExprKind::Pressure, 0.5), 0).unwrap();
        assert_eq!(ev.r#type, Event_::EventTypes_::kPolyPressureEvent as u16);
        unsafe {
            assert_eq!(ev.__field0.polyPressure.noteId, -1);
            assert_eq!(ev.__field0.polyPressure.pressure, 0.5);
        }
    }

    #[test]
    fn note_expression_without_an_id_is_dropped() {
        assert!(to_vst3_event(expr(0, ExprKind::Pitch, 1.0), 0).is_none());
    }
}

impl Processor for Vst3Processor {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        debug_assert!(sample_rate == self.sample_rate && max_block <= self.max_block);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let frames = ctx.audio.frames().min(self.max_block);
        if !self.started {
            unsafe { self.inst.processor.setProcessing(1) };
            self.started = true;
        }

        self.out_changes.clear();
        let last = frames.saturating_sub(1) as u32;
        for e in ctx.params {
            self.in_changes
                .add_or_merge(e.id.0, e.offset.min(last) as int32, e.value);
        }
        unsafe {
            (*self.events.events.get()).clear();
            (*self.out_events.events.get()).clear();
        }
        match self.mpe.take() {
            Some(mut mpe) => {
                let mut pass = |e: MidiEvent| {
                    self.add_event(e.message, e.offset.min(last) as int32);
                };
                mpe.encode(ctx.events, &mut pass);
                self.mpe = Some(mpe);
            }
            None => {
                for e in ctx.events {
                    self.add_event(e.message, e.offset.min(last) as int32);
                }
            }
        }

        let t = ctx.transport;
        let beats = t.beats();
        let ppq = rlive_core::PPQ as f64;
        use ProcessContext_::StatesAndFlags_ as F;
        self.context.state =
            (F::kTempoValid | F::kTimeSigValid | F::kProjectTimeMusicValid | F::kBarPositionValid)
                | if t.playing { F::kPlaying } else { 0 }
                | if t.loop_range.is_some() {
                    F::kCycleValid | F::kCycleActive
                } else {
                    0
                };
        self.context.sampleRate = t.sample_rate;
        self.context.projectTimeSamples = t.sample_pos as i64;
        self.context.projectTimeMusic = beats;
        self.context.barPositionMusic = t.bar_start / ppq;
        let (cycle_start, cycle_end) = t.loop_range.unwrap_or((0.0, 0.0));
        self.context.cycleStartMusic = cycle_start / ppq;
        self.context.cycleEndMusic = cycle_end / ppq;
        self.context.tempo = t.tempo;
        self.context.timeSigNumerator = t.time_signature.numerator as i32;
        self.context.timeSigDenominator = t.time_signature.denominator as i32;

        let left = &mut ctx.audio.left[..frames];
        let right = &mut ctx.audio.right[..frames];
        if let Some(main) = self.inputs.first_mut() {
            main.load_stereo(left, right);
        }
        for p in self.inputs.iter_mut().skip(1) {
            p.clear(frames);
        }
        if let (Some(sc), Some(port)) = (&ctx.sidechain, self.inputs.get_mut(1)) {
            let n = frames.min(sc.frames());
            port.load_stereo(&sc.left[..n], &sc.right[..n]);
        }
        for p in self.outputs.iter_mut() {
            p.clear(frames);
        }

        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as int32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            numSamples: frames as int32,
            numInputs: self.in_buses.len() as int32,
            numOutputs: self.out_buses.len() as int32,
            inputs: self.in_buses.as_mut_ptr(),
            outputs: self.out_buses.as_mut_ptr(),
            inputParameterChanges: self.in_changes_ptr,
            outputParameterChanges: self.out_changes_ptr,
            inputEvents: self.events_ptr,
            outputEvents: self.out_events_ptr,
            processContext: &mut self.context,
        };
        unsafe { self.inst.processor.process(&mut data) };
        self.in_changes.clear();
        self.collect_events(&mut *ctx.events_out, frames);

        for i in 0..self.out_changes.used.get() {
            let q = &self.out_changes.queues[i];
            if let Some(&(_, v)) = unsafe { (&*q.points.get()).last() } {
                ctx.param_out.push(q.id.get(), v);
            }
        }
        if let Some(main) = self.outputs.first() {
            main.store_stereo(left, right, self.is_instrument);
        }
        for (port, aux) in self.outputs.iter().skip(1).zip(ctx.aux_out.iter_mut()) {
            let n = frames.min(aux.frames());
            port.store_stereo(&mut aux.left[..n], &mut aux.right[..n], false);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.in_changes.add_or_merge(id, 0, value);
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot != MPE_SLOT {
            return Some(resource);
        }
        if let Some(output) = resource.downcast_ref::<Option<MpeOutput>>() {
            self.mpe = (*output).map(MpeEncoder::configured);
            self.midi_out = MidiOutput::new_for(*output);
        }
        Some(resource)
    }

    fn reset(&mut self) {
        if let Some(mpe) = self.mpe.as_mut() {
            mpe.reset();
        }
    }

    fn stop_processing(&mut self) {
        if self.started {
            unsafe { self.inst.processor.setProcessing(0) };
            self.started = false;
        }
    }

    fn latency(&self) -> u32 {
        self.inst.latency_frames.load(Ordering::Relaxed)
    }

    fn tail(&self) -> Tail {
        Tail::from_samples(self.inst.tail_frames.load(Ordering::Relaxed))
    }
}

impl Drop for Vst3Processor {
    fn drop(&mut self) {
        self.stop_processing();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_file_round_trips_and_is_laid_out_as_vst3_says() {
        let state = join_state(b"component", b"ctrl");
        let id = "0123456789ABCDEF0123456789ABCDEF";
        let file = encode_preset(id, &state).unwrap();
        assert_eq!(&file[..4], b"VST3");
        assert_eq!(&file[8..40], id.as_bytes());
        assert_eq!(&file[48..57], b"component");
        let list = i64::from_le_bytes(file[40..48].try_into().unwrap()) as usize;
        assert_eq!(&file[list..list + 4], b"List");
        assert_eq!(decode_preset(&file), Some((id.to_string(), state)));
    }

    #[test]
    fn a_damaged_preset_file_is_refused() {
        assert_eq!(decode_preset(b"not a preset"), None);
        let file = encode_preset(&"A".repeat(32), &join_state(b"x", b"y")).unwrap();
        assert_eq!(decode_preset(&file[..file.len() - 3]), None);
        assert_eq!(encode_preset("short", &join_state(b"x", b"y")), None);
        assert_eq!(encode_preset(&"A".repeat(32), b"not our state"), None);
    }

    #[test]
    fn tuid_hex_roundtrip() {
        let t: TUID = [1, -2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 127];
        assert_eq!(hex_to_tuid(&tuid_to_hex(&t)), Some(t));
    }

    #[test]
    fn memory_stream_seek_read_write() {
        let s = new_stream(Vec::new());
        unsafe {
            let mut n = 0;
            let data = [1u8, 2, 3, 4];
            s.write(data.as_ptr() as *mut c_void, 4, &mut n);
            assert_eq!(n, 4);
            let mut pos = 0;
            s.seek(1, IBStream_::IStreamSeekMode_::kIBSeekSet as i32, &mut pos);
            let mut out = [0u8; 8];
            s.read(out.as_mut_ptr() as *mut c_void, 8, &mut n);
            assert_eq!(n, 3);
            assert_eq!(&out[..3], &[2, 3, 4]);
        }
    }
}
