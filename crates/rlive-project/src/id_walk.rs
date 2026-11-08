//! Finds every id in a project without knowing its shape.
//!
//! The walk rides on `Serialize`: anything that reaches the file is visited, however deeply a
//! rack nests it, and a new id field or a new `id_types!` entry needs no code here. An id is
//! recognised by its newtype's name in [`ID_TYPES`], so a `u32` parameter id is not mistaken for
//! one.

use rlive_core::ids::ID_TYPES;
use serde::ser::{self, Serialize};
use std::fmt;

/// Calls `f` with every id reachable from `value`.
pub fn walk_ids<T: Serialize + ?Sized>(value: &T, mut f: impl FnMut(u64)) {
    let mut w = Walker {
        f: &mut f,
        in_id: false,
    };
    // Only `Walker`'s own methods can fail, and they don't.
    let _ = value.serialize(&mut w);
}

struct Walker<'a> {
    f: &'a mut dyn FnMut(u64),
    in_id: bool,
}

/// Never constructed: the walker has no errors of its own.
#[derive(Debug)]
struct WalkError;

impl fmt::Display for WalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("id walk failed")
    }
}

impl std::error::Error for WalkError {}

impl ser::Error for WalkError {
    fn custom<T: fmt::Display>(_: T) -> Self {
        WalkError
    }
}

macro_rules! ignore_scalars {
    ($($method:ident: $ty:ty),* $(,)?) => {
        $(fn $method(self, _: $ty) -> Result<(), WalkError> { Ok(()) })*
    };
}

macro_rules! descend_each {
    ($($tr:ident :: $method:ident),* $(,)?) => {
        $(impl ser::$tr for &mut Walker<'_> {
            type Ok = ();
            type Error = WalkError;
            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), WalkError> {
                value.serialize(&mut **self)
            }
            fn end(self) -> Result<(), WalkError> {
                Ok(())
            }
        })*
    };
}

macro_rules! descend_fields {
    ($($tr:ident),* $(,)?) => {
        $(impl ser::$tr for &mut Walker<'_> {
            type Ok = ();
            type Error = WalkError;
            fn serialize_field<T: Serialize + ?Sized>(
                &mut self,
                _: &'static str,
                value: &T,
            ) -> Result<(), WalkError> {
                value.serialize(&mut **self)
            }
            fn end(self) -> Result<(), WalkError> {
                Ok(())
            }
        })*
    };
}

descend_each!(
    SerializeSeq::serialize_element,
    SerializeTuple::serialize_element,
    SerializeTupleStruct::serialize_field,
    SerializeTupleVariant::serialize_field,
);
descend_fields!(SerializeStruct, SerializeStructVariant);

impl ser::SerializeMap for &mut Walker<'_> {
    type Ok = ();
    type Error = WalkError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), WalkError> {
        key.serialize(&mut **self)
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), WalkError> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), WalkError> {
        Ok(())
    }
}

impl ser::Serializer for &mut Walker<'_> {
    type Ok = ();
    type Error = WalkError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    ignore_scalars!(
        serialize_bool: bool,
        serialize_i8: i8,
        serialize_i16: i16,
        serialize_i32: i32,
        serialize_i64: i64,
        serialize_u8: u8,
        serialize_u16: u16,
        serialize_u32: u32,
        serialize_f32: f32,
        serialize_f64: f64,
        serialize_char: char,
        serialize_str: &str,
        serialize_bytes: &[u8],
    );

    fn serialize_u64(self, v: u64) -> Result<(), WalkError> {
        if self.in_id {
            (self.f)(v);
        }
        Ok(())
    }

    fn serialize_none(self) -> Result<(), WalkError> {
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), WalkError> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), WalkError> {
        Ok(())
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<(), WalkError> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<(), WalkError> {
        Ok(())
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<(), WalkError> {
        self.in_id = ID_TYPES.contains(&name);
        let done = value.serialize(&mut *self);
        self.in_id = false;
        done
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        value: &T,
    ) -> Result<(), WalkError> {
        value.serialize(self)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self, WalkError> {
        Ok(self)
    }

    fn serialize_tuple(self, _: usize) -> Result<Self, WalkError> {
        Ok(self)
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, WalkError> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, WalkError> {
        Ok(self)
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Self, WalkError> {
        Ok(self)
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, WalkError> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, WalkError> {
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chain, Clip, Device, Project, RackKind, Track, TrackKind};
    use rlive_core::{DeviceId, GrooveId, TrackId};
    use std::collections::HashSet;

    fn ids_of(p: &Project) -> HashSet<u64> {
        let mut seen = HashSet::new();
        walk_ids(p, |id| {
            seen.insert(id);
        });
        seen
    }

    #[test]
    fn the_walk_reaches_ids_nested_in_racks_and_new_fields() {
        let mut p = Project::empty();
        let mut track = Track::new("t", TrackKind::Midi, [0; 3], p.scenes.len());
        let mut inner_rack = Device::rack(RackKind::Instrument, "Inner");
        let leaf = Device::builtin("rlive.poly", "Leaf");
        let leaf_id = leaf.id;
        let mut inner_chain = Chain::new("deep");
        let deep_chain = inner_chain.id;
        inner_chain.devices.push(leaf);
        if let crate::DeviceKind::Rack { chains, .. } = &mut inner_rack.kind {
            chains.push(inner_chain);
        }
        let inner_id = inner_rack.id;
        let mut outer_chain = Chain::new("shallow");
        outer_chain.devices.push(inner_rack);
        let mut outer = Device::rack(RackKind::Audio, "Outer");
        if let crate::DeviceKind::Rack { returns, .. } = &mut outer.kind {
            returns.push(outer_chain);
        }
        track.devices.push(outer);
        let groove = GrooveId(424_242);
        let mut clip = Clip::new_midi("c", [0; 3], 1, p.time_signature);
        clip.groove = Some(groove);
        track.clips[0] = Some(clip);
        let group = TrackId::new();
        track.group = Some(group);
        p.tracks.push(track);

        let ids = ids_of(&p);
        for id in [leaf_id.0, inner_id.0, deep_chain.0, groove.0, group.0] {
            assert!(ids.contains(&id), "missed {id}");
        }
    }

    #[test]
    fn non_id_numbers_are_not_mistaken_for_ids() {
        let mut p = Project::empty();
        let mut d = Device::builtin("rlive.poly", "p");
        d.params.insert(987_654, 1.0);
        p.master.devices.push(d);
        p.arrangement.loop_end = 123_456_789;
        let ids = ids_of(&p);
        assert!(!ids.contains(&987_654));
        assert!(!ids.contains(&123_456_789));
    }

    #[test]
    fn a_device_id_is_found_wherever_it_is_serialized() {
        let d = DeviceId(77);
        let mut seen = Vec::new();
        walk_ids(&d, |id| seen.push(id));
        assert_eq!(seen, [77]);
    }
}
