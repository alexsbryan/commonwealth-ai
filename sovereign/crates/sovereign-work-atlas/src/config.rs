// SPDX-License-Identifier: AGPL-3.0-or-later
//! Operator-tunable settings. Defaults shipped as an in-crate asset
//! (data, not program — ARCH §6.2); operators override at
//! `~/.sovereign/work-atlas.toml`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::Privacy;

const DEFAULT_CONFIG_TOML: &str = include_str!("../assets/work_atlas_default_config.toml");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkAtlasConfig {
    pub node: NodeConfig,
    pub claims: ClaimsConfig,
    pub sessions: SessionsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub default_privacy: String,
}

impl NodeConfig {
    pub fn default_privacy_enum(&self) -> Privacy {
        Privacy::from_id(&self.default_privacy).unwrap_or(Privacy::Public)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimsConfig {
    pub default_ttl_seconds: u64,
    pub max_ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsConfig {
    pub idle_timeout_seconds: u64,
}

impl WorkAtlasConfig {
    /// Built-in defaults. Always succeeds — the asset is validated at
    /// build time via the toml parser.
    pub fn defaults() -> Self {
        toml::from_str(DEFAULT_CONFIG_TOML).expect("default work-atlas config asset is invalid")
    }

    /// Load from disk; fall back to defaults if the file doesn't
    /// exist. Operator errors (malformed toml) bubble up — better to
    /// fail loud at daemon start than silently mask the user's
    /// override.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(ConfigError::Toml),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::defaults()),
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    /// Clamp a requested TTL into `[1, max_ttl_seconds]`. Used by the
    /// `declare_scope` write surface.
    pub fn clamp_ttl(&self, requested: Option<u64>) -> u64 {
        let r = requested.unwrap_or(self.claims.default_ttl_seconds);
        r.clamp(1, self.claims.max_ttl_seconds)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("toml parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load_from_asset() {
        let cfg = WorkAtlasConfig::defaults();
        assert_eq!(cfg.claims.default_ttl_seconds, 14400);
        assert_eq!(cfg.claims.max_ttl_seconds, 86400);
        assert_eq!(cfg.sessions.idle_timeout_seconds, 14400);
        assert_eq!(cfg.node.default_privacy_enum(), Privacy::Public);
    }

    #[test]
    fn clamp_ttl_respects_max() {
        let cfg = WorkAtlasConfig::defaults();
        assert_eq!(cfg.clamp_ttl(None), 14400);
        assert_eq!(cfg.clamp_ttl(Some(60)), 60);
        // Above max → max.
        assert_eq!(cfg.clamp_ttl(Some(999999)), 86400);
        // Zero → at least 1.
        assert_eq!(cfg.clamp_ttl(Some(0)), 1);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = WorkAtlasConfig::load_or_default(&tmp.path().join("no.toml")).unwrap();
        assert_eq!(cfg.claims.default_ttl_seconds, 14400);
    }
}
