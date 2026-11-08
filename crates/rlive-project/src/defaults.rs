//! Serde helpers for fields that stay out of the file while they hold their default.

pub(crate) fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

pub(crate) fn yes() -> bool {
    true
}

pub(crate) fn is_true(v: &bool) -> bool {
    *v
}

pub(crate) fn one() -> f32 {
    1.0
}

pub(crate) fn is_one(v: &f32) -> bool {
    *v == 1.0
}
