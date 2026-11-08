use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a process-unique id. Ids loaded from a project bump the counter via [`reserve_id`].
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Make sure future ids are larger than `id` (used after loading a project from disk).
pub fn reserve_id(id: u64) {
    NEXT_ID.fetch_max(id + 1, Ordering::Relaxed);
}

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        pub struct $name(pub u64);

        impl $name {
            pub fn new() -> Self {
                Self(next_id())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

id_type!(TrackId);
id_type!(DeviceId);
