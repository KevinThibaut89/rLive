//! Making the engine's worker threads real-time, alongside the audio device's own thread.
//!
//! On macOS each worker asks for time-constraint scheduling and then joins the Core Audio device's
//! audio workgroup, which is what keeps it co-scheduled with the device's I/O thread on a
//! performance core. When there's no workgroup to join (an older system, or no real device) the
//! time-constraint policy alone still makes it a real-time thread.
//!
//! Elsewhere workers run at the priority they were born with. That is a known gap, for the day
//! those platforms are real targets: it only costs headroom, as a callback that finds them asleep
//! does their work itself.

use rlive_engine::WorkerHooks;
use std::sync::Arc;

/// How many threads to run the engine on, the audio callback included, for a choice of `0` (Auto)
/// or an explicit count: the performance cores, at most eight.
pub fn resolve_thread_count(setting: u32) -> usize {
    if setting > 0 {
        return setting as usize;
    }
    let cores = performance_cores().unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    });
    cores.clamp(1, 8)
}

/// Hooks that put a worker where it needs to be, for a device at this sample rate.
pub fn worker_hooks(device: Option<&str>, sample_rate: f64) -> WorkerHooks {
    let shared = Arc::new(imp::RealtimeWorkers::new(device, sample_rate));
    let on_stop = shared.clone();
    WorkerHooks {
        start: Arc::new(move |worker| shared.start(worker)),
        stop: Some(Arc::new(move |worker| on_stop.stop(worker))),
    }
}

#[cfg(target_os = "macos")]
fn performance_cores() -> Option<usize> {
    extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            oldp: *mut std::ffi::c_void,
            oldlenp: *mut usize,
            newp: *mut std::ffi::c_void,
            newlen: usize,
        ) -> std::ffi::c_int;
    }
    let mut count: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    // SAFETY: the name is a valid C string and `count`/`len` are valid for writing.
    let result = unsafe {
        sysctlbyname(
            c"hw.perflevel0.physicalcpu".as_ptr(),
            &mut count as *mut i32 as *mut std::ffi::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && count > 0).then_some(count as usize)
}

#[cfg(not(target_os = "macos"))]
fn performance_cores() -> Option<usize> {
    None
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::UnsafeCell;
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::ptr::null;

    type OSStatus = i32;
    type AudioObjectId = u32;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const SYSTEM_OBJECT: AudioObjectId = 1;
    const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    const ELEMENT_MAIN: u32 = 0;
    const PROP_DEVICES: u32 = u32::from_be_bytes(*b"dev#");
    const PROP_NAME: u32 = u32::from_be_bytes(*b"lnam");
    const PROP_DEFAULT_OUTPUT: u32 = u32::from_be_bytes(*b"dOut");
    const PROP_WORKGROUP: u32 = u32::from_be_bytes(*b"oswg");
    const PROP_BUFFER_FRAMES: u32 = u32::from_be_bytes(*b"fsiz");

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(
            object: AudioObjectId,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> OSStatus;
        fn AudioObjectGetPropertyDataSize(
            object: AudioObjectId,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> OSStatus;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    extern "C" {
        fn mach_thread_self() -> u32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
        static mach_task_self_: u32;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
        fn thread_policy_set(thread: u32, flavor: u32, policy: *const c_int, count: u32) -> i32;
    }

    #[repr(C)]
    #[derive(Default)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;

    #[repr(C)]
    struct TimeConstraint {
        period: u32,
        computation: u32,
        constraint: u32,
        preemptible: u32,
    }

    /// `os_workgroup_join`, from libSystem on macOS 11 and later. A generous opaque token, as the
    /// real one is opaque too and never larger than this.
    #[repr(align(8))]
    #[allow(dead_code)]
    struct JoinToken([u8; 256]);

    type WorkgroupJoin = unsafe extern "C" fn(*mut c_void, *mut JoinToken) -> c_int;
    type WorkgroupLeave = unsafe extern "C" fn(*mut c_void, *mut JoinToken);

    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn os_release(object: *mut c_void);
    }
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

    /// What a device gives its worker threads: its workgroup and how long its buffers run.
    pub(super) struct RealtimeWorkers {
        workgroup: Option<*mut c_void>,
        join: Option<WorkgroupJoin>,
        leave: Option<WorkgroupLeave>,
        tokens: Box<[UnsafeCell<JoinToken>]>,
        /// In Mach absolute time units.
        period: u32,
        computation: u32,
        constraint: u32,
    }

    // SAFETY: `workgroup` is only read; each token is touched only by its own worker.
    unsafe impl Send for RealtimeWorkers {}
    // SAFETY: as above.
    unsafe impl Sync for RealtimeWorkers {}

    fn property(id: AudioObjectId, selector: u32) -> PropertyAddress {
        let _ = id;
        PropertyAddress {
            selector,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        }
    }

    fn device_name(id: AudioObjectId) -> Option<String> {
        let address = property(id, PROP_NAME);
        let mut cf: *const c_void = null();
        let mut size = size_of::<*const c_void>() as u32;
        // SAFETY: `size` matches `cf`; Core Audio writes a retained CFString to it.
        let status = unsafe {
            AudioObjectGetPropertyData(
                id,
                &address,
                0,
                null(),
                &mut size,
                &mut cf as *mut _ as *mut c_void,
            )
        };
        if status != 0 || cf.is_null() {
            return None;
        }
        let mut buffer = [0 as c_char; 256];
        // SAFETY: `cf` is a CFString owned by us; the buffer holds 256 bytes.
        let ok = unsafe {
            let ok = CFStringGetCString(cf, buffer.as_mut_ptr(), 256, 0x0800_0100);
            CFRelease(cf);
            ok
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: the buffer is NUL-terminated by CFStringGetCString on success.
        Some(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn device_id(name: Option<&str>) -> Option<AudioObjectId> {
        if name.is_none() {
            let address = property(SYSTEM_OBJECT, PROP_DEFAULT_OUTPUT);
            let (mut id, mut size) = (0u32, 4u32);
            // SAFETY: `id` is a u32 of the size given.
            let status = unsafe {
                AudioObjectGetPropertyData(
                    SYSTEM_OBJECT,
                    &address,
                    0,
                    null(),
                    &mut size,
                    &mut id as *mut u32 as *mut c_void,
                )
            };
            return (status == 0 && id != 0).then_some(id);
        }
        let address = property(SYSTEM_OBJECT, PROP_DEVICES);
        let mut size = 0u32;
        // SAFETY: querying the size only.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &address, 0, null(), &mut size)
        };
        if status != 0 {
            return None;
        }
        let mut ids = vec![0u32; size as usize / 4];
        // SAFETY: `ids` holds `size` bytes.
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &address,
                0,
                null(),
                &mut size,
                ids.as_mut_ptr() as *mut c_void,
            )
        };
        if status != 0 {
            return None;
        }
        ids.into_iter()
            .find(|id| device_name(*id).as_deref() == name)
    }

    fn workgroup_of(id: AudioObjectId) -> Option<*mut c_void> {
        let address = property(id, PROP_WORKGROUP);
        let mut workgroup: *mut c_void = std::ptr::null_mut();
        let mut size = size_of::<*mut c_void>() as u32;
        // SAFETY: `size` matches; Core Audio writes a retained workgroup object.
        let status = unsafe {
            AudioObjectGetPropertyData(
                id,
                &address,
                0,
                null(),
                &mut size,
                &mut workgroup as *mut _ as *mut c_void,
            )
        };
        (status == 0 && !workgroup.is_null()).then_some(workgroup)
    }

    fn buffer_frames(id: AudioObjectId) -> Option<u32> {
        let address = PropertyAddress {
            selector: PROP_BUFFER_FRAMES,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        };
        let (mut frames, mut size) = (0u32, 4u32);
        // SAFETY: `frames` is a u32 of the size given.
        let status = unsafe {
            AudioObjectGetPropertyData(
                id,
                &address,
                0,
                null(),
                &mut size,
                &mut frames as *mut u32 as *mut c_void,
            )
        };
        (status == 0 && frames > 0).then_some(frames)
    }

    impl RealtimeWorkers {
        pub(super) fn new(device: Option<&str>, sample_rate: f64) -> Self {
            let id = device_id(device);
            let workgroup = id.and_then(workgroup_of);
            let frames = id.and_then(buffer_frames).unwrap_or(512) as f64;
            let seconds = frames / sample_rate.max(1.0);
            let mut timebase = Timebase::default();
            // SAFETY: valid to write to.
            unsafe { mach_timebase_info(&mut timebase) };
            let to_ticks = |seconds: f64| {
                (seconds * 1e9 * timebase.denom.max(1) as f64 / timebase.numer.max(1) as f64) as u32
            };
            // SAFETY: looking up symbols in the system library; the signatures are those of
            // os/workgroup.h.
            let (join, leave) = unsafe {
                let join = dlsym(RTLD_DEFAULT, c"os_workgroup_join".as_ptr());
                let leave = dlsym(RTLD_DEFAULT, c"os_workgroup_leave".as_ptr());
                let join = (!join.is_null()).then(|| std::mem::transmute::<_, WorkgroupJoin>(join));
                let leave =
                    (!leave.is_null()).then(|| std::mem::transmute::<_, WorkgroupLeave>(leave));
                (join, leave)
            };
            if workgroup.is_none() {
                log::info!("audio workers: no audio workgroup, time-constraint threads only");
            }
            Self {
                workgroup,
                join,
                leave,
                tokens: (0..rlive_engine::sched::MAX_WORKERS)
                    .map(|_| UnsafeCell::new(JoinToken([0; 256])))
                    .collect(),
                period: to_ticks(seconds),
                computation: to_ticks((seconds * 0.5).max(0.0005)),
                constraint: to_ticks(seconds),
            }
        }

        /// On a worker thread, before it can take any work.
        pub(super) fn start(&self, worker: usize) {
            let policy = TimeConstraint {
                period: self.period,
                computation: self.computation,
                constraint: self.constraint,
                preemptible: 1,
            };
            // SAFETY: valid arguments for this thread; a failure only leaves it at its old
            // priority, which costs headroom rather than correctness.
            unsafe {
                let thread = mach_thread_self();
                thread_policy_set(
                    thread,
                    THREAD_TIME_CONSTRAINT_POLICY,
                    &policy as *const TimeConstraint as *const c_int,
                    (size_of::<TimeConstraint>() / size_of::<c_int>()) as u32,
                );
                mach_port_deallocate(mach_task_self_, thread);
            }
            if let (Some(workgroup), Some(join), Some(token)) =
                (self.workgroup, self.join, self.tokens.get(worker))
            {
                // SAFETY: the token is this worker's alone until `stop`.
                unsafe { join(workgroup, token.get()) };
            }
        }

        pub(super) fn stop(&self, worker: usize) {
            if let (Some(workgroup), Some(leave), Some(token)) =
                (self.workgroup, self.leave, self.tokens.get(worker))
            {
                // SAFETY: left only by the thread that joined, with its own token.
                unsafe { leave(workgroup, token.get()) };
            }
        }
    }

    impl Drop for RealtimeWorkers {
        fn drop(&mut self) {
            if let Some(workgroup) = self.workgroup {
                // SAFETY: the retained reference from `workgroup_of`; every worker has left it.
                unsafe { os_release(workgroup) };
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// No real-time promotion on this platform yet.
    pub(super) struct RealtimeWorkers;

    impl RealtimeWorkers {
        pub(super) fn new(_device: Option<&str>, _sample_rate: f64) -> Self {
            Self
        }

        pub(super) fn start(&self, _worker: usize) {}

        pub(super) fn stop(&self, _worker: usize) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_thread_count_is_taken_as_given() {
        assert_eq!(resolve_thread_count(1), 1);
        assert_eq!(resolve_thread_count(5), 5);
    }

    #[test]
    fn auto_uses_some_cores_and_never_more_than_eight() {
        let n = resolve_thread_count(0);
        assert!((1..=8).contains(&n), "{n}");
    }

    #[test]
    fn workers_start_and_stop_cleanly_with_real_time_hooks() {
        // No real device is involved, so this exercises the fallback path; it must never fail.
        let hooks = worker_hooks(Some("no such device"), 48_000.0);
        let pool = rlive_engine::WorkerPool::spawn(2, Some(hooks)).expect("threads");
        pool.scatter(4, &|_| {});
        drop(pool);
    }
}
