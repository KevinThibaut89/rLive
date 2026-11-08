use crate::defaults::is_default;
use crate::FollowActions;
use rlive_core::TimeSignature;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
    /// Launching the scene sets this tempo.
    #[serde(default, skip_serializing_if = "is_default")]
    pub tempo: Option<f64>,
    /// Launching the scene sets this time signature.
    #[serde(default, skip_serializing_if = "is_default")]
    pub signature: Option<TimeSignature>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub follow: FollowActions,
    #[serde(default, skip_serializing_if = "is_default")]
    pub color: Option<[u8; 3]>,
    /// The user's info text.
    #[serde(default, skip_serializing_if = "is_default")]
    pub info: String,
}

impl Scene {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tempo: None,
            signature: None,
            follow: FollowActions::default(),
            color: None,
            info: String::new(),
        }
    }
}
