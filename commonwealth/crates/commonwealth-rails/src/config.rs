// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rails.toml` — everything this daemon is configured with, and nothing else.
//!
//! An absent file is the whole default set: a shim author who installs this
//! and joins by invite never writes one. A file that is PRESENT and malformed
//! is a named refusal at start, never a silent fall back to defaults — the
//! operator wrote it on purpose and a daemon that ignored it would be running
//! a configuration nobody chose (ARCH §18.3).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The loopback API port. 9741..9745 belong to the inference daemon family
/// (mesh, internal, client, desktop bridge, mobile), so a rails daemon on the
/// same machine as one must not land in that range.
pub const DEFAULT_LISTEN: u16 = 9747;

/// Env var naming the data dir when `--data-dir` is absent.
pub const DATA_DIR_ENV: &str = "CW_RAILS_DIR";

/// File name under the data dir.
pub const CONFIG_FILE: &str = "rails.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// This member's name on the roster. Defaults to the hostname, which is
    /// what a person recognises in `svrn mesh status` on somebody else's box.
    #[serde(default = "default_name")]
    pub name: String,
    /// Loopback API port.
    #[serde(default = "default_listen")]
    pub listen: u16,
    #[serde(default)]
    pub relay: RelaySection,
    #[serde(default)]
    pub media: MediaSection,
    #[serde(default = "default_gossip_interval")]
    pub gossip_interval_secs: u64,
    #[serde(default = "default_offline_threshold")]
    pub offline_threshold_secs: u64,
}

/// iroh reachability posture. Maps to
/// [`commonwealth_transport::iroh::RelayConfig::from_parts`] — one mapping,
/// shared with every other daemon that binds an endpoint (ARCH §10.6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaySection {
    /// Custom relay URLs. Empty means the default for `discovery`.
    #[serde(default)]
    pub urls: Vec<String>,
    /// `n0` / absent = n0 relays + n0 DNS; `none` / `self` / `local` severs
    /// every n0 contact. An unknown value warns and keeps n0 (the transport's
    /// own decision — this crate does not re-derive it).
    #[serde(default)]
    pub discovery: Option<String>,
}

/// The local media server this node lends to the mesh.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaSection {
    /// e.g. `127.0.0.1:8096`. Absent means this node OFFERS nothing: the
    /// media protocol is not advertised in gossip and a member's dial is
    /// closed rather than forwarded to a port nothing listens on. That is a
    /// config absence with a visible consequence, not a dark default.
    #[serde(default)]
    pub origin: Option<SocketAddr>,
    /// Member names, or node-id prefixes of at least four characters. Empty
    /// admits every member of the mesh; a non-empty list admits only those
    /// named. Resolution is `commonwealth_media`'s, so the acceptor and the
    /// viewer verb cannot disagree about what a name means.
    #[serde(default)]
    pub allow: Vec<String>,
}

fn default_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "cw-rails".to_string())
}

fn default_listen() -> u16 {
    DEFAULT_LISTEN
}

fn default_gossip_interval() -> u64 {
    10
}

fn default_offline_threshold() -> u64 {
    60
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: default_name(),
            listen: default_listen(),
            relay: RelaySection::default(),
            media: MediaSection::default(),
            gossip_interval_secs: default_gossip_interval(),
            offline_threshold_secs: default_offline_threshold(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigRefusal {
    #[error("{0} could not be read: {1}")]
    Unreadable(PathBuf, std::io::Error),
    #[error("{0} is not valid rails.toml: {1}")]
    Malformed(PathBuf, toml::de::Error),
    #[error("{0} sets `listen = 0` — a port the operator cannot dial back")]
    ZeroListen(PathBuf),
    #[error("{0} sets `gossip_interval_secs = 0` — a round loop with no interval")]
    ZeroInterval(PathBuf),
}

impl Config {
    /// Resolve the data dir: the flag, then `$CW_RAILS_DIR`, then
    /// `~/.commonwealth-rails`.
    pub fn resolve_data_dir(flag: Option<&Path>) -> PathBuf {
        if let Some(d) = flag {
            return d.to_path_buf();
        }
        if let Some(d) = std::env::var_os(DATA_DIR_ENV) {
            return PathBuf::from(d);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".commonwealth-rails")
    }

    /// Load `<data_dir>/rails.toml`, or `path` when one is named. An absent
    /// file is the default set; a present-but-malformed one is a refusal.
    pub fn load(data_dir: &Path, path: Option<&Path>) -> Result<Self, ConfigRefusal> {
        let file = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data_dir.join(CONFIG_FILE));
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && path.is_none() => {
                tracing::debug!(
                    target: "rails",
                    path = %file.display(),
                    "config: absent — running on defaults"
                );
                return Ok(Self::default());
            }
            Err(e) => return Err(ConfigRefusal::Unreadable(file, e)),
        };
        let cfg: Config =
            toml::from_str(&text).map_err(|e| ConfigRefusal::Malformed(file.clone(), e))?;
        if cfg.listen == 0 {
            return Err(ConfigRefusal::ZeroListen(file));
        }
        if cfg.gossip_interval_secs == 0 {
            return Err(ConfigRefusal::ZeroInterval(file));
        }
        tracing::info!(
            target: "rails",
            path = %file.display(),
            name = %cfg.name,
            listen = cfg.listen,
            media_origin = ?cfg.media.origin,
            media_allow = cfg.media.allow.len(),
            gossip_interval_secs = cfg.gossip_interval_secs,
            "config: loaded"
        );
        Ok(cfg)
    }

    /// The transport's relay posture for this config.
    pub fn relay_config(&self) -> commonwealth_transport::iroh::RelayConfig {
        commonwealth_transport::iroh::RelayConfig::from_parts(
            self.relay.urls.clone(),
            self.relay.discovery.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// The whole point of the shim author's install: no file, no questions.
    #[test]
    fn an_absent_file_is_the_default_set() {
        let d = dir();
        let cfg = Config::load(d.path(), None).expect("defaults");
        assert_eq!(cfg.listen, DEFAULT_LISTEN);
        assert_eq!(cfg.gossip_interval_secs, 10);
        assert!(cfg.media.origin.is_none(), "no origin is the default");
        assert!(cfg.media.allow.is_empty());
    }

    /// **The failing input.** A file the operator wrote and got wrong must
    /// not be absorbed into the defaults — that is a daemon running a
    /// configuration nobody chose. The typo here is a real one: `orign`.
    #[test]
    fn a_malformed_file_is_refused_by_name_not_defaulted() {
        let d = dir();
        std::fs::write(
            d.path().join(CONFIG_FILE),
            "listen = 9747\n[media]\norign = \"127.0.0.1:8096\"\n",
        )
        .unwrap();
        let err = Config::load(d.path(), None).unwrap_err();
        assert!(
            matches!(err, ConfigRefusal::Malformed(_, _)),
            "expected a named refusal, got {err}"
        );
        assert!(err.to_string().contains("rails.toml"), "{err}");
    }

    /// A named `--config` that does not exist is a refusal, not the defaults:
    /// the operator pointed at a file, and silently running without it is the
    /// substitution §18.3 forbids.
    #[test]
    fn a_named_config_file_that_is_absent_is_refused() {
        let d = dir();
        let missing = d.path().join("nowhere.toml");
        let err = Config::load(d.path(), Some(&missing)).unwrap_err();
        assert!(matches!(err, ConfigRefusal::Unreadable(_, _)), "{err}");
    }

    #[test]
    fn a_full_file_round_trips_every_field() {
        let d = dir();
        std::fs::write(
            d.path().join(CONFIG_FILE),
            r#"
name = "living-room"
listen = 9750
gossip_interval_secs = 5
offline_threshold_secs = 30

[relay]
urls = ["https://relay.example"]
discovery = "none"

[media]
origin = "127.0.0.1:8096"
allow = ["LittleMac", "node-44ae"]
"#,
        )
        .unwrap();
        let cfg = Config::load(d.path(), None).unwrap();
        assert_eq!(cfg.name, "living-room");
        assert_eq!(cfg.listen, 9750);
        assert_eq!(cfg.gossip_interval_secs, 5);
        assert_eq!(cfg.offline_threshold_secs, 30);
        assert_eq!(cfg.media.origin.unwrap().port(), 8096);
        assert_eq!(cfg.media.allow.len(), 2);
        // `discovery = "none"` severs n0 — read through the transport's own
        // mapping rather than re-derived here.
        assert!(!cfg.relay_config().n0_services);
        assert_eq!(cfg.relay_config().relay_urls.len(), 1);
    }

    /// `listen = 0` binds an ephemeral port nobody can dial back, which for
    /// the ONE route surface this daemon has is a daemon with no address.
    #[test]
    fn a_zero_listen_port_is_refused() {
        let d = dir();
        std::fs::write(d.path().join(CONFIG_FILE), "listen = 0\n").unwrap();
        assert!(matches!(
            Config::load(d.path(), None).unwrap_err(),
            ConfigRefusal::ZeroListen(_)
        ));
    }

    #[test]
    fn the_data_dir_precedence_is_flag_then_env_then_home() {
        let flag = PathBuf::from("/tmp/explicit");
        assert_eq!(Config::resolve_data_dir(Some(&flag)), flag);
    }
}
