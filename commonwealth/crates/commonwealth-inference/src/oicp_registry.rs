// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};

use crate::oicp::CapabilityProfile;
use commonwealth_core::{Error, Result};

/// A community-maintained OICP profile for a specific model+quantization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OicpProfileEntry {
    pub model_repo: String,
    pub quantization: String,
    pub profile_id: String,
    pub capabilities: CapabilityProfile,
    pub context_tokens: u32,
    #[serde(default)]
    pub notes: Option<String>,
}

/// A collection of OICP profiles (the community registry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OicpProfileRegistry {
    pub profiles: Vec<OicpProfileEntry>,
}

/// Parse a single OICP profile from TOML.
pub fn parse_profile(toml_str: &str) -> Result<OicpProfileEntry> {
    toml::from_str(toml_str)
        .map_err(|e| Error::Config(format!("failed to parse OICP profile: {e}")))
}

/// Parse a registry (multiple profiles) from TOML.
pub fn parse_registry(toml_str: &str) -> Result<OicpProfileRegistry> {
    toml::from_str(toml_str)
        .map_err(|e| Error::Config(format!("failed to parse OICP registry: {e}")))
}

/// Look up a profile by model repo and quantization.
pub fn lookup_profile<'a>(
    registry: &'a OicpProfileRegistry,
    repo: &str,
    quant: &str,
) -> Option<&'a OicpProfileEntry> {
    registry
        .profiles
        .iter()
        .find(|p| p.model_repo == repo && p.quantization == quant)
}
