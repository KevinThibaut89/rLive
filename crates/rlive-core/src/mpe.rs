//! MPE zones: turning member-channel MIDI into per-note expression, and back again.
//!
//! An MPE zone gives each note its own "member" channel, so channel-wide messages (bend,
//! pressure, CC74) describe that note alone. The zone's global "manager" channel carries what
//! applies to all notes. [`MpeDecoder`] sits at MIDI input and turns the member channels into
//! [`MidiMessage::NoteExpr`]. [`MpeEncoder`] sits at the boundary of plugins that have no
//! native note expression and does the reverse.

use crate::{
    ExprKind, MidiEvent, MidiMessage, NoteId, NoteIds, NoteSpace, MPE_BEND_RANGE, SLIDE_CC,
};

/// Notes tracked per member channel. MPE gives each note its own channel, so this is generous.
const NOTES_PER_CHANNEL: usize = 8;

/// Notes the encoder can follow at once. A note beyond this plays with no member channel.
const MAX_ENCODED_NOTES: usize = 64;

const MCM_RPN: u8 = 6;
const BEND_RANGE_RPN: u8 = 0;
const SLIDE_CENTER: f32 = 64.0 / 127.0;

/// Which end of the channel range a zone uses. The lower zone's global channel is channel 1,
/// the upper zone's is channel 16 (0 and 15 here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpeZone {
    Lower,
    Upper,
}

impl MpeZone {
    /// The zone's global channel.
    pub fn manager(self) -> u8 {
        match self {
            MpeZone::Lower => 0,
            MpeZone::Upper => 15,
        }
    }

    /// The zone's note channels when it has `members` of them, counted from the global channel.
    fn member(self, index: u8) -> u8 {
        match self {
            MpeZone::Lower => 1 + index,
            MpeZone::Upper => 14 - index,
        }
    }

    fn slot(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy)]
struct Zone {
    members: u8,
    bend_range: f32,
}

#[derive(Clone, Copy, Default)]
struct Held {
    key: u8,
    id: NoteId,
}

#[derive(Clone, Copy)]
struct DecodedChannel {
    notes: [Held; NOTES_PER_CHANNEL],
    len: usize,
    bend: f32,
    slide: f32,
    pressure: f32,
    /// Registered parameter selected by CC 101 and 100, once both have arrived.
    rpn: [Option<u8>; 2],
}

impl DecodedChannel {
    const fn new() -> Self {
        Self {
            notes: [Held { key: 0, id: 0 }; NOTES_PER_CHANNEL],
            len: 0,
            bend: 0.0,
            slide: SLIDE_CENTER,
            pressure: 0.0,
            rpn: [None, None],
        }
    }

    fn held(&self) -> &[Held] {
        &self.notes[..self.len]
    }
}

/// Turns MPE into [`MidiMessage::NoteExpr`] at MIDI input. Non-member traffic passes unchanged.
pub struct MpeDecoder {
    zones: [Zone; 2],
    channels: [DecodedChannel; 16],
    ids: NoteIds,
}

impl Default for MpeDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MpeDecoder {
    /// A decoder for one lower zone using all 15 note channels, MPE's usual layout.
    pub fn new() -> Self {
        Self {
            zones: [
                Zone {
                    members: 15,
                    bend_range: MPE_BEND_RANGE,
                },
                Zone {
                    members: 0,
                    bend_range: MPE_BEND_RANGE,
                },
            ],
            channels: [DecodedChannel::new(); 16],
            ids: NoteIds::new(NoteSpace::MidiInput),
        }
    }

    /// Set a zone's number of note channels (0 turns the zone off).
    pub fn set_zone(&mut self, zone: MpeZone, members: u8) {
        self.zones[zone.slot()].members = members.min(15);
        self.zones[zone.slot()].bend_range = MPE_BEND_RANGE;
    }

    fn zone_of(&self, channel: u8) -> Option<MpeZone> {
        let lower = self.zones[MpeZone::Lower.slot()].members;
        let upper = self.zones[MpeZone::Upper.slot()].members;
        // The upper zone's manager is never a member of the lower zone.
        if (1..=lower).contains(&channel) && !(upper > 0 && channel == 15) {
            Some(MpeZone::Lower)
        } else if upper > 0 && (15 - upper..15).contains(&channel) {
            Some(MpeZone::Upper)
        } else {
            None
        }
    }

    /// Feed one parsed message, sending what it decodes to through `out`.
    pub fn feed(&mut self, message: MidiMessage, out: &mut impl FnMut(MidiMessage)) {
        if let MidiMessage::ControlChange { channel, cc, value } = message {
            self.track_rpn(channel, cc, value);
        }
        let channel = match message {
            MidiMessage::NoteOn { channel, .. }
            | MidiMessage::NoteOff { channel, .. }
            | MidiMessage::NoteExpr { channel, .. }
            | MidiMessage::ControlChange { channel, .. }
            | MidiMessage::PitchBend { channel, .. }
            | MidiMessage::ChannelPressure { channel, .. }
            | MidiMessage::ProgramChange { channel, .. } => channel,
            MidiMessage::AllNotesOff => {
                for c in self.channels.iter_mut() {
                    c.len = 0;
                }
                out(message);
                return;
            }
        };
        match self.zone_of(channel) {
            Some(zone) => self.feed_member(channel, zone, message, out),
            None => out(message),
        }
    }

    fn track_rpn(&mut self, channel: u8, cc: u8, value: u8) {
        let c = &mut self.channels[(channel & 15) as usize];
        match cc {
            101 => c.rpn[0] = Some(value),
            100 => c.rpn[1] = Some(value),
            6 => {
                let rpn = match c.rpn {
                    [Some(0), Some(lsb)] => Some(lsb),
                    _ => None,
                };
                match rpn {
                    Some(BEND_RANGE_RPN) if channel != 0 && channel != 15 => {
                        // Bend range set on any member applies to its whole zone.
                        if let Some(zone) = self.zone_of(channel) {
                            self.zones[zone.slot()].bend_range = value as f32;
                        }
                    }
                    Some(MCM_RPN) if channel == 0 => self.set_zone(MpeZone::Lower, value),
                    Some(MCM_RPN) if channel == 15 => self.set_zone(MpeZone::Upper, value),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn feed_member(
        &mut self,
        channel: u8,
        zone: MpeZone,
        message: MidiMessage,
        out: &mut impl FnMut(MidiMessage),
    ) {
        let c = &mut self.channels[channel as usize];
        match message {
            MidiMessage::NoteOn {
                key,
                velocity,
                tuning,
                ..
            } => {
                let note_id = self.ids.alloc();
                if c.len == NOTES_PER_CHANNEL {
                    // Forget the oldest: its note-off will pass through without an id.
                    c.notes.copy_within(1.., 0);
                    c.len -= 1;
                }
                c.notes[c.len] = Held { key, id: note_id };
                c.len += 1;
                out(MidiMessage::NoteOn {
                    channel,
                    key,
                    velocity,
                    note_id,
                    tuning,
                });
                // Expression sent ahead of the note (usual for MPE) applies from its start.
                let (bend, slide, pressure) = (c.bend, c.slide, c.pressure);
                let range = self.zones[zone.slot()].bend_range;
                let mut initial = |kind, value| {
                    out(MidiMessage::NoteExpr {
                        channel,
                        key,
                        note_id,
                        kind,
                        value,
                    })
                };
                if bend != 0.0 {
                    initial(ExprKind::Pitch, bend * range);
                }
                if slide != SLIDE_CENTER {
                    initial(ExprKind::Slide, slide);
                }
                if pressure != 0.0 {
                    initial(ExprKind::Pressure, pressure);
                }
            }
            MidiMessage::NoteOff { key, velocity, .. } => {
                let found = c.held().iter().position(|h| h.key == key);
                let note_id = found.map_or(0, |i| c.notes[i].id);
                if let Some(i) = found {
                    c.notes.copy_within(i + 1.., i);
                    c.len -= 1;
                    if c.len == 0 {
                        c.bend = 0.0;
                        c.pressure = 0.0;
                    }
                }
                out(MidiMessage::NoteOff {
                    channel,
                    key,
                    velocity,
                    note_id,
                });
            }
            // Poly aftertouch on a member channel reaches just that key.
            MidiMessage::NoteExpr {
                key, kind, value, ..
            } => {
                if let Some(h) = c.held().iter().find(|h| h.key == key) {
                    out(MidiMessage::NoteExpr {
                        channel,
                        key,
                        note_id: h.id,
                        kind,
                        value,
                    });
                }
            }
            MidiMessage::PitchBend { value, .. } => {
                c.bend = value;
                let range = self.zones[zone.slot()].bend_range;
                Self::spread(c, channel, ExprKind::Pitch, value * range, out);
            }
            MidiMessage::ChannelPressure { value, .. } => {
                c.pressure = value;
                Self::spread(c, channel, ExprKind::Pressure, value, out);
            }
            MidiMessage::ControlChange {
                cc: SLIDE_CC,
                value,
                ..
            } => {
                c.slide = value as f32 / 127.0;
                Self::spread(c, channel, ExprKind::Slide, c.slide, out);
            }
            other => out(other),
        }
    }

    /// Apply a channel-wide message to every note held on the channel.
    fn spread(
        c: &DecodedChannel,
        channel: u8,
        kind: ExprKind,
        value: f32,
        out: &mut impl FnMut(MidiMessage),
    ) {
        for h in c.held() {
            out(MidiMessage::NoteExpr {
                channel,
                key: h.key,
                note_id: h.id,
                kind,
                value,
            });
        }
    }
}

#[derive(Clone, Copy, Default)]
struct EncodedNote {
    id: NoteId,
    source_channel: u8,
    key: u8,
    member: u8,
}

#[derive(Clone, Copy)]
struct Member {
    notes: u8,
    last_used: u64,
    bend: f32,
    slide: f32,
    pressure: f32,
}

/// Turns [`MidiMessage::NoteExpr`] into MPE for plugins with no native note expression.
///
/// Until pitch or slide expression first arrives it stays transparent: notes keep their
/// channel and bytes reach the plugin exactly as they always did. Pressure alone can't engage
/// it, because poly aftertouch needs no member channel. Once engaged, each note takes its own
/// member channel and the zone is announced with an MPE configuration message.
pub struct MpeEncoder {
    zone: MpeZone,
    members: u8,
    notes: [EncodedNote; MAX_ENCODED_NOTES],
    len: usize,
    channels: [Member; 15],
    clock: u64,
    engaged: bool,
}

impl MpeEncoder {
    /// An encoder for `zone` using `members` note channels (1 to 15).
    pub fn new(zone: MpeZone, members: u8) -> Self {
        Self {
            zone,
            members: members.clamp(1, 15),
            notes: [EncodedNote::default(); MAX_ENCODED_NOTES],
            len: 0,
            channels: [Member {
                notes: 0,
                last_used: 0,
                bend: 0.0,
                slide: SLIDE_CENTER,
                pressure: 0.0,
            }; 15],
            clock: 0,
            engaged: false,
        }
    }

    /// Whether pitch or slide expression has switched this encoder to member channels.
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    /// Forget all notes and go back to being transparent.
    pub fn reset(&mut self) {
        *self = Self::new(self.zone, self.members);
    }

    /// Encode one block of events, sending the results to `out`.
    pub fn encode(&mut self, events: &[MidiEvent], out: &mut impl FnMut(MidiEvent)) {
        if !self.engaged && events.iter().any(needs_member_channels) {
            self.engaged = true;
            self.announce_zone(events[0].offset, out);
        }
        for e in events {
            if self.engaged {
                self.encode_event(*e, out);
            } else {
                out(*e);
            }
        }
    }

    /// The MPE configuration message: RPN 6, member count, on the manager channel.
    fn announce_zone(&self, offset: u32, out: &mut impl FnMut(MidiEvent)) {
        let channel = self.zone.manager();
        for (cc, value) in [(101, 0), (100, MCM_RPN), (6, self.members)] {
            out(MidiEvent::new(
                offset,
                MidiMessage::ControlChange { channel, cc, value },
            ));
        }
    }

    fn encode_event(&mut self, e: MidiEvent, out: &mut impl FnMut(MidiEvent)) {
        let offset = e.offset;
        match e.message {
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
                note_id,
                tuning,
            } => {
                if self.len == MAX_ENCODED_NOTES {
                    // Nothing could route its note-off, so play it without a member channel.
                    out(e);
                    return;
                }
                let member = self.claim_member(offset, out);
                self.notes[self.len] = EncodedNote {
                    id: note_id,
                    source_channel: channel,
                    key,
                    member,
                };
                self.len += 1;
                out(MidiEvent::new(
                    offset,
                    MidiMessage::NoteOn {
                        channel: member,
                        key,
                        velocity,
                        note_id,
                        tuning,
                    },
                ));
            }
            MidiMessage::NoteOff {
                channel,
                key,
                velocity,
                note_id,
            } => match self.find(channel, key, note_id) {
                Some(i) => {
                    let member = self.notes[i].member;
                    self.notes.copy_within(i + 1..self.len, i);
                    self.len -= 1;
                    let slot = self.channel_slot(member);
                    self.channels[slot].notes = self.channels[slot].notes.saturating_sub(1);
                    out(MidiEvent::new(
                        offset,
                        MidiMessage::NoteOff {
                            channel: member,
                            key,
                            velocity,
                            note_id,
                        },
                    ));
                }
                // Started before encoding began (or never tracked): its channel is unchanged.
                None => out(e),
            },
            MidiMessage::NoteExpr {
                channel,
                key,
                note_id,
                kind,
                value,
            } => match self.find(channel, key, note_id) {
                Some(i) => {
                    let member = self.notes[i].member;
                    let slot = self.channel_slot(member);
                    let state = &mut self.channels[slot];
                    let message = match kind {
                        ExprKind::Pitch => {
                            let bend = value / MPE_BEND_RANGE;
                            state.bend = bend;
                            MidiMessage::PitchBend {
                                channel: member,
                                value: bend,
                            }
                        }
                        ExprKind::Slide => {
                            state.slide = value;
                            MidiMessage::ControlChange {
                                channel: member,
                                cc: SLIDE_CC,
                                value: (value.clamp(0.0, 1.0) * 127.0).round() as u8,
                            }
                        }
                        ExprKind::Pressure => {
                            state.pressure = value;
                            MidiMessage::ChannelPressure {
                                channel: member,
                                value,
                            }
                        }
                    };
                    out(MidiEvent::new(offset, message));
                }
                None if kind == ExprKind::Pressure => out(e),
                None => {}
            },
            MidiMessage::AllNotesOff => {
                for index in 0..self.members {
                    out(MidiEvent::new(
                        offset,
                        MidiMessage::ControlChange {
                            channel: self.zone.member(index),
                            cc: 123,
                            value: 0,
                        },
                    ));
                }
                self.len = 0;
                for c in self.channels.iter_mut() {
                    c.notes = 0;
                }
                out(e);
            }
            _ => out(e),
        }
    }

    fn find(&self, channel: u8, key: u8, note_id: NoteId) -> Option<usize> {
        self.notes[..self.len].iter().position(|n| {
            if note_id != 0 {
                n.id == note_id
            } else {
                n.source_channel == channel && n.key == key
            }
        })
    }

    fn channel_slot(&self, member: u8) -> usize {
        match self.zone {
            MpeZone::Lower => member as usize - 1,
            MpeZone::Upper => 14 - member as usize,
        }
    }

    /// Pick the least recently used free member channel, or share the quietest if all are
    /// busy, and clear what the last note left on it.
    fn claim_member(&mut self, offset: u32, out: &mut impl FnMut(MidiEvent)) -> u8 {
        let in_use = |c: &Member| (c.notes, c.last_used);
        let slot = self.channels[..self.members as usize]
            .iter()
            .enumerate()
            .min_by_key(|(_, c)| in_use(c))
            .map_or(0, |(i, _)| i);
        self.clock += 1;
        let channel = self.zone.member(slot as u8);
        let state = &mut self.channels[slot];
        state.notes += 1;
        state.last_used = self.clock;
        if state.bend != 0.0 {
            state.bend = 0.0;
            out(MidiEvent::new(
                offset,
                MidiMessage::PitchBend {
                    channel,
                    value: 0.0,
                },
            ));
        }
        if state.slide != SLIDE_CENTER {
            state.slide = SLIDE_CENTER;
            out(MidiEvent::new(
                offset,
                MidiMessage::ControlChange {
                    channel,
                    cc: SLIDE_CC,
                    value: 64,
                },
            ));
        }
        if state.pressure != 0.0 {
            state.pressure = 0.0;
            out(MidiEvent::new(
                offset,
                MidiMessage::ChannelPressure {
                    channel,
                    value: 0.0,
                },
            ));
        }
        channel
    }
}

fn needs_member_channels(e: &MidiEvent) -> bool {
    matches!(
        e.message,
        MidiMessage::NoteExpr {
            kind: ExprKind::Pitch | ExprKind::Slide,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(d: &mut MpeDecoder, m: MidiMessage) -> Vec<MidiMessage> {
        let mut out = Vec::new();
        d.feed(m, &mut |x| out.push(x));
        out
    }

    fn encoded(e: &mut MpeEncoder, events: &[MidiEvent]) -> Vec<MidiEvent> {
        let mut out = Vec::new();
        e.encode(events, &mut |x| out.push(x));
        out
    }

    fn on(channel: u8, key: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel,
            key,
            velocity: 1.0,
            note_id: 0,
            tuning: 0.0,
        }
    }

    fn off(channel: u8, key: u8) -> MidiMessage {
        MidiMessage::NoteOff {
            channel,
            key,
            velocity: 0.5,
            note_id: 0,
        }
    }

    fn id_of(m: MidiMessage) -> NoteId {
        match m {
            MidiMessage::NoteOn { note_id, .. }
            | MidiMessage::NoteOff { note_id, .. }
            | MidiMessage::NoteExpr { note_id, .. } => note_id,
            _ => 0,
        }
    }

    #[test]
    fn member_channel_notes_get_ids_and_channel_messages_become_expression() {
        let mut d = MpeDecoder::new();
        let a = decoded(&mut d, on(1, 60))[0];
        let b = decoded(&mut d, on(2, 64))[0];
        assert!(id_of(a) != 0 && id_of(a) != id_of(b));

        let bend = decoded(
            &mut d,
            MidiMessage::PitchBend {
                channel: 1,
                value: 0.5,
            },
        );
        assert_eq!(
            bend,
            vec![MidiMessage::NoteExpr {
                channel: 1,
                key: 60,
                note_id: id_of(a),
                kind: ExprKind::Pitch,
                value: 24.0,
            }]
        );
        let slide = decoded(
            &mut d,
            MidiMessage::ControlChange {
                channel: 2,
                cc: SLIDE_CC,
                value: 127,
            },
        );
        assert_eq!(id_of(slide[0]), id_of(b));
        assert!(matches!(
            slide[0],
            MidiMessage::NoteExpr {
                kind: ExprKind::Slide,
                value: 1.0,
                ..
            }
        ));
        let pressure = decoded(
            &mut d,
            MidiMessage::ChannelPressure {
                channel: 2,
                value: 0.25,
            },
        );
        assert!(matches!(
            pressure[0],
            MidiMessage::NoteExpr {
                kind: ExprKind::Pressure,
                ..
            }
        ));

        // The note-off carries the same id.
        assert_eq!(id_of(decoded(&mut d, off(1, 60))[0]), id_of(a));
    }

    #[test]
    fn same_key_on_two_member_channels_stays_distinct() {
        let mut d = MpeDecoder::new();
        let a = id_of(decoded(&mut d, on(1, 60))[0]);
        let b = id_of(decoded(&mut d, on(2, 60))[0]);
        assert_ne!(a, b);
        assert_eq!(id_of(decoded(&mut d, off(2, 60))[0]), b);
        assert_eq!(id_of(decoded(&mut d, off(1, 60))[0]), a);
    }

    #[test]
    fn expression_sent_before_a_note_applies_to_it() {
        let mut d = MpeDecoder::new();
        assert!(decoded(
            &mut d,
            MidiMessage::PitchBend {
                channel: 3,
                value: 0.25,
            },
        )
        .is_empty());
        let out = decoded(&mut d, on(3, 70));
        assert!(matches!(out[0], MidiMessage::NoteOn { key: 70, .. }));
        assert!(matches!(
            out[1],
            MidiMessage::NoteExpr {
                kind: ExprKind::Pitch,
                value: 12.0,
                ..
            }
        ));
    }

    #[test]
    fn the_manager_channel_passes_through_untouched() {
        let mut d = MpeDecoder::new();
        let bend = MidiMessage::PitchBend {
            channel: 0,
            value: 0.5,
        };
        assert_eq!(decoded(&mut d, bend), vec![bend]);
        assert_eq!(decoded(&mut d, on(0, 60)), vec![on(0, 60)]);
    }

    #[test]
    fn zones_follow_the_mcm_and_outside_channels_pass_through() {
        let mut d = MpeDecoder::new();
        // Lower zone of 3 members; channel 5 is then outside any zone.
        for (cc, value) in [(101, 0), (100, 6), (6, 3)] {
            decoded(
                &mut d,
                MidiMessage::ControlChange {
                    channel: 0,
                    cc,
                    value,
                },
            );
        }
        assert_eq!(decoded(&mut d, on(5, 60)), vec![on(5, 60)]);
        assert_ne!(id_of(decoded(&mut d, on(3, 60))[0]), 0);
    }

    #[test]
    fn the_upper_zone_uses_channels_below_its_manager() {
        let mut d = MpeDecoder::new();
        d.set_zone(MpeZone::Lower, 0);
        d.set_zone(MpeZone::Upper, 5);
        let on14 = decoded(&mut d, on(14, 60));
        assert_ne!(id_of(on14[0]), 0);
        assert_eq!(decoded(&mut d, on(15, 60)), vec![on(15, 60)]);
    }

    #[test]
    fn a_member_channel_bend_range_applies_to_its_zone() {
        let mut d = MpeDecoder::new();
        for (cc, value) in [(101, 0), (100, 0), (6, 24)] {
            decoded(
                &mut d,
                MidiMessage::ControlChange {
                    channel: 1,
                    cc,
                    value,
                },
            );
        }
        decoded(&mut d, on(2, 60));
        let out = decoded(
            &mut d,
            MidiMessage::PitchBend {
                channel: 2,
                value: 0.5,
            },
        );
        assert!(matches!(
            out[0],
            MidiMessage::NoteExpr {
                kind: ExprKind::Pitch,
                value: 12.0,
                ..
            }
        ));
    }

    fn expr(note_id: NoteId, key: u8, kind: ExprKind, value: f32) -> MidiEvent {
        MidiEvent::new(
            0,
            MidiMessage::NoteExpr {
                channel: 0,
                key,
                note_id,
                kind,
                value,
            },
        )
    }

    fn id_on(note_id: NoteId, key: u8) -> MidiEvent {
        MidiEvent::new(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                key,
                velocity: 1.0,
                note_id,
                tuning: 0.0,
            },
        )
    }

    fn id_off(note_id: NoteId, key: u8) -> MidiEvent {
        MidiEvent::new(
            0,
            MidiMessage::NoteOff {
                channel: 0,
                key,
                velocity: 0.5,
                note_id,
            },
        )
    }

    fn messages(events: &[MidiEvent]) -> Vec<MidiMessage> {
        events.iter().map(|e| e.message).collect()
    }

    #[test]
    fn the_encoder_is_transparent_for_plain_notes() {
        let mut e = MpeEncoder::new(MpeZone::Lower, 15);
        let input = [id_on(1, 60), id_off(1, 60)];
        assert_eq!(encoded(&mut e, &input), input);
        assert!(!e.engaged());
    }

    #[test]
    fn pressure_alone_stays_poly_aftertouch() {
        let mut e = MpeEncoder::new(MpeZone::Lower, 15);
        let input = [id_on(1, 60), expr(1, 60, ExprKind::Pressure, 0.5)];
        assert_eq!(encoded(&mut e, &input), input);
        assert!(!e.engaged());
    }

    #[test]
    fn expression_moves_notes_to_their_own_member_channels() {
        let mut e = MpeEncoder::new(MpeZone::Lower, 15);
        let out = encoded(
            &mut e,
            &[
                id_on(1, 60),
                id_on(2, 60),
                expr(1, 60, ExprKind::Pitch, 12.0),
                expr(2, 60, ExprKind::Slide, 1.0),
                expr(1, 60, ExprKind::Pressure, 0.5),
            ],
        );
        let out = messages(&out);
        // The zone is announced first (RPN 6), then each note on its own channel.
        assert_eq!(
            &out[..3],
            &[
                MidiMessage::ControlChange {
                    channel: 0,
                    cc: 101,
                    value: 0
                },
                MidiMessage::ControlChange {
                    channel: 0,
                    cc: 100,
                    value: 6
                },
                MidiMessage::ControlChange {
                    channel: 0,
                    cc: 6,
                    value: 15
                },
            ]
        );
        assert!(matches!(
            out[3],
            MidiMessage::NoteOn {
                channel: 1,
                key: 60,
                ..
            }
        ));
        assert!(matches!(
            out[4],
            MidiMessage::NoteOn {
                channel: 2,
                key: 60,
                ..
            }
        ));
        assert_eq!(
            out[5],
            MidiMessage::PitchBend {
                channel: 1,
                value: 0.25
            }
        );
        assert_eq!(
            out[6],
            MidiMessage::ControlChange {
                channel: 2,
                cc: SLIDE_CC,
                value: 127
            }
        );
        assert_eq!(
            out[7],
            MidiMessage::ChannelPressure {
                channel: 1,
                value: 0.5
            }
        );

        // Note-offs follow their note to its member channel.
        let out = messages(&encoded(&mut e, &[id_off(2, 60)]));
        assert!(matches!(
            out[0],
            MidiMessage::NoteOff {
                channel: 2,
                key: 60,
                ..
            }
        ));
    }

    #[test]
    fn a_reused_channel_is_cleared_before_the_next_note() {
        let mut e = MpeEncoder::new(MpeZone::Lower, 1);
        encoded(&mut e, &[id_on(1, 60), expr(1, 60, ExprKind::Pitch, 12.0)]);
        encoded(&mut e, &[id_off(1, 60)]);
        let out = messages(&encoded(&mut e, &[id_on(2, 62)]));
        assert_eq!(
            out[0],
            MidiMessage::PitchBend {
                channel: 1,
                value: 0.0
            }
        );
        assert!(matches!(out[1], MidiMessage::NoteOn { channel: 1, .. }));
    }

    #[test]
    fn notes_held_when_it_engages_keep_their_channel() {
        let mut e = MpeEncoder::new(MpeZone::Lower, 15);
        encoded(&mut e, &[id_on(1, 60)]);
        encoded(&mut e, &[expr(2, 61, ExprKind::Pitch, 1.0)]);
        // Note 1 predates engaging, so its note-off stays where the note was played.
        assert_eq!(encoded(&mut e, &[id_off(1, 60)]), vec![id_off(1, 60)]);
    }

    #[test]
    fn the_upper_zone_counts_down_from_its_manager() {
        let mut e = MpeEncoder::new(MpeZone::Upper, 4);
        let out = messages(&encoded(
            &mut e,
            &[id_on(1, 60), expr(1, 60, ExprKind::Pitch, 0.0)],
        ));
        assert_eq!(
            out[2],
            MidiMessage::ControlChange {
                channel: 15,
                cc: 6,
                value: 4
            }
        );
        assert!(matches!(out[3], MidiMessage::NoteOn { channel: 14, .. }));
    }

    #[test]
    fn all_notes_off_reaches_every_member_channel() {
        let mut e = MpeEncoder::new(MpeZone::Lower, 2);
        encoded(&mut e, &[id_on(1, 60), expr(1, 60, ExprKind::Pitch, 1.0)]);
        let out = messages(&encoded(
            &mut e,
            &[MidiEvent::new(0, MidiMessage::AllNotesOff)],
        ));
        assert_eq!(
            &out[..2],
            &[
                MidiMessage::ControlChange {
                    channel: 1,
                    cc: 123,
                    value: 0
                },
                MidiMessage::ControlChange {
                    channel: 2,
                    cc: 123,
                    value: 0
                }
            ]
        );
        assert_eq!(*out.last().unwrap(), MidiMessage::AllNotesOff);
    }

    #[test]
    fn encoded_mpe_decodes_back_to_the_same_expression() {
        let mut e = MpeEncoder::new(MpeZone::Lower, 15);
        let mut d = MpeDecoder::new();
        let out = encoded(
            &mut e,
            &[
                id_on(1, 60),
                expr(1, 60, ExprKind::Pitch, 6.0),
                expr(1, 60, ExprKind::Pressure, 0.5),
            ],
        );
        let mut got = Vec::new();
        for ev in out {
            let (bytes, n) = ev.message.to_bytes();
            d.feed(MidiMessage::from_bytes(&bytes[..n]).unwrap(), &mut |m| {
                got.push(m)
            });
        }
        let on = got
            .iter()
            .find(|m| matches!(m, MidiMessage::NoteOn { .. }))
            .unwrap();
        let id = id_of(*on);
        let pitch = got.iter().find_map(|m| match m {
            MidiMessage::NoteExpr {
                kind: ExprKind::Pitch,
                note_id,
                value,
                ..
            } if *note_id == id => Some(*value),
            _ => None,
        });
        assert!((pitch.unwrap() - 6.0).abs() < 0.01);
        assert!(got.iter().any(|m| matches!(
            m,
            MidiMessage::NoteExpr {
                kind: ExprKind::Pressure,
                ..
            }
        )));
    }
}
