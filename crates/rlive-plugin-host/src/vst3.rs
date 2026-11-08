//! VST3 plugin hosting.
use crate::{HostError, PluginDescriptor, PluginInstance, Result};
use std::path::Path;

pub fn scan(_path: &Path) -> Result<Vec<PluginDescriptor>> {
    Err(HostError::Unsupported("VST3"))
}

pub fn instantiate(
    _desc: &PluginDescriptor,
    _sr: f64,
    _max_block: usize,
) -> Result<PluginInstance> {
    Err(HostError::Unsupported("VST3"))
}
