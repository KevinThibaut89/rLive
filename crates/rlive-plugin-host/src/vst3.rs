//! VST3 plugin hosting (via the MIT-licensed `vst3` bindings).

use crate::{
    bundle_binary, cbuf_to_string, HostError, HostNotification, PluginDescriptor, PluginHandle,
    PluginInstance, PortBuffers, Result,
};
use rlive_core::{
    MidiMessage, ParamFlags, ParamInfo, ParamKind, PluginFormat, ProcessContext, Processor,
};
use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
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
    fn new(capacity: usize) -> Self {
        let queues: Vec<_> = (0..capacity)
            .map(|_| {
                ComWrapper::new(ParamQueue {
                    id: Cell::new(0),
                    points: UnsafeCell::new(Vec::with_capacity(16)),
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

    fn clear(&self) {
        for q in &self.queues[..self.used.get()] {
            unsafe { (*q.points.get()).clear() };
        }
        self.used.set(0);
    }

    fn add(&self, id: ParamID, offset: i32, value: f64) {
        let mut index = 0;
        let q = unsafe { self.addParameterData(&id, &mut index) };
        if !q.is_null() {
            unsafe { self.queues[index as usize].addPoint(offset, value, std::ptr::null_mut()) };
        }
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

struct Vst3Instance {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    separate_controller: bool,
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
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
        let inst = Arc::new(Vst3Instance {
            component,
            processor,
            controller,
            separate_controller: separate,
            connections,
            _host: host,
            _handler: handler,
            _lib: lib,
        });
        let proc_ = Vst3Processor::new(inst.clone(), &inputs, &outputs, max_block, sample_rate);
        let mut handle = Vst3Handle {
            inst,
            desc: desc.clone(),
            params: Vec::new(),
            handler: handler_state,
            editor: None,
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
    desc: PluginDescriptor,
    params: Vec<ParamInfo>,
    handler: Arc<HandlerState>,
    #[cfg(target_os = "macos")]
    editor: Option<Editor>,
    #[cfg(not(target_os = "macos"))]
    editor: Option<()>,
}

const STATE_MAGIC: &[u8; 4] = b"RLV3";

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
            let window = crate::mac::EditorWindow::new(&self.desc.name, w, h, resizable)
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
        if flags & RestartFlags_::kLatencyChanged != 0 {
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
        unsafe { self.inst.processor.getLatencySamples() }
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
    in_changes_ptr: *mut IParameterChanges,
    out_changes_ptr: *mut IParameterChanges,
    events_ptr: *mut IEventList,
    context: vst3::Steinberg::Vst::ProcessContext,
    is_instrument: bool,
    max_block: usize,
    sample_rate: f64,
}

unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    fn new(
        inst: Arc<Vst3Instance>,
        inputs: &[usize],
        outputs: &[usize],
        max_block: usize,
        sample_rate: f64,
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
        let in_changes = ComWrapper::new(ParamChanges::new(128));
        let out_changes = ComWrapper::new(ParamChanges::new(128));
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
            in_changes_ptr,
            out_changes_ptr,
            events_ptr,
            context: unsafe { std::mem::zeroed() },
            max_block,
            sample_rate,
        }
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
        let events = unsafe { &mut *self.events.events.get() };
        events.clear();
        for e in ctx.events {
            let offset = e.offset.min(frames.saturating_sub(1) as u32) as int32;
            let mut ev: Event = unsafe { std::mem::zeroed() };
            ev.busIndex = 0;
            ev.sampleOffset = offset;
            ev.flags = Event_::EventFlags_::kIsLive as u16;
            match e.message {
                MidiMessage::NoteOn {
                    channel,
                    key,
                    velocity,
                } => {
                    ev.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
                    ev.__field0.noteOn = NoteOnEvent {
                        channel: channel as i16,
                        pitch: key as i16,
                        tuning: 0.0,
                        velocity,
                        length: 0,
                        noteId: -1,
                    };
                }
                MidiMessage::NoteOff {
                    channel,
                    key,
                    velocity,
                } => {
                    ev.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
                    ev.__field0.noteOff = NoteOffEvent {
                        channel: channel as i16,
                        pitch: key as i16,
                        velocity,
                        noteId: -1,
                        tuning: 0.0,
                    };
                }
                // CC/pitch bend go through IMidiMapping parameters in VST3; not mapped yet.
                _ => continue,
            }
            if events.len() < events.capacity() {
                events.push(ev);
            }
        }

        let t = ctx.transport;
        let beats = t.beats();
        let bar_beats = t.time_signature.ticks_per_bar() as f64 / rlive_core::PPQ as f64;
        use ProcessContext_::StatesAndFlags_ as F;
        self.context.state =
            (F::kTempoValid | F::kTimeSigValid | F::kProjectTimeMusicValid | F::kBarPositionValid)
                | if t.playing { F::kPlaying } else { 0 };
        self.context.sampleRate = t.sample_rate;
        self.context.projectTimeSamples = t.sample_pos as i64;
        self.context.projectTimeMusic = beats;
        self.context.barPositionMusic = (beats / bar_beats).floor() * bar_beats;
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
            outputEvents: std::ptr::null_mut(),
            processContext: &mut self.context,
        };
        unsafe { self.inst.processor.process(&mut data) };
        self.in_changes.clear();

        for i in 0..self.out_changes.used.get() {
            let q = &self.out_changes.queues[i];
            if let Some(&(_, v)) = unsafe { (&*q.points.get()).last() } {
                ctx.param_out.push(q.id.get(), v);
            }
        }
        if let Some(main) = self.outputs.first() {
            main.store_stereo(left, right, self.is_instrument);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.in_changes.add(id, 0, value);
    }

    fn stop_processing(&mut self) {
        if self.started {
            unsafe { self.inst.processor.setProcessing(0) };
            self.started = false;
        }
    }

    fn latency(&self) -> u32 {
        0
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
