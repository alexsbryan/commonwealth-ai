//! Persistent configuration written by `sovereign setup` and read by
//! `sovereign daemon run`. Lives at `~/.config/sovereign/config.toml`
//! on Linux, or `~/Library/Application Support/sovereign/config.toml`
//! on macOS (whatever `dirs::config_dir()` resolves to) — distinct
//! from the project-level `.sovereign/sovereign.toml` which configures
//! per-project watchers.
//!
//! The split is deliberate: model paths and daemon ports are user-scoped,
//! while test/lint runners and workspace roots are project-scoped.
//!
//! This module used to live in `sovereign-cli`. It moved here so the
//! desktop app (which depends on `sovereign-core` but *not* on the CLI
//! binary crate) can read the same config and attach to a CLI-started
//! daemon without redefining the schema.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level structure of `~/.config/sovereign/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupConfig {
    pub models: ModelsSection,
    #[serde(default)]
    pub daemon: DaemonSection,
    #[serde(default)]
    pub data: DataSection,
}

/// Absolute paths to the loaded GGUF models. Three slots are
/// required (`fast` + `primary` + `embed`); a fourth optional slot
/// (`code`) is the specialization added in PR-E2, loaded lazily
/// when a code-hinted request arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsSection {
    /// The "primary" model — what the UX calls the Main responder.
    /// Internally this is the `thoughtful` profile slot.
    pub primary: PathBuf,
    pub fast: PathBuf,
    pub embed: PathBuf,
    /// Optional code-specialist model. When present, `code`-hinted
    /// inference requests route here instead of the primary. Lazy-
    /// loaded on first use and unloaded after the same idle window
    /// as the primary. `None` means the node relies on the primary
    /// model for code work (the common case; a well-rounded general
    /// model still handles code adequately per v0.3 §4.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<PathBuf>,

    /// Per-slot llama.cpp context size (n_ctx). `None` falls back to
    /// `default_context_size()` (16384), the conservative default that
    /// fits a 30B+ primary on a 64 GB Mac without OOMing the KV cache.
    /// Bump this on a Strix Halo (128 GB unified) or any box where the
    /// primary's KV cache + weights + fast slot still fit comfortably:
    /// 32768 roughly doubles output budget for atlas Phase 1, where
    /// long structured outputs were truncating against the 16384 cap.
    /// Applies to all loaded slots (fast / primary / embed / code) —
    /// per-slot override would need a richer schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u32>,
}

fn default_context_size() -> u32 {
    16384
}

impl ModelsSection {
    /// Effective n_ctx — the configured value, or the safe default.
    /// Use this anywhere you'd otherwise hardcode a context size, so
    /// cold-start and reload paths can't drift.
    pub fn effective_context_size(&self) -> u32 {
        self.context_size.unwrap_or_else(default_context_size)
    }
}

/// Network listener configuration. Defaults match the spec:
/// :9741 serves /v1 + /mcp, :9742 carries internal mesh gossip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonSection {
    #[serde(default = "default_client_port")]
    pub client_port: u16,
    #[serde(default = "default_internal_port")]
    pub internal_port: u16,
    /// When true, `sovereign setup` registers a launchd/systemd service
    /// so the daemon survives logout/restart.
    #[serde(default = "default_autostart")]
    pub autostart: bool,
    /// Idle seconds before the lazy primary slot is unloaded to reclaim
    /// VRAM. The default of 60s suits an interactive desktop where the
    /// model is touched once an hour; for batch workloads (the atlas
    /// enrich pipeline runs many short LLM calls back-to-back) it
    /// causes a 3–4 s reload tax between calls. Bump to 1800 (30 min)
    /// for batch hosts. Set to a very high number to effectively pin
    /// the slot for the daemon's lifetime.
    #[serde(default = "default_primary_idle_secs")]
    pub primary_idle_secs: u64,
}

/// Filesystem paths for mutable state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSection {
    /// Root of data directory. Models, indexes, notes, and mesh.json
    /// all live underneath. Default: `~/.sovereign`.
    #[serde(default = "default_data_dir")]
    pub dir: PathBuf,
}

impl Default for DaemonSection {
    fn default() -> Self {
        Self {
            client_port: default_client_port(),
            internal_port: default_internal_port(),
            autostart: default_autostart(),
            primary_idle_secs: default_primary_idle_secs(),
        }
    }
}

impl Default for DataSection {
    fn default() -> Self {
        Self { dir: default_data_dir() }
    }
}

fn default_client_port() -> u16 { 9741 }
fn default_internal_port() -> u16 { 9742 }
fn default_autostart() -> bool { true }
fn default_primary_idle_secs() -> u64 { 60 }

/// `~/.sovereign/`. Previously lived in `sovereign-cli::util::dirs`;
/// inlined here so `sovereign-core` has no dependency on the CLI crate.
/// Falls back to `.` if the home directory can't be resolved — matches
/// the prior behaviour.
fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".sovereign"))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl SetupConfig {
    /// The canonical XDG config path:
    /// - Linux: `~/.config/sovereign/config.toml`
    /// - macOS: `~/Library/Application Support/sovereign/config.toml`
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h| h.join(".config"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
            .join("sovereign")
            .join("config.toml")
    }

    /// Whether the config file exists on disk. Used by `sovereign setup`
    /// to short-circuit when the user has already configured, and by the
    /// desktop app's bootstrap probe to decide whether to skip the
    /// model-selection screens in the setup wizard.
    pub fn exists() -> bool {
        Self::default_path().exists()
    }

    /// Load from the canonical path.
    pub fn load() -> Result<Self, String> {
        let path = Self::default_path();
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let mut cfg: SetupConfig = toml::from_str(&content)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        cfg.expand_paths();
        Ok(cfg)
    }

    /// Write to the canonical path, creating parent directories as needed.
    /// Serializes with `toml::to_string_pretty` for human readability.
    pub fn save(&self) -> Result<PathBuf, String> {
        let path = Self::default_path();
        self.save_to(&path)?;
        Ok(path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let toml = toml::to_string_pretty(self)
            .map_err(|e| format!("serialize config: {e}"))?;
        std::fs::write(path, toml)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(())
    }

    /// Remove the config file. Used by `sovereign setup --reset`.
    pub fn remove() -> Result<(), String> {
        let path = Self::default_path();
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("remove {}: {e}", path.display()))?;
        }
        Ok(())
    }

    /// Expand leading `~` in all path fields to the user's home dir.
    /// TOML stores `~/.sovereign/...` literally; we resolve at load time.
    fn expand_paths(&mut self) {
        self.models.primary = expand_home(&self.models.primary);
        self.models.fast = expand_home(&self.models.fast);
        self.models.embed = expand_home(&self.models.embed);
        self.data.dir = expand_home(&self.data.dir);
    }
}

/// Resolve a `~/...` path to the user's home directory. Returns the
/// path unchanged if it doesn't start with `~` or home can't be found.
fn expand_home(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_minimal_config() {
        let cfg = SetupConfig {
            models: ModelsSection {
                primary: PathBuf::from("/models/primary.gguf"),
                fast: PathBuf::from("/models/fast.gguf"),
                embed: PathBuf::from("/models/embed.gguf"),
                code: None,
                context_size: None,
            },
            daemon: DaemonSection::default(),
            data: DataSection::default(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        cfg.save_to(&path).unwrap();
        let loaded = SetupConfig::load_from(&path).unwrap();
        assert_eq!(loaded.models.primary, cfg.models.primary);
        assert_eq!(loaded.daemon.client_port, 9741);
        assert_eq!(loaded.daemon.internal_port, 9742);
        assert!(loaded.daemon.autostart);
    }

    #[test]
    fn expand_home_resolves_tilde() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_home(Path::new("~/foo/bar")), home.join("foo/bar"));
        assert_eq!(expand_home(Path::new("/abs/path")), PathBuf::from("/abs/path"));
    }

    #[test]
    fn defaults_match_spec() {
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.daemon.client_port, 9741);
        assert_eq!(cfg.daemon.internal_port, 9742);
        assert!(cfg.daemon.autostart);
    }

    #[test]
    fn default_path_includes_sovereign_and_config_toml() {
        let p = SetupConfig::default_path();
        assert!(p.ends_with("sovereign/config.toml"), "unexpected path: {}", p.display());
    }
}
