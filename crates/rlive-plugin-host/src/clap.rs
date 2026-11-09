//! CLAP plugin hosting.

use crate::{
    bundle_binary, cbuf_to_string, cptr_to_string, wire_note_id, HostError, HostNotification,
    PluginDescriptor, PluginHandle, PluginInstance, PortBuffers, Result,
};
use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::log::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::state::*;
use clap_sys::ext::tail::*;
use clap_sys::ext::thread_check::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use rlive_core::{
    ExprKind, MidiEvent, MidiMessage, MpeEncoder, MpeOutput, MpeZone, NoteId, ParamFlags,
    ParamInfo, ParamKind, ParamOutQueue, PluginFormat, ProcessContext, Processor, Resource, Tail,
    MPE_SLOT,
};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::ThreadId;

// ------------------------------------------------------------------ library loading

struct ClapLibrary {
    entry: *const clap_plugin_entry,
    _lib: libloading::Library,
}

unsafe impl Send for ClapLibrary {}
unsafe impl Sync for ClapLibrary {}

impl Drop for ClapLibrary {
    fn drop(&mut self) {
        unsafe {
            if let Some(deinit) = (*self.entry).deinit {
                deinit();
            }
        }
    }
}

type LibraryCache = Mutex<Vec<(PathBuf, Weak<ClapLibrary>)>>;
static LIBRARIES: LibraryCache = Mutex::new(Vec::new());

/// Load (or reuse) a CLAP library. `clap_entry.init` is called exactly once per load.
fn load_library(path: &Path) -> Result<Arc<ClapLibrary>> {
    let mut cache = LIBRARIES.lock().unwrap();
    cache.retain(|(_, w)| w.strong_count() > 0);
    if let Some(lib) = cache
        .iter()
        .find(|(p, _)| p == path)
        .and_then(|(_, w)| w.upgrade())
    {
        return Ok(lib);
    }
    let binary = bundle_binary(path, "x86_64-linux");
    let lib = unsafe { libloading::Library::new(&binary) }
        .map_err(|e| HostError::Load(binary.clone(), e.to_string()))?;
    let entry: *const clap_plugin_entry = unsafe {
        let sym: libloading::Symbol<*const clap_plugin_entry> = lib
            .get(b"clap_entry\0")
            .map_err(|_| HostError::NoEntry("clap_entry".into()))?;
        *sym
    };
    if entry.is_null() {
        return Err(HostError::NoEntry("clap_entry".into()));
    }
    let path_c = CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();
    let ok = unsafe {
        match (*entry).init {
            Some(init) => init(path_c.as_ptr()),
            None => false,
        }
    };
    if !ok {
        return Err(HostError::Init("clap_entry.init returned false".into()));
    }
    let lib = Arc::new(ClapLibrary { entry, _lib: lib });
    cache.push((path.to_path_buf(), Arc::downgrade(&lib)));
    Ok(lib)
}

fn factory(lib: &ClapLibrary) -> Result<*const clap_plugin_factory> {
    let f = unsafe {
        match (*lib.entry).get_factory {
            Some(get) => get(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory,
            None => std::ptr::null(),
        }
    };
    if f.is_null() {
        Err(HostError::NoEntry("clap.plugin-factory".into()))
    } else {
        Ok(f)
    }
}

unsafe fn has_feature(features: *const *const c_char, wanted: &CStr) -> bool {
    if features.is_null() {
        return false;
    }
    let mut p = features;
    while !(*p).is_null() {
        if CStr::from_ptr(*p) == wanted {
            return true;
        }
        p = p.add(1);
    }
    false
}

/// List the plugins inside a `.clap` file.
pub fn scan(path: &Path) -> Result<Vec<PluginDescriptor>> {
    let lib = load_library(path)?;
    let f = factory(&lib)?;
    let mut out = Vec::new();
    unsafe {
        let count = (*f).get_plugin_count.map_or(0, |c| c(f));
        for i in 0..count {
            let Some(get) = (*f).get_plugin_descriptor else {
                break;
            };
            let d = get(f, i);
            if d.is_null() {
                continue;
            }
            let d = &*d;
            out.push(PluginDescriptor {
                format: PluginFormat::Clap,
                id: cptr_to_string(d.id),
                name: cptr_to_string(d.name),
                vendor: cptr_to_string(d.vendor),
                version: cptr_to_string(d.version),
                path: path.to_path_buf(),
                is_instrument: has_feature(
                    d.features,
                    clap_sys::plugin_features::CLAP_PLUGIN_FEATURE_INSTRUMENT,
                ),
                subcategories: String::new(),
            });
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ host callbacks

struct HostData {
    main_thread: ThreadId,
    callback_requested: AtomicBool,
    restart_requested: AtomicBool,
    params_rescan: AtomicBool,
    latency_changed: AtomicBool,
    tail_changed: AtomicBool,
    gui_closed: AtomicBool,
    resize_request: Mutex<Option<(u32, u32)>>,
}

unsafe fn host_data<'a>(host: *const clap_host) -> &'a HostData {
    &*((*host).host_data as *const HostData)
}

unsafe extern "C" fn host_get_extension(
    _host: *const clap_host,
    id: *const c_char,
) -> *const c_void {
    if id.is_null() {
        return std::ptr::null();
    }
    let id = CStr::from_ptr(id);
    if id == CLAP_EXT_LOG {
        &HOST_LOG as *const _ as *const c_void
    } else if id == CLAP_EXT_THREAD_CHECK {
        &HOST_THREAD_CHECK as *const _ as *const c_void
    } else if id == CLAP_EXT_PARAMS {
        &HOST_PARAMS as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &HOST_STATE as *const _ as *const c_void
    } else if id == CLAP_EXT_LATENCY {
        &HOST_LATENCY as *const _ as *const c_void
    } else if id == CLAP_EXT_TAIL {
        &HOST_TAIL as *const _ as *const c_void
    } else if id == CLAP_EXT_GUI {
        &HOST_GUI as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &HOST_AUDIO_PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS {
        &HOST_NOTE_PORTS as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn host_request_restart(host: *const clap_host) {
    host_data(host)
        .restart_requested
        .store(true, Ordering::Release);
}

unsafe extern "C" fn host_request_process(_host: *const clap_host) {
    // We always process, nothing to do.
}

unsafe extern "C" fn host_request_callback(host: *const clap_host) {
    host_data(host)
        .callback_requested
        .store(true, Ordering::Release);
}

unsafe extern "C" fn host_log(
    _host: *const clap_host,
    severity: clap_log_severity,
    msg: *const c_char,
) {
    let msg = cptr_to_string(msg);
    match severity {
        0 | 1 => log::debug!("[clap] {msg}"),
        2 => log::info!("[clap] {msg}"),
        3 => log::warn!("[clap] {msg}"),
        _ => log::error!("[clap] {msg}"),
    }
}

unsafe extern "C" fn host_is_main_thread(host: *const clap_host) -> bool {
    host_data(host).main_thread == std::thread::current().id()
}

unsafe extern "C" fn host_is_audio_thread(host: *const clap_host) -> bool {
    host_data(host).main_thread != std::thread::current().id()
}

unsafe extern "C" fn host_params_rescan(host: *const clap_host, _flags: clap_param_rescan_flags) {
    host_data(host).params_rescan.store(true, Ordering::Release);
}

unsafe extern "C" fn host_params_clear(
    _host: *const clap_host,
    _id: u32,
    _flags: clap_param_clear_flags,
) {
}

unsafe extern "C" fn host_params_request_flush(_host: *const clap_host) {
    // The engine processes continuously, so parameter changes are flushed by `process`.
}

unsafe extern "C" fn host_state_mark_dirty(_host: *const clap_host) {}

unsafe extern "C" fn host_latency_changed(host: *const clap_host) {
    host_data(host)
        .latency_changed
        .store(true, Ordering::Release);
}

unsafe extern "C" fn host_tail_changed(host: *const clap_host) {
    host_data(host).tail_changed.store(true, Ordering::Release);
}

unsafe extern "C" fn host_gui_resize_hints_changed(_host: *const clap_host) {}

unsafe extern "C" fn host_gui_request_resize(host: *const clap_host, w: u32, h: u32) -> bool {
    *host_data(host).resize_request.lock().unwrap() = Some((w, h));
    true
}

unsafe extern "C" fn host_gui_request_show(_host: *const clap_host) -> bool {
    false
}

unsafe extern "C" fn host_gui_request_hide(_host: *const clap_host) -> bool {
    false
}

unsafe extern "C" fn host_gui_closed(host: *const clap_host, _was_destroyed: bool) {
    host_data(host).gui_closed.store(true, Ordering::Release);
}

unsafe extern "C" fn host_audio_ports_is_rescan_flag_supported(
    _host: *const clap_host,
    _flag: u32,
) -> bool {
    false
}

unsafe extern "C" fn host_audio_ports_rescan(host: *const clap_host, _flags: u32) {
    host_data(host)
        .restart_requested
        .store(true, Ordering::Release);
}

unsafe extern "C" fn host_note_ports_supported_dialects(
    _host: *const clap_host,
) -> clap_note_dialect {
    CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI | CLAP_NOTE_DIALECT_MIDI_MPE
}

unsafe extern "C" fn host_note_ports_rescan(_host: *const clap_host, _flags: u32) {}

static HOST_LOG: clap_host_log = clap_host_log {
    log: Some(host_log),
};
static HOST_THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(host_is_main_thread),
    is_audio_thread: Some(host_is_audio_thread),
};
static HOST_PARAMS: clap_host_params = clap_host_params {
    rescan: Some(host_params_rescan),
    clear: Some(host_params_clear),
    request_flush: Some(host_params_request_flush),
};
static HOST_STATE: clap_host_state = clap_host_state {
    mark_dirty: Some(host_state_mark_dirty),
};
static HOST_LATENCY: clap_host_latency = clap_host_latency {
    changed: Some(host_latency_changed),
};
static HOST_TAIL: clap_host_tail = clap_host_tail {
    changed: Some(host_tail_changed),
};
static HOST_GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(host_gui_resize_hints_changed),
    request_resize: Some(host_gui_request_resize),
    request_show: Some(host_gui_request_show),
    request_hide: Some(host_gui_request_hide),
    closed: Some(host_gui_closed),
};
static HOST_AUDIO_PORTS: clap_host_audio_ports = clap_host_audio_ports {
    is_rescan_flag_supported: Some(host_audio_ports_is_rescan_flag_supported),
    rescan: Some(host_audio_ports_rescan),
};
static HOST_NOTE_PORTS: clap_host_note_ports = clap_host_note_ports {
    supported_dialects: Some(host_note_ports_supported_dialects),
    rescan: Some(host_note_ports_rescan),
};

const HOST_NAME: &CStr = c"rLive";
const HOST_VENDOR: &CStr = c"rLive";
const HOST_URL: &CStr = c"https://github.com/KevinThibaut89/rLive";
const HOST_VERSION: &CStr = c"0.1.0";

// ------------------------------------------------------------------ instance

struct ClapInstance {
    plugin: *const clap_plugin,
    params: *const clap_plugin_params,
    state: *const clap_plugin_state,
    gui: *const clap_plugin_gui,
    latency: *const clap_plugin_latency,
    tail: *const clap_plugin_tail,
    /// Latency in frames, as last read on the main thread.
    latency_frames: AtomicU32,
    /// Tail in frames, in CLAP's encoding, as last read on the main thread.
    tail_frames: AtomicU32,
    activated: AtomicBool,
    // Kept alive for as long as the plugin exists.
    host: Box<clap_host>,
    host_data: Box<HostData>,
    _lib: Arc<ClapLibrary>,
}

unsafe impl Send for ClapInstance {}
unsafe impl Sync for ClapInstance {}

impl ClapInstance {
    /// Read the latency and tail. Main thread only, while activated.
    fn read_audio_props(&self) {
        let (plugin, latency, tail) = (self.plugin, self.latency, self.tail);
        let frames = if latency.is_null() {
            0
        } else {
            unsafe { (*latency).get.map_or(0, |g| g(plugin)) }
        };
        self.latency_frames.store(frames, Ordering::Relaxed);
        let frames = if tail.is_null() {
            0
        } else {
            unsafe { (*tail).get.map_or(0, |g| g(plugin)) }
        };
        self.tail_frames.store(frames, Ordering::Relaxed);
    }

    unsafe fn ext<T>(plugin: *const clap_plugin, id: &CStr) -> *const T {
        match (*plugin).get_extension {
            Some(get) => get(plugin, id.as_ptr()) as *const T,
            None => std::ptr::null(),
        }
    }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        unsafe {
            if self.activated.swap(false, Ordering::AcqRel) {
                if let Some(deactivate) = (*self.plugin).deactivate {
                    deactivate(self.plugin);
                }
            }
            if let Some(destroy) = (*self.plugin).destroy {
                destroy(self.plugin);
            }
        }
        let _ = &self.host;
        let _ = &self.host_data;
    }
}

fn port_channels(plugin: *const clap_plugin, is_input: bool) -> Vec<usize> {
    unsafe {
        let ext: *const clap_plugin_audio_ports = ClapInstance::ext(plugin, CLAP_EXT_AUDIO_PORTS);
        if ext.is_null() {
            // No audio-ports extension: assume a stereo output and no input.
            return if is_input { vec![] } else { vec![2] };
        }
        let count = (*ext).count.map_or(0, |c| c(plugin, is_input));
        (0..count)
            .map(|i| {
                let mut info: clap_audio_port_info = std::mem::zeroed();
                match (*ext).get {
                    Some(get) if get(plugin, i, is_input, &mut info) => info.channel_count as usize,
                    _ => 2,
                }
            })
            .collect()
    }
}

/// Whether the plugin wants notes as raw MIDI rather than CLAP note events.
fn prefers_midi(plugin: *const clap_plugin) -> bool {
    unsafe {
        let ext: *const clap_plugin_note_ports = ClapInstance::ext(plugin, CLAP_EXT_NOTE_PORTS);
        if ext.is_null() {
            return false;
        }
        let count = (*ext).count.map_or(0, |c| c(plugin, true));
        if count == 0 {
            return false;
        }
        let mut info: clap_note_port_info = std::mem::zeroed();
        match (*ext).get {
            Some(get) if get(plugin, 0, true, &mut info) => {
                info.supported_dialects & CLAP_NOTE_DIALECT_CLAP == 0
            }
            _ => false,
        }
    }
}

pub fn instantiate(
    desc: &PluginDescriptor,
    sample_rate: f64,
    max_block: usize,
) -> Result<PluginInstance> {
    let lib = load_library(&desc.path)?;
    let f = factory(&lib)?;
    let host_data = Box::new(HostData {
        main_thread: std::thread::current().id(),
        callback_requested: AtomicBool::new(false),
        restart_requested: AtomicBool::new(false),
        params_rescan: AtomicBool::new(false),
        latency_changed: AtomicBool::new(false),
        tail_changed: AtomicBool::new(false),
        gui_closed: AtomicBool::new(false),
        resize_request: Mutex::new(None),
    });
    let host = Box::new(clap_host {
        clap_version: CLAP_VERSION,
        host_data: &*host_data as *const HostData as *mut c_void,
        name: HOST_NAME.as_ptr(),
        vendor: HOST_VENDOR.as_ptr(),
        url: HOST_URL.as_ptr(),
        version: HOST_VERSION.as_ptr(),
        get_extension: Some(host_get_extension),
        request_restart: Some(host_request_restart),
        request_process: Some(host_request_process),
        request_callback: Some(host_request_callback),
    });
    let id =
        CString::new(desc.id.as_bytes()).map_err(|_| HostError::Init("bad plugin id".into()))?;
    let plugin = unsafe {
        match (*f).create_plugin {
            Some(create) => create(f, &*host, id.as_ptr()),
            None => std::ptr::null(),
        }
    };
    if plugin.is_null() {
        return Err(HostError::NotFound(desc.id.clone(), desc.path.clone()));
    }
    let ok = unsafe { (*plugin).init.is_some_and(|init| init(plugin)) };
    if !ok {
        unsafe {
            if let Some(destroy) = (*plugin).destroy {
                destroy(plugin);
            }
        }
        return Err(HostError::Init(format!("{} init() failed", desc.name)));
    }

    let inst = unsafe {
        Arc::new(ClapInstance {
            plugin,
            params: ClapInstance::ext(plugin, CLAP_EXT_PARAMS),
            state: ClapInstance::ext(plugin, CLAP_EXT_STATE),
            gui: ClapInstance::ext(plugin, CLAP_EXT_GUI),
            latency: ClapInstance::ext(plugin, CLAP_EXT_LATENCY),
            tail: ClapInstance::ext(plugin, CLAP_EXT_TAIL),
            latency_frames: AtomicU32::new(0),
            tail_frames: AtomicU32::new(0),
            activated: AtomicBool::new(false),
            host,
            host_data,
            _lib: lib,
        })
    };

    let inputs = port_channels(plugin, true);
    let outputs = port_channels(plugin, false);
    let midi_notes = prefers_midi(plugin);

    let activated = unsafe {
        (*plugin)
            .activate
            .is_some_and(|a| a(plugin, sample_rate, 1, max_block as u32))
    };
    if !activated {
        return Err(HostError::Activate(desc.name.clone()));
    }
    inst.activated.store(true, Ordering::Release);
    inst.read_audio_props();

    let processor = ClapProcessor::new(
        inst.clone(),
        &inputs,
        &outputs,
        max_block,
        sample_rate,
        midi_notes,
    );
    let mut handle = ClapHandle {
        inst,
        has_sidechain: inputs.len() > 1,
        aux_count: outputs.len().saturating_sub(1),
        desc: desc.clone(),
        params: Vec::new(),
        editor: None,
    };
    handle.rescan_params();
    Ok(PluginInstance {
        handle: Box::new(handle),
        processor: Box::new(processor),
    })
}

// ------------------------------------------------------------------ main-thread handle

enum Editor {
    #[cfg(target_os = "macos")]
    Embedded(crate::mac::EditorWindow),
    Floating,
}

pub struct ClapHandle {
    inst: Arc<ClapInstance>,
    has_sidechain: bool,
    aux_count: usize,
    desc: PluginDescriptor,
    params: Vec<ParamInfo>,
    editor: Option<Editor>,
}

impl ClapHandle {
    fn rescan_params(&mut self) {
        self.params.clear();
        let ext = self.inst.params;
        if ext.is_null() {
            return;
        }
        unsafe {
            let count = (*ext).count.map_or(0, |c| c(self.inst.plugin));
            for i in 0..count {
                let mut info: clap_param_info = std::mem::zeroed();
                let Some(get) = (*ext).get_info else { break };
                if !get(self.inst.plugin, i, &mut info) {
                    continue;
                }
                let hidden = info.flags & CLAP_PARAM_IS_HIDDEN != 0;
                let kind = if info.flags & CLAP_PARAM_IS_STEPPED != 0 {
                    let steps = (info.max_value - info.min_value).round() as i64;
                    if (0..=64).contains(&steps) {
                        let labels = (0..=steps)
                            .map(|s| {
                                let v = info.min_value + s as f64;
                                self.format_raw(info.id, v).unwrap_or_else(|| v.to_string())
                            })
                            .collect();
                        ParamKind::Stepped { labels }
                    } else {
                        ParamKind::Continuous { skew: 1.0 }
                    }
                } else {
                    ParamKind::Continuous { skew: 1.0 }
                };
                self.params.push(ParamInfo {
                    id: info.id,
                    name: cbuf_to_string(&info.name),
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                    unit: String::new(),
                    kind,
                    mod_mode: Default::default(),
                    flags: ParamFlags::for_plugin(hidden),
                });
            }
        }
    }

    fn format_raw(&self, id: u32, value: f64) -> Option<String> {
        let ext = self.inst.params;
        if ext.is_null() {
            return None;
        }
        let mut buf = [0 as c_char; 128];
        unsafe {
            let f = (*ext).value_to_text?;
            if f(
                self.inst.plugin,
                id,
                value,
                buf.as_mut_ptr(),
                buf.len() as u32,
            ) {
                Some(cbuf_to_string(&buf))
            } else {
                None
            }
        }
    }

    fn gui_supported(&self, floating: bool) -> Option<&'static CStr> {
        let gui = self.inst.gui;
        if gui.is_null() {
            return None;
        }
        let api = if cfg!(target_os = "macos") {
            CLAP_WINDOW_API_COCOA
        } else if cfg!(target_os = "windows") {
            CLAP_WINDOW_API_WIN32
        } else {
            CLAP_WINDOW_API_X11
        };
        unsafe {
            let sup = (*gui).is_api_supported?;
            sup(self.inst.plugin, api.as_ptr(), floating).then_some(api)
        }
    }

    fn destroy_gui(&mut self) {
        let gui = self.inst.gui;
        if gui.is_null() {
            return;
        }
        unsafe {
            if let Some(hide) = (*gui).hide {
                hide(self.inst.plugin);
            }
            if let Some(destroy) = (*gui).destroy {
                destroy(self.inst.plugin);
            }
        }
    }
}

impl PluginHandle for ClapHandle {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.desc
    }

    fn sidechain_input(&self) -> bool {
        self.has_sidechain
    }

    fn aux_outputs(&self) -> Vec<String> {
        (0..self.aux_count)
            .map(|n| format!("Output {}", n + 2))
            .collect()
    }

    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }

    fn param_value(&self, id: u32) -> Option<f64> {
        let ext = self.inst.params;
        if ext.is_null() {
            return None;
        }
        let mut v = 0.0;
        unsafe {
            let get = (*ext).get_value?;
            get(self.inst.plugin, id, &mut v).then_some(v)
        }
    }

    fn format_value(&self, id: u32, value: f64) -> Option<String> {
        self.format_raw(id, value)
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let ext = self.inst.state;
        if ext.is_null() {
            return None;
        }
        let mut data: Vec<u8> = Vec::new();
        let stream = clap_ostream {
            ctx: &mut data as *mut Vec<u8> as *mut c_void,
            write: Some(ostream_write),
        };
        unsafe {
            let save = (*ext).save?;
            save(self.inst.plugin, &stream).then_some(data)
        }
    }

    fn load_state(&mut self, data: &[u8]) -> bool {
        let ext = self.inst.state;
        if ext.is_null() {
            return false;
        }
        let mut reader = (data, 0usize);
        let stream = clap_istream {
            ctx: &mut reader as *mut (&[u8], usize) as *mut c_void,
            read: Some(istream_read),
        };
        let ok = unsafe {
            (*ext)
                .load
                .is_some_and(|load| load(self.inst.plugin, &stream))
        };
        if ok {
            self.rescan_params();
        }
        ok
    }

    fn has_editor(&self) -> bool {
        self.gui_supported(false).is_some() || self.gui_supported(true).is_some()
    }

    fn is_editor_open(&self) -> bool {
        self.editor.is_some()
    }

    fn open_editor(&mut self) -> Result<()> {
        if self.editor.is_some() {
            return Ok(());
        }
        let gui = self.inst.gui;
        let plugin = self.inst.plugin;
        if gui.is_null() {
            return Err(HostError::NoEditor);
        }
        self.inst
            .host_data
            .gui_closed
            .store(false, Ordering::Release);

        #[cfg(target_os = "macos")]
        if let Some(api) = self.gui_supported(false) {
            unsafe {
                if !(*gui)
                    .create
                    .is_some_and(|c| c(plugin, api.as_ptr(), false))
                {
                    return Err(HostError::Editor("gui.create failed".into()));
                }
                let (mut w, mut h) = (400u32, 300u32);
                if let Some(get_size) = (*gui).get_size {
                    get_size(plugin, &mut w, &mut h);
                }
                let resizable = (*gui).can_resize.is_some_and(|c| c(plugin));
                let window =
                    crate::mac::EditorWindow::new(&self.desc.name, w as f64, h as f64, resizable)
                        .ok_or_else(|| HostError::Editor("could not create window".into()))?;
                let cw = clap_window {
                    api: api.as_ptr(),
                    specific: clap_window_handle {
                        cocoa: window.ns_view(),
                    },
                };
                if !(*gui).set_parent.is_some_and(|s| s(plugin, &cw)) {
                    if let Some(destroy) = (*gui).destroy {
                        destroy(plugin);
                    }
                    return Err(HostError::Editor("gui.set_parent failed".into()));
                }
                if let Some(show) = (*gui).show {
                    show(plugin);
                }
                window.show();
                self.editor = Some(Editor::Embedded(window));
                return Ok(());
            }
        }

        let api = self.gui_supported(true).ok_or(HostError::NoEditor)?;
        unsafe {
            if !(*gui).create.is_some_and(|c| c(plugin, api.as_ptr(), true)) {
                return Err(HostError::Editor("gui.create (floating) failed".into()));
            }
            let title = CString::new(self.desc.name.as_bytes()).unwrap_or_default();
            if let Some(suggest) = (*gui).suggest_title {
                suggest(plugin, title.as_ptr());
            }
            if let Some(show) = (*gui).show {
                show(plugin);
            }
        }
        self.editor = Some(Editor::Floating);
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(editor) = self.editor.take() {
            self.destroy_gui();
            match editor {
                #[cfg(target_os = "macos")]
                Editor::Embedded(w) => w.close(),
                Editor::Floating => {}
            }
        }
    }

    fn idle(&mut self) -> Vec<HostNotification> {
        let mut out = Vec::new();
        let inst = self.inst.clone();
        let hd = &inst.host_data;
        if hd.callback_requested.swap(false, Ordering::AcqRel) {
            unsafe {
                if let Some(cb) = (*self.inst.plugin).on_main_thread {
                    cb(self.inst.plugin);
                }
            }
        }
        if hd.params_rescan.swap(false, Ordering::AcqRel) {
            self.rescan_params();
            out.push(HostNotification::ParamsRescanned);
        }
        if hd.restart_requested.swap(false, Ordering::AcqRel) {
            out.push(HostNotification::RestartRequested);
        }
        let latency_changed = hd.latency_changed.swap(false, Ordering::AcqRel);
        if latency_changed || hd.tail_changed.swap(false, Ordering::AcqRel) {
            self.inst.read_audio_props();
        }
        if latency_changed {
            out.push(HostNotification::LatencyChanged);
        }
        let resize = hd.resize_request.lock().unwrap().take();
        #[cfg(target_os = "macos")]
        if let (Some((w, h)), Some(Editor::Embedded(win))) = (resize, &self.editor) {
            win.resize(w as f64, h as f64);
            unsafe {
                if let Some(set_size) = (*self.inst.gui).set_size {
                    set_size(self.inst.plugin, w, h);
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = resize;
        #[cfg(target_os = "macos")]
        let window_closed = matches!(&self.editor, Some(Editor::Embedded(w)) if !w.is_visible());
        #[cfg(not(target_os = "macos"))]
        let window_closed = false;
        if hd.gui_closed.swap(false, Ordering::AcqRel) || window_closed {
            self.close_editor();
            out.push(HostNotification::EditorClosed);
        }
        out
    }

    fn latency(&self) -> u32 {
        self.inst.read_audio_props();
        self.inst.latency_frames.load(Ordering::Relaxed)
    }
}

impl Drop for ClapHandle {
    fn drop(&mut self) {
        self.close_editor();
    }
}

unsafe extern "C" fn ostream_write(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    let data = &mut *((*stream).ctx as *mut Vec<u8>);
    let bytes = std::slice::from_raw_parts(buffer as *const u8, size as usize);
    data.extend_from_slice(bytes);
    size as i64
}

unsafe extern "C" fn istream_read(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    let (data, pos) = &mut *((*stream).ctx as *mut (&[u8], usize));
    let n = (data.len() - *pos).min(size as usize);
    std::ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buffer as *mut u8, n);
    *pos += n;
    n as i64
}

// ------------------------------------------------------------------ audio-thread processor

#[repr(C)]
#[derive(Clone, Copy)]
union ClapEvent {
    header: clap_event_header,
    note: clap_event_note,
    param: clap_event_param_value,
    midi: clap_event_midi,
    expression: clap_event_note_expression,
}

const MAX_EVENTS: usize = 1024;

pub struct ClapProcessor {
    inst: Arc<ClapInstance>,
    started: bool,
    inputs: Vec<PortBuffers>,
    outputs: Vec<PortBuffers>,
    in_bufs: Vec<clap_audio_buffer>,
    out_bufs: Vec<clap_audio_buffer>,
    events: Vec<ClapEvent>,
    pending_params: Vec<(u32, f64)>,
    transport: clap_event_transport,
    steady_time: i64,
    is_instrument: bool,
    midi_notes: bool,
    /// Carries note expression to plugins that take notes as MIDI.
    mpe: MpeEncoder,
    /// Reused each block for what `mpe` produces.
    encoded: Vec<MidiEvent>,
    max_block: usize,
    sample_rate: f64,
}

// Raw pointers in the buffers point into heap memory owned by this struct.
unsafe impl Send for ClapProcessor {}

impl ClapProcessor {
    fn new(
        inst: Arc<ClapInstance>,
        inputs: &[usize],
        outputs: &[usize],
        max_block: usize,
        sample_rate: f64,
        midi_notes: bool,
    ) -> Self {
        let mut inputs: Vec<PortBuffers> = inputs
            .iter()
            .map(|&c| PortBuffers::new(c, max_block))
            .collect();
        let mut outputs: Vec<PortBuffers> = outputs
            .iter()
            .map(|&c| PortBuffers::new(c, max_block))
            .collect();
        let mk = |p: &mut PortBuffers| clap_audio_buffer {
            data32: p.ptrs.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: p.count() as u32,
            latency: 0,
            constant_mask: 0,
        };
        let in_bufs = inputs.iter_mut().map(mk).collect();
        let out_bufs = outputs.iter_mut().map(mk).collect();
        Self {
            is_instrument: inputs.is_empty(),
            inst,
            started: false,
            inputs,
            outputs,
            in_bufs,
            out_bufs,
            events: Vec::with_capacity(MAX_EVENTS),
            pending_params: Vec::with_capacity(256),
            transport: unsafe { std::mem::zeroed() },
            steady_time: 0,
            midi_notes,
            mpe: MpeEncoder::new(MpeZone::Lower, 15),
            encoded: Vec::with_capacity(MAX_EVENTS),
            max_block,
            sample_rate,
        }
    }

    fn push(&mut self, e: ClapEvent) {
        if self.events.len() < self.events.capacity() {
            self.events.push(e);
        }
    }

    fn header<T>(time: u32, type_: u16) -> clap_event_header {
        clap_event_header {
            size: std::mem::size_of::<T>() as u32,
            time,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_,
            flags: 0,
        }
    }

    /// Queue a parameter change. If this block's event list is full it goes to the next block,
    /// because the value must reach the plugin even if its timing can't.
    fn push_param(&mut self, time: u32, id: u32, value: f64) {
        if self.events.len() < self.events.capacity() {
            self.push(ClapEvent {
                param: clap_event_param_value {
                    header: Self::header::<clap_event_param_value>(time, CLAP_EVENT_PARAM_VALUE),
                    param_id: id,
                    cookie: std::ptr::null_mut(),
                    note_id: -1,
                    port_index: -1,
                    channel: -1,
                    key: -1,
                    value,
                },
            });
        } else {
            self.defer_param(id, value);
        }
    }

    fn defer_param(&mut self, id: u32, value: f64) {
        if let Some(e) = self.pending_params.iter_mut().find(|(i, _)| *i == id) {
            e.1 = value;
        } else if self.pending_params.len() < self.pending_params.capacity() {
            self.pending_params.push((id, value));
        }
    }

    fn push_note(&mut self, time: u32, event: &MidiEvent) {
        match event.message {
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
                note_id,
                tuning,
            } if !self.midi_notes => {
                self.push(ClapEvent {
                    note: clap_event_note {
                        header: Self::header::<clap_event_note>(time, CLAP_EVENT_NOTE_ON),
                        note_id: wire_note_id(note_id),
                        port_index: 0,
                        channel: channel as i16,
                        key: key as i16,
                        velocity: velocity as f64,
                    },
                });
                if tuning != 0.0 {
                    self.push_expression(
                        time,
                        note_id,
                        channel,
                        key,
                        CLAP_NOTE_EXPRESSION_TUNING,
                        tuning,
                    );
                }
            }
            MidiMessage::NoteOff {
                channel,
                key,
                velocity,
                note_id,
            } if !self.midi_notes => self.push(ClapEvent {
                note: clap_event_note {
                    header: Self::header::<clap_event_note>(time, CLAP_EVENT_NOTE_OFF),
                    note_id: wire_note_id(note_id),
                    port_index: 0,
                    channel: channel as i16,
                    key: key as i16,
                    velocity: velocity as f64,
                },
            }),
            MidiMessage::NoteExpr {
                channel,
                key,
                note_id,
                kind,
                value,
            } if !self.midi_notes => {
                let expression = match kind {
                    ExprKind::Pitch => CLAP_NOTE_EXPRESSION_TUNING,
                    ExprKind::Slide => CLAP_NOTE_EXPRESSION_BRIGHTNESS,
                    ExprKind::Pressure => CLAP_NOTE_EXPRESSION_PRESSURE,
                };
                self.push_expression(time, note_id, channel, key, expression, value);
            }
            ref m => self.push_midi(time, m),
        }
    }

    fn push_expression(
        &mut self,
        time: u32,
        note_id: NoteId,
        channel: u8,
        key: u8,
        expression: clap_note_expression,
        value: f32,
    ) {
        self.push(ClapEvent {
            expression: clap_event_note_expression {
                header: Self::header::<clap_event_note_expression>(
                    time,
                    CLAP_EVENT_NOTE_EXPRESSION,
                ),
                expression_id: expression,
                note_id: wire_note_id(note_id),
                port_index: 0,
                channel: channel as i16,
                key: key as i16,
                value: value as f64,
            },
        });
    }

    fn push_midi(&mut self, time: u32, msg: &MidiMessage) {
        let (bytes, _) = msg.to_bytes();
        self.push(ClapEvent {
            midi: clap_event_midi {
                header: Self::header::<clap_event_midi>(time, CLAP_EVENT_MIDI),
                port_index: 0,
                data: bytes,
            },
        });
    }
}

unsafe extern "C" fn in_events_size(list: *const clap_input_events) -> u32 {
    (*((*list).ctx as *const Vec<ClapEvent>)).len() as u32
}

unsafe extern "C" fn in_events_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    let v = &*((*list).ctx as *const Vec<ClapEvent>);
    match v.get(index as usize) {
        Some(e) => &e.header,
        None => std::ptr::null(),
    }
}

unsafe extern "C" fn out_events_try_push(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    let q = &mut *((*list).ctx as *mut ParamOutQueue);
    if (*event).space_id == CLAP_CORE_EVENT_SPACE_ID && (*event).type_ == CLAP_EVENT_PARAM_VALUE {
        let e = &*(event as *const clap_event_param_value);
        q.push(e.param_id, e.value);
    }
    true
}

impl Processor for ClapProcessor {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        // Activation happens at instantiation; a different configuration requires a new instance.
        debug_assert!(sample_rate == self.sample_rate && max_block <= self.max_block);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let plugin = self.inst.plugin;
        let frames = ctx.audio.frames().min(self.max_block);
        if !self.started {
            self.started = unsafe { (*plugin).start_processing.is_none_or(|s| s(plugin)) };
            if !self.started {
                return;
            }
        }

        self.events.clear();
        // Never fuller than the list, so nothing here gets deferred.
        for i in 0..self.pending_params.len() {
            let (id, value) = self.pending_params[i];
            self.push_param(0, id, value);
        }
        self.pending_params.clear();
        let last = frames.saturating_sub(1) as u32;
        // Plugins that take MIDI can't receive CLAP note expression, so it becomes MPE.
        let mut encoded = std::mem::take(&mut self.encoded);
        if self.midi_notes {
            self.mpe.encode(ctx.events, &mut |e| {
                if encoded.len() < encoded.capacity() {
                    encoded.push(e);
                }
            });
        }
        let source = if self.midi_notes {
            &encoded
        } else {
            ctx.events
        };
        let (mut next_param, mut next_event) = (0, 0);
        // CLAP needs events in time order, so merge the parameter and MIDI streams.
        loop {
            match (ctx.params.get(next_param), source.get(next_event)) {
                (Some(p), e) if e.is_none_or(|e| p.offset <= e.offset) => {
                    self.push_param(p.offset.min(last), p.id.0, p.value);
                    next_param += 1;
                }
                (_, Some(e)) => {
                    self.push_note(e.offset.min(last), e);
                    next_event += 1;
                }
                _ => break,
            }
        }
        encoded.clear();
        self.encoded = encoded;

        let t = ctx.transport;
        let beats = t.beats();
        let seconds = t.sample_pos as f64 / t.sample_rate;
        let ppq = rlive_core::PPQ as f64;
        let bar_beats = t.time_signature.ticks_per_bar() as f64 / ppq;
        let bar = t.bar_start / ppq / bar_beats;
        let (loop_start, loop_end) = t.loop_range.unwrap_or((0.0, 0.0));
        let to_seconds = |ticks: f64| ticks / t.ticks_per_sample() / t.sample_rate;
        self.transport = clap_event_transport {
            header: Self::header::<clap_event_transport>(0, CLAP_EVENT_TRANSPORT),
            flags: CLAP_TRANSPORT_HAS_TEMPO
                | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
                | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
                | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
                | if t.playing {
                    CLAP_TRANSPORT_IS_PLAYING
                } else {
                    0
                }
                | if t.loop_range.is_some() {
                    CLAP_TRANSPORT_IS_LOOP_ACTIVE
                } else {
                    0
                },
            song_pos_beats: (beats * CLAP_BEATTIME_FACTOR as f64) as i64,
            song_pos_seconds: (seconds * CLAP_SECTIME_FACTOR as f64) as i64,
            tempo: t.tempo,
            tempo_inc: 0.0,
            loop_start_beats: (loop_start / ppq * CLAP_BEATTIME_FACTOR as f64) as i64,
            loop_end_beats: (loop_end / ppq * CLAP_BEATTIME_FACTOR as f64) as i64,
            loop_start_seconds: (to_seconds(loop_start) * CLAP_SECTIME_FACTOR as f64) as i64,
            loop_end_seconds: (to_seconds(loop_end) * CLAP_SECTIME_FACTOR as f64) as i64,
            bar_start: (t.bar_start / ppq * CLAP_BEATTIME_FACTOR as f64) as i64,
            bar_number: bar as i32,
            tsig_num: t.time_signature.numerator as u16,
            tsig_denom: t.time_signature.denominator as u16,
        };

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

        let in_list = clap_input_events {
            ctx: &self.events as *const Vec<ClapEvent> as *mut c_void,
            size: Some(in_events_size),
            get: Some(in_events_get),
        };
        let out_list = clap_output_events {
            ctx: &mut *ctx.param_out as *mut ParamOutQueue as *mut c_void,
            try_push: Some(out_events_try_push),
        };
        let process = clap_process {
            steady_time: self.steady_time,
            frames_count: frames as u32,
            transport: &self.transport,
            audio_inputs: self.in_bufs.as_ptr(),
            audio_outputs: self.out_bufs.as_mut_ptr(),
            audio_inputs_count: self.in_bufs.len() as u32,
            audio_outputs_count: self.out_bufs.len() as u32,
            in_events: &in_list,
            out_events: &out_list,
        };
        unsafe {
            if let Some(p) = (*plugin).process {
                p(plugin, &process);
            }
        }
        self.steady_time += frames as i64;
        if let Some(main) = self.outputs.first() {
            main.store_stereo(left, right, self.is_instrument);
        }
        for (port, aux) in self.outputs.iter().skip(1).zip(ctx.aux_out.iter_mut()) {
            let n = frames.min(aux.frames());
            port.store_stereo(&mut aux.left[..n], &mut aux.right[..n], false);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.defer_param(id, value);
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot != MPE_SLOT {
            return Some(resource);
        }
        if let Some(output) = resource.downcast_ref::<Option<MpeOutput>>() {
            self.mpe = match output {
                Some(o) => MpeEncoder::configured(*o),
                None => MpeEncoder::new(MpeZone::Lower, 15),
            };
        }
        Some(resource)
    }

    fn stop_processing(&mut self) {
        if self.started {
            unsafe {
                if let Some(stop) = (*self.inst.plugin).stop_processing {
                    stop(self.inst.plugin);
                }
            }
            self.started = false;
        }
    }

    fn reset(&mut self) {
        unsafe {
            if let Some(reset) = (*self.inst.plugin).reset {
                reset(self.inst.plugin);
            }
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

impl Drop for ClapProcessor {
    fn drop(&mut self) {
        // Normally the engine already called `stop_processing` on the audio thread.
        self.stop_processing();
    }
}
