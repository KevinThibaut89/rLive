//! The UI's end of device view channels. A channel is read only while its device is on screen,
//! because a device stops measuring as soon as its last reader goes.

use super::super::widgets::MeterState;
use crate::session::DeviceRuntime;
use rlive_core::{DeviceId, ViewChannel, ViewId, ViewReader, METER_VALUES, SCOPE_VALUES};
use std::collections::HashMap;
use std::sync::Arc;

struct Attached {
    channel: Arc<ViewChannel>,
    reader: ViewReader,
    /// The frame it was last used in.
    seen: u64,
}

/// The readers of the devices on screen, and what was last read from them. A scope holds on to
/// what it read, so a frame that loses the race with the audio thread keeps showing good data.
#[derive(Default)]
pub struct ViewStore {
    frame: u64,
    attached: HashMap<DeviceId, Attached>,
    meters: HashMap<(DeviceId, ViewId), MeterState>,
    scopes: HashMap<(DeviceId, ViewId), Box<[f32; SCOPE_VALUES]>>,
}

impl ViewStore {
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Drop the readers of devices that weren't drawn this frame, so they stop measuring.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.attached.retain(|_, a| a.seen == frame);
        let attached = &self.attached;
        self.meters.retain(|(id, _), _| attached.contains_key(id));
        self.scopes.retain(|(id, _), _| attached.contains_key(id));
    }

    /// The reader of a device drawn this frame. A device with no channel (nothing declared needs
    /// one) has none. A rebuilt device has a new channel, so the old reader is replaced.
    pub fn reader(&mut self, device: DeviceId, rt: &DeviceRuntime) -> Option<&ViewReader> {
        let channel = rt.view.as_ref()?;
        let attach = || Attached {
            channel: channel.clone(),
            reader: channel.attach(),
            seen: 0,
        };
        let entry = self.attached.entry(device).or_insert_with(attach);
        if !Arc::ptr_eq(&entry.channel, channel) {
            *entry = attach();
        }
        entry.seen = self.frame;
        Some(&entry.reader)
    }

    /// The level to show on a meter this frame, a decaying peak. A bypassed device has stopped
    /// publishing, so what's in its channel is stale: it reads as silence.
    pub fn meter(
        &mut self,
        device: DeviceId,
        rt: &DeviceRuntime,
        view: ViewId,
        live: bool,
        dt: f32,
    ) -> (f32, f32) {
        let mut out = [0.0; METER_VALUES];
        if live {
            if let Some(r) = self.reader(device, rt) {
                r.read(view, &mut out);
            }
        }
        let state = self.meters.entry((device, view)).or_default();
        state.update((out[0], out[1]), dt);
        state.get()
    }

    /// The values to draw on a scope this frame, all zero for a device that isn't publishing.
    pub fn scope(
        &mut self,
        device: DeviceId,
        rt: &DeviceRuntime,
        view: ViewId,
        live: bool,
    ) -> &[f32] {
        let mut values = [0.0; SCOPE_VALUES];
        let read = live
            && self
                .reader(device, rt)
                .is_some_and(|r| r.read(view, &mut values));
        let buffer = self
            .scopes
            .entry((device, view))
            .or_insert_with(|| Box::new([0.0; SCOPE_VALUES]));
        if read {
            **buffer = values;
        } else if !live {
            buffer.fill(0.0);
        }
        &buffer[..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::ViewWriter;
    use rlive_devices::DeviceRole;
    use std::sync::atomic::AtomicU32;

    fn runtime(view: Option<Arc<ViewChannel>>) -> DeviceRuntime {
        DeviceRuntime {
            params: Vec::new(),
            plugin: None,
            error: None,
            role: DeviceRole::AudioEffect,
            latency: Arc::new(AtomicU32::new(0)),
            sample_slots: 0,
            unmoved: HashMap::new(),
            layout: Vec::new(),
            view,
            sidechain_input: false,
            touched: None,
            window_title: String::new(),
            window_visible: false,
            presets: Vec::new(),
            aux_names: Vec::new(),
        }
    }

    fn id(n: u64) -> DeviceId {
        DeviceId(n)
    }

    #[test]
    fn a_device_with_no_channel_has_no_reader() {
        let mut store = ViewStore::default();
        store.begin_frame();
        assert!(store.reader(id(1), &runtime(None)).is_none());
    }

    #[test]
    fn a_device_only_writes_while_it_is_on_screen() {
        let channel = Arc::new(ViewChannel::new(2));
        let rt = runtime(Some(channel.clone()));
        let writer = ViewWriter::new(&channel);
        let mut store = ViewStore::default();

        store.begin_frame();
        store.reader(id(1), &rt).unwrap();
        store.end_frame();
        assert!(writer.is_active(), "drawn this frame");

        store.begin_frame();
        store.end_frame();
        assert!(!writer.is_active(), "not drawn this frame");

        // Coming back on screen picks it up again.
        store.begin_frame();
        store.reader(id(1), &rt).unwrap();
        store.end_frame();
        assert!(writer.is_active());
    }

    #[test]
    fn a_reader_follows_the_device_to_its_new_channel() {
        let (old, new) = (Arc::new(ViewChannel::new(2)), Arc::new(ViewChannel::new(2)));
        let mut store = ViewStore::default();
        store.begin_frame();
        store.reader(id(1), &runtime(Some(old.clone()))).unwrap();
        store.reader(id(1), &runtime(Some(new.clone()))).unwrap();
        store.end_frame();
        assert!(!ViewWriter::new(&old).is_active());
        assert!(ViewWriter::new(&new).is_active());
    }

    #[test]
    fn a_meter_reads_what_is_published_and_a_bypassed_one_falls() {
        let channel = Arc::new(ViewChannel::new(METER_VALUES));
        let rt = runtime(Some(channel.clone()));
        let writer = ViewWriter::new(&channel);
        let mut store = ViewStore::default();
        store.begin_frame();
        store.meter(id(1), &rt, ViewId(0), true, 0.016);
        writer.publish(ViewId(0), &[0.5, 0.25]);
        let (l, r) = store.meter(id(1), &rt, ViewId(0), true, 0.016);
        assert_eq!((l, r), (0.5, 0.25));

        let (l, _) = store.meter(id(1), &rt, ViewId(0), false, 0.1);
        assert!(l < 0.5, "decays when bypassed: {l}");
    }

    #[test]
    fn a_scope_keeps_its_buffer_and_clears_when_not_live() {
        let channel = Arc::new(ViewChannel::new(SCOPE_VALUES));
        let rt = runtime(Some(channel.clone()));
        let writer = ViewWriter::new(&channel);
        let mut store = ViewStore::default();
        store.begin_frame();
        store.scope(id(1), &rt, ViewId(0), true);
        writer.publish(ViewId(0), &[0.75; 4]);
        assert_eq!(store.scope(id(1), &rt, ViewId(0), true)[..4], [0.75; 4]);
        assert!(store
            .scope(id(1), &rt, ViewId(0), false)
            .iter()
            .all(|v| *v == 0.0));
    }

    #[test]
    fn what_was_read_goes_when_the_device_goes() {
        let channel = Arc::new(ViewChannel::new(METER_VALUES));
        let rt = runtime(Some(channel));
        let mut store = ViewStore::default();
        store.begin_frame();
        store.meter(id(1), &rt, ViewId(0), true, 0.016);
        store.end_frame();
        assert_eq!(store.meters.len(), 1);
        store.begin_frame();
        store.end_frame();
        assert!(store.meters.is_empty());
    }
}
