//! Two tiny CLAP plugins used by rLive's hosting tests:
//!
//! * `rs.rlive.test-gain` — stereo effect with one "Gain" parameter (0..2) and state save/load.
//! * `rs.rlive.test-sine` — instrument that plays a sine for each held note.

#![allow(clippy::missing_safety_doc)]

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::state::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::host::clap_host;
use clap_sys::id::CLAP_INVALID_ID;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::*;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use std::ffi::{c_char, c_void, CStr};
use std::ptr::null;

struct Sync<T>(T);
unsafe impl<T> std::marker::Sync for Sync<T> {}

static GAIN_FEATURES: Sync<[*const c_char; 3]> =
    Sync([c"audio-effect".as_ptr(), c"stereo".as_ptr(), null()]);
static SINE_FEATURES: Sync<[*const c_char; 3]> =
    Sync([c"instrument".as_ptr(), c"synthesizer".as_ptr(), null()]);

static GAIN_DESC: Sync<clap_plugin_descriptor> = Sync(clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: c"rs.rlive.test-gain".as_ptr(),
    name: c"rLive Test Gain".as_ptr(),
    vendor: c"rLive".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"1.0.0".as_ptr(),
    description: c"Test gain".as_ptr(),
    features: GAIN_FEATURES.0.as_ptr(),
});

static SINE_DESC: Sync<clap_plugin_descriptor> = Sync(clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: c"rs.rlive.test-sine".as_ptr(),
    name: c"rLive Test Sine".as_ptr(),
    vendor: c"rLive".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"1.0.0".as_ptr(),
    description: c"Test sine instrument".as_ptr(),
    features: SINE_FEATURES.0.as_ptr(),
});

struct Plugin {
    clap: clap_plugin,
    instrument: bool,
    /// Gain (effect) or volume (instrument).
    param: f64,
    sample_rate: f64,
    phase: f64,
    note: Option<i16>,
}

unsafe fn this<'a>(p: *const clap_plugin) -> &'a mut Plugin {
    &mut *((*p).plugin_data as *mut Plugin)
}

unsafe extern "C" fn init(_p: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn destroy(p: *const clap_plugin) {
    drop(Box::from_raw((*p).plugin_data as *mut Plugin));
}

unsafe extern "C" fn activate(p: *const clap_plugin, sr: f64, _min: u32, _max: u32) -> bool {
    this(p).sample_rate = sr;
    true
}

unsafe extern "C" fn deactivate(_p: *const clap_plugin) {}
unsafe extern "C" fn start_processing(_p: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn stop_processing(_p: *const clap_plugin) {}
unsafe extern "C" fn reset(p: *const clap_plugin) {
    this(p).note = None;
}

unsafe fn handle_event(
    plugin: &mut Plugin,
    e: *const clap_event_header,
    out: *const clap_output_events,
) {
    if (*e).space_id != CLAP_CORE_EVENT_SPACE_ID {
        return;
    }
    match (*e).type_ {
        CLAP_EVENT_PARAM_VALUE => {
            let pv = &*(e as *const clap_event_param_value);
            if pv.param_id == 0 {
                plugin.param = pv.value.clamp(0.0, 2.0);
                // Echo back as an output event so the host sees plugin-originated changes.
                if !out.is_null() && pv.value > 1.5 {
                    let echo = clap_event_param_value { value: 1.5, ..*pv };
                    if let Some(push) = (*out).try_push {
                        push(out, &echo.header);
                    }
                    plugin.param = 1.5;
                }
            }
        }
        CLAP_EVENT_NOTE_ON => plugin.note = Some((*(e as *const clap_event_note)).key),
        CLAP_EVENT_NOTE_OFF => {
            let key = (*(e as *const clap_event_note)).key;
            if plugin.note == Some(key) {
                plugin.note = None;
            }
        }
        _ => {}
    }
}

unsafe extern "C" fn process(
    p: *const clap_plugin,
    proc_: *const clap_process,
) -> clap_process_status {
    let plugin = this(p);
    let proc_ = &*proc_;
    let n = proc_.frames_count as usize;
    let in_ev = proc_.in_events;
    let count = (*in_ev).size.map_or(0, |s| s(in_ev));
    let mut next_ev = 0;
    let out_buf: &clap_audio_buffer = &*proc_.audio_outputs;
    let out_l = std::slice::from_raw_parts_mut(*out_buf.data32, n);
    let out_r = std::slice::from_raw_parts_mut(*out_buf.data32.add(1), n);
    for i in 0..n {
        while next_ev < count {
            let e = (*in_ev).get.unwrap()(in_ev, next_ev);
            if (*e).time as usize > i {
                break;
            }
            handle_event(plugin, e, proc_.out_events);
            next_ev += 1;
        }
        if plugin.instrument {
            let s = match plugin.note {
                Some(key) => {
                    let hz = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
                    plugin.phase = (plugin.phase + hz / plugin.sample_rate).fract();
                    (plugin.phase * std::f64::consts::TAU).sin() * plugin.param * 0.5
                }
                None => 0.0,
            };
            out_l[i] = s as f32;
            out_r[i] = s as f32;
        } else {
            let inp: &clap_audio_buffer = &*proc_.audio_inputs;
            let l = *(*inp.data32).add(i);
            let r = *(*inp.data32.add(1)).add(i);
            out_l[i] = l * plugin.param as f32;
            out_r[i] = r * plugin.param as f32;
        }
    }
    while next_ev < count {
        let e = (*in_ev).get.unwrap()(in_ev, next_ev);
        handle_event(plugin, e, proc_.out_events);
        next_ev += 1;
    }
    CLAP_PROCESS_CONTINUE
}

// --- params

unsafe extern "C" fn params_count(_p: *const clap_plugin) -> u32 {
    1
}

unsafe extern "C" fn params_get_info(
    p: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    if index != 0 {
        return false;
    }
    let info = &mut *info;
    info.id = 0;
    info.flags = CLAP_PARAM_IS_AUTOMATABLE;
    info.cookie = std::ptr::null_mut();
    let name: &CStr = if this(p).instrument {
        c"Volume"
    } else {
        c"Gain"
    };
    for (d, s) in info.name.iter_mut().zip(name.to_bytes_with_nul()) {
        *d = *s as c_char;
    }
    info.module[0] = 0;
    info.min_value = 0.0;
    info.max_value = 2.0;
    info.default_value = 1.0;
    true
}

unsafe extern "C" fn params_get_value(p: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    if id != 0 {
        return false;
    }
    *out = this(p).param;
    true
}

unsafe extern "C" fn params_value_to_text(
    _p: *const clap_plugin,
    id: u32,
    value: f64,
    buf: *mut c_char,
    cap: u32,
) -> bool {
    if id != 0 || cap < 16 {
        return false;
    }
    let s = format!("{:.2}x\0", value);
    for (i, b) in s.bytes().enumerate().take(cap as usize) {
        *buf.add(i) = b as c_char;
    }
    true
}

unsafe extern "C" fn params_text_to_value(
    _p: *const clap_plugin,
    _id: u32,
    _t: *const c_char,
    _o: *mut f64,
) -> bool {
    false
}

unsafe extern "C" fn params_flush(
    p: *const clap_plugin,
    in_: *const clap_input_events,
    out: *const clap_output_events,
) {
    let count = (*in_).size.map_or(0, |s| s(in_));
    for i in 0..count {
        handle_event(this(p), (*in_).get.unwrap()(in_, i), out);
    }
}

static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_get_info),
    get_value: Some(params_get_value),
    value_to_text: Some(params_value_to_text),
    text_to_value: Some(params_text_to_value),
    flush: Some(params_flush),
};

// --- state

unsafe extern "C" fn state_save(p: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let bytes = this(p).param.to_le_bytes();
    (*stream).write.unwrap()(stream, bytes.as_ptr() as *const c_void, 8) == 8
}

unsafe extern "C" fn state_load(p: *const clap_plugin, stream: *const clap_istream) -> bool {
    let mut bytes = [0u8; 8];
    if (*stream).read.unwrap()(stream, bytes.as_mut_ptr() as *mut c_void, 8) != 8 {
        return false;
    }
    this(p).param = f64::from_le_bytes(bytes);
    true
}

static STATE: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};

// --- ports

unsafe extern "C" fn audio_ports_count(p: *const clap_plugin, is_input: bool) -> u32 {
    if is_input && this(p).instrument {
        0
    } else {
        1
    }
}

unsafe extern "C" fn audio_ports_get(
    p: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    if index != 0 || (is_input && this(p).instrument) {
        return false;
    }
    let info = &mut *info;
    info.id = 0;
    info.name[0] = 0;
    info.flags = CLAP_AUDIO_PORT_IS_MAIN;
    info.channel_count = 2;
    info.port_type = CLAP_PORT_STEREO.as_ptr();
    info.in_place_pair = CLAP_INVALID_ID;
    true
}

static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(audio_ports_count),
    get: Some(audio_ports_get),
};

unsafe extern "C" fn note_ports_count(p: *const clap_plugin, is_input: bool) -> u32 {
    (is_input && this(p).instrument) as u32
}

unsafe extern "C" fn note_ports_get(
    _p: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if index != 0 || !is_input {
        return false;
    }
    let info = &mut *info;
    info.id = 0;
    info.supported_dialects = CLAP_NOTE_DIALECT_CLAP;
    info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
    info.name[0] = 0;
    true
}

static NOTE_PORTS: clap_plugin_note_ports = clap_plugin_note_ports {
    count: Some(note_ports_count),
    get: Some(note_ports_get),
};

unsafe extern "C" fn get_extension(_p: *const clap_plugin, id: *const c_char) -> *const c_void {
    let id = CStr::from_ptr(id);
    if id == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &STATE as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &AUDIO_PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS {
        &NOTE_PORTS as *const _ as *const c_void
    } else {
        null()
    }
}

unsafe extern "C" fn on_main_thread(_p: *const clap_plugin) {}

// --- factory & entry

unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
    2
}

unsafe extern "C" fn factory_descriptor(
    _f: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    match index {
        0 => &GAIN_DESC.0,
        1 => &SINE_DESC.0,
        _ => null(),
    }
}

unsafe extern "C" fn factory_create(
    _f: *const clap_plugin_factory,
    _host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    let id = CStr::from_ptr(id);
    let (desc, instrument) = if id == CStr::from_ptr(GAIN_DESC.0.id) {
        (&GAIN_DESC.0 as *const _, false)
    } else if id == CStr::from_ptr(SINE_DESC.0.id) {
        (&SINE_DESC.0 as *const _, true)
    } else {
        return null();
    };
    let plugin = Box::into_raw(Box::new(Plugin {
        clap: clap_plugin {
            desc,
            plugin_data: std::ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(deactivate),
            start_processing: Some(start_processing),
            stop_processing: Some(stop_processing),
            reset: Some(reset),
            process: Some(process),
            get_extension: Some(get_extension),
            on_main_thread: Some(on_main_thread),
        },
        instrument,
        param: 1.0,
        sample_rate: 48_000.0,
        phase: 0.0,
        note: None,
    }));
    (*plugin).clap.plugin_data = plugin as *mut c_void;
    &(*plugin).clap
}

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(factory_descriptor),
    create_plugin: Some(factory_create),
};

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}
unsafe extern "C" fn entry_deinit() {}
unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    if CStr::from_ptr(id) == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY as *const _ as *const c_void
    } else {
        null()
    }
}

#[no_mangle]
#[allow(non_upper_case_globals)]
#[used]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};
