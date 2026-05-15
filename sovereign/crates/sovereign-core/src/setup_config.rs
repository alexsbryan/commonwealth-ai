//! Persistent configuration written by `sovereign setup` and read by
//! `sovereign daemon run`. Lives at `~/.sovereign/config.toml` —
//! co-located with the rest of the user-scoped sovereign state (corpora,
//! indexes, notes db, mesh.json). Distinct from the project-level
//! `.sovereign/sovereign.toml` which configures per-project watchers.
//!
//! The split is: user-scoped state (this file + everything else under
//! `~/.sovereign/`) versus project-scoped state (test/lint runners,
//! workspace roots — in the repo's `.sovereign/sovereign.toml`).
//!
//! ## Legacy location
//!
//! Earlier versions wrote this file to `dirs::config_dir()/sovereign/
//! config.toml` (i.e. `~/.config/sovereign/...` on Linux,
//! `~/Library/Application Support/sovereign/...` on macOS). `load()` and
//! `exists()` transparently migrate the file from that path on first
//! access; no operator action is required.
//!
//! This module used to live in `sovereign-cli`. It moved here so the
//! desktop app (which depends on `sovereign-core` but *not* on the CLI
//! binary crate) can read the same config and attach to a CLI-started
//! daemon without redefining the schema.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level structure of `~/.sovereign/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupConfig {
    pub models: ModelsSection,
    #[serde(default)]
    pub daemon: DaemonSection,
    #[serde(default)]
    pub data: DataSection,
    /// Operator-tunable defaults for the watched-folder reconciliation
    /// scheduler. Per-corpus values stored in
    /// `WatchedFolderConfig` always win — this section just supplies
    /// the defaults that `corpus watch` uses when a CLI flag is
    /// omitted, plus a global `paused_at_boot` override for batch
    /// hosts that don't want sweeps running unattended.
    #[serde(default)]
    pub watched_folders: WatchedFoldersSection,
}

/// Absolute paths to the loaded GGUF models. Two slots are
/// required (`primary` + `embed`); `fast` is optional with a clean
/// subsume story (when unset, the primary model serves the fast role
/// — Speed::Fast requests land on the primary slot). A fourth slot
/// (`code`) is the optional PR-E2 specialization, loaded lazily when
/// a code-hinted request arrives.
///
/// Why `fast` is optional but `embed` is required: chat slots all
/// run the same `llama_decode` family, so primary can stand in for
/// fast (mmap weight-sharing makes the cost ~per-slot-KV-cache, not
/// per-weight-copy). Embedding is a different model class entirely
/// (Qwen3-Embedding vs Darwin/Qwen-instruct); the primary can't
/// substitute, so embed-less configs raise an invariant exception
/// at the first embed call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsSection {
    /// The "primary" model — what the UX calls the Main responder.
    /// Internally this is the `thoughtful` profile slot.
    pub primary: PathBuf,
    /// Optional fast/speed slot. When `None`, the primary slot
    /// subsumes the fast role — see [`Self::fast_path`] /
    /// [`Self::has_explicit_fast`]. The field stays private-ish in
    /// the API surface: callers should go through those methods
    /// instead of reading the Option directly, so the "subsume to
    /// primary" decision lives in one place. Right setting for
    /// VRAM-tight peers (e.g. L40S, 45 GB) where the primary alone
    /// (~30 GB Q6) leaves no room for a second model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<PathBuf>,
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
    /// Applies to all loaded slots (fast / primary / embed / code /
    /// extras) — per-slot override would need a richer schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u32>,

    /// Additional named chat slots loaded eagerly at daemon startup
    /// alongside primary/fast/embed/code. Keys are operator-chosen
    /// slot names; values are absolute paths to the GGUF files.
    /// Slots stay resident for the daemon's lifetime — they don't
    /// participate in the primary slot's idle-unload lifecycle.
    ///
    /// Routing: an inbound `/v1/chat/completions` request whose
    /// `model` field matches an extra slot's loaded model id (the
    /// gguf file stem) lands on that slot. Untagged requests still
    /// route via the existing Speed-based selection across
    /// fast/primary/code.
    ///
    /// Operators wire per-phase routing by combining this map with
    /// `EnrichConfig.chat_models` (corpus-side phase → model_id
    /// map). See `project_antifragile_pipeline_phase1.md` for the
    /// end-to-end picture.
    ///
    /// TOML shape:
    ///
    /// ```toml
    /// [models.extra]
    /// reasoning = "/path/to/Qwopus-27B-v3-Q6_K.gguf"
    /// bulk = "/path/to/Qwen3.5-9B.Q8_0.gguf"
    /// ```
    ///
    /// Empty map (the default) preserves the historical 3-slot
    /// behaviour for operators who haven't opted in.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, PathBuf>,

    /// Total memory budget for the eagerly-loaded extras lineup,
    /// in gigabytes. When set, an attempt to load a new extras slot
    /// that would push (existing extras' on-disk gguf size) +
    /// (new slot's gguf size) above this budget triggers LRU
    /// eviction: cold extras slots are dropped (in least-recently-
    /// used order, skipping any slot serving an in-flight request)
    /// until the new slot fits.
    ///
    /// `None` (the default) disables eviction entirely — the
    /// pre-LRU behaviour. Operators on tight VRAM (e.g. a 64 GB Mac
    /// running Qwopus-27B Q6 ≈ 21 GB primary + Qwen3.5-9B Q8 ≈ 9 GB
    /// extras + embed ≈ 1 GB + OS + KV cache) set this to a value
    /// like 12.0 to keep the cumulative extras footprint bounded
    /// without dropping the primary.
    ///
    /// Note: only the gguf size on disk is counted, NOT the KV
    /// cache or activation buffers. Real GPU memory use is roughly
    /// `gguf_size + n_ctx * model_size_factor`. Pad the budget
    /// accordingly — `max_extras_memory_gb` is a conservative
    /// upper bound on weights, not a strict OS-level limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_extras_memory_gb: Option<f32>,

    /// Multi-primary slot pool. When set, the daemon loads `copies`
    /// independent instances of `path` as separate primary-class slots
    /// at startup, in addition to the singleton `primary` field.
    /// Inbound chat-completion requests targeting Speed::Slow /
    /// Speed::Normal are dispatched round-robin across the pool, so a
    /// host with sufficient VRAM (e.g. MI300X 192 GB) can serve N
    /// concurrent Phase 1 extracts without queueing.
    ///
    /// `None` (the default) preserves single-primary behaviour. Use
    /// case is bulk ingest workers that want horizontal parallelism on
    /// one box; a 6-copy pool of Darwin-36B-Q6 (28 GB each) fits in
    /// 168 GB and yields ~6× throughput vs. a single slot.
    ///
    /// TOML shape:
    /// ```toml
    /// [models.primary_pool]
    /// copies = 6
    /// path = "/workspace/models/Darwin-36B-Opus-Q6_K.gguf"
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_pool: Option<PrimaryPoolSection>,
}

/// Multi-primary slot pool config. See `ModelsSection::primary_pool`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimaryPoolSection {
    /// Number of additional primary-class slot copies to spawn at
    /// daemon startup. The singleton `primary` (always loaded) counts
    /// as one slot; `copies` of those plus the singleton total
    /// `1 + copies` primary slots. `0` is treated as no pool.
    pub copies: u32,
    /// GGUF path for each pool member. Today every copy points at the
    /// same file; future variants could carry per-slot model paths.
    pub path: PathBuf,
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

    /// Path the fast-slot loader should use, with primary as the
    /// fallback when no distinct fast model is configured. Callers
    /// that need to know whether a fast model is configured *as
    /// opposed to* subsumed by primary should use
    /// [`Self::has_explicit_fast`] alongside this. Encapsulates the
    /// subsume rule in one place so the rest of the codebase can
    /// treat fast as if it always existed.
    pub fn fast_path(&self) -> &Path {
        self.fast.as_deref().unwrap_or(&self.primary)
    }

    /// `true` when an explicit fast GGUF is configured (the
    /// `[models].fast` key is set in `config.toml`). `false` when
    /// the primary subsumes the fast role.
    ///
    /// Use when the distinction matters — e.g. VRAM accounting
    /// (don't double-count primary's weights), mesh advertising
    /// (don't list a duplicate `fast` slot to peers when it's just
    /// an alias), reload-diff (a change from None to Some/primary-
    /// equal is operator-visible). For path lookups, prefer
    /// [`Self::fast_path`].
    pub fn has_explicit_fast(&self) -> bool {
        self.fast.is_some()
    }

    /// Memory budget in raw bytes for the extras lineup.
    /// `None` when unset — caller treats that as "no eviction".
    /// Returned in bytes so the eviction policy can compare directly
    /// against per-slot `size_bytes`.
    pub fn max_extras_memory_bytes(&self) -> Option<u64> {
        self.max_extras_memory_gb.map(|gb| {
            // 1 GiB = 2^30 bytes. Saturate to u64::MAX on absurd
            // inputs rather than overflow.
            let bytes = (gb as f64) * (1u64 << 30) as f64;
            if bytes <= 0.0 {
                0
            } else if bytes >= u64::MAX as f64 {
                u64::MAX
            } else {
                bytes as u64
            }
        })
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
    /// VRAM. Default 300s (5 min) — long enough that mid-conversation
    /// pauses (read the answer, think, formulate follow-up) don't
    /// cause an eviction-then-reload that re-pays the 10–90s
    /// model-load wait on the next turn, short enough that an
    /// abandoned session frees memory within a single coffee break.
    /// For batch workloads (the atlas enrich pipeline runs many
    /// short LLM calls back-to-back) bump to 1800 (30 min). Set to
    /// a very high number to effectively pin the slot for the
    /// daemon's lifetime; combined with the desktop's window-focus
    /// prewarm, that gives "always-hot" semantics at the cost of
    /// pinning ~28 GB for a 35B Q6.
    #[serde(default = "default_primary_idle_secs")]
    pub primary_idle_secs: u64,

    /// Idle seconds before an unused **extras** chat slot is unloaded
    /// to reclaim VRAM. Defaults to `0` — eviction-driven only, no
    /// idle drop. Set to a positive value (e.g. `1800` for 30 min) to
    /// have a background task drop extras slots that haven't served a
    /// request in that long, even when no other load is competing for
    /// memory. Useful on shared hardware where another user might
    /// need the GPU back.
    ///
    /// Slots currently serving a request (Arc strong_count > 1) are
    /// skipped. The check runs every 10s; the actual idle window is
    /// `max(extras_idle_secs, 10)`.
    #[serde(default = "default_extras_idle_secs")]
    pub extras_idle_secs: u64,

    /// Cooperative yield window for background corpus ingestion. When
    /// the daemon serves a foreground inference request on
    /// `/v1/chat/completions`, it stamps a "last-active" timestamp;
    /// the ingest pipeline polls this before each embed batch and
    /// enrichment phase and pauses while the timestamp is within
    /// `yield_to_foreground_secs` seconds of now.
    ///
    /// Why: an embed batch is an atomic `llama_decode` that can hold
    /// the GPU for ~7s. While it runs the primary chat slot can't
    /// interleave tokens, so chat latency collapses (1 tok/s instead
    /// of 7+ on Q1_0 8B class models). Yielding between batches
    /// frees the GPU for the user without cancelling ongoing
    /// ingest work.
    ///
    /// `0` (or unset) disables the feature entirely — appropriate
    /// for batch hosts that never serve interactive chat. Default
    /// `60` covers the common case where a user might issue a chat
    /// query and follow it up within a minute. Bump to 120-180 for
    /// "stay paused while I'm working" behavior, drop to 30 for
    /// "resume quickly between exchanges".
    #[serde(default = "default_yield_to_foreground_secs")]
    pub yield_to_foreground_secs: u64,

    /// When true, the inference adapter treats every tools-using
    /// request with `tool_choice: "auto"` (or unset) as if the caller
    /// had sent `tool_choice: "required"`, so the JSON-Schema
    /// tool-envelope grammar engages. Identical to setting the env var
    /// `SOVEREIGN_FORCE_TOOL_CALLS=1` — env wins when both are set,
    /// otherwise this config flag drives the same code path. Empirically
    /// (2026-05-08 measurement) the grammar pass eliminates Qwopus and
    /// FINAL-Bench native-markup parse failures (`<tool_call>{...}` with
    /// arguments-as-string-of-json) at zero per-call cost — the
    /// `JsonConstraint` path is already optimised for these short
    /// envelopes.
    ///
    /// Defaults to `false` so the disabled-default keeps existing
    /// non-tools-using callers unaffected. Set to `true` in
    /// `setup_config.toml` for daemons that primarily host opencode /
    /// Aider / autonomous-loop traffic.
    #[serde(default = "default_force_tool_calls")]
    pub force_tool_calls: bool,
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
            extras_idle_secs: default_extras_idle_secs(),
            yield_to_foreground_secs: default_yield_to_foreground_secs(),
            force_tool_calls: default_force_tool_calls(),
        }
    }
}

impl Default for DataSection {
    fn default() -> Self {
        Self { dir: default_data_dir() }
    }
}

/// Defaults for watched-folder corpora (`sovereign corpus watch`).
/// Per-corpus values from `WatchedFolderConfig` override these — the
/// only setting here that's *not* overridable per-corpus is
/// `paused_at_boot`, which is an operator/host-level decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFoldersSection {
    #[serde(default = "default_wf_sweep_interval_secs")]
    pub default_sweep_interval_secs: u64,
    #[serde(default = "default_wf_grace_secs")]
    pub default_soft_delete_grace_secs: u64,
    #[serde(default = "default_wf_absolute_threshold")]
    pub default_absolute_threshold: usize,
    #[serde(default = "default_wf_fractional_threshold")]
    pub default_fractional_threshold: f32,
    /// When `true`, all watched-folder corpora start in
    /// `PausedManual` on daemon boot. Use on batch hosts where the
    /// operator wants to inspect status before the scheduler starts
    /// sweeping unattended.
    #[serde(default)]
    pub paused_at_boot: bool,
    /// Cap on the number of watched-folder sweeps that may run
    /// concurrently. Default 2 — bounded so a user with several
    /// large folders doesn't fan out unboundedly.
    #[serde(default = "default_wf_max_concurrent")]
    pub max_concurrent_sweeps: usize,
}

impl Default for WatchedFoldersSection {
    fn default() -> Self {
        Self {
            default_sweep_interval_secs: default_wf_sweep_interval_secs(),
            default_soft_delete_grace_secs: default_wf_grace_secs(),
            default_absolute_threshold: default_wf_absolute_threshold(),
            default_fractional_threshold: default_wf_fractional_threshold(),
            paused_at_boot: false,
            max_concurrent_sweeps: default_wf_max_concurrent(),
        }
    }
}

fn default_wf_sweep_interval_secs() -> u64 { 120 }
fn default_wf_grace_secs() -> u64 { 7 * 86_400 }
fn default_wf_absolute_threshold() -> usize { 100 }
fn default_wf_fractional_threshold() -> f32 { 0.25 }
fn default_wf_max_concurrent() -> usize { 2 }

fn default_client_port() -> u16 { 9741 }
fn default_internal_port() -> u16 { 9742 }
fn default_autostart() -> bool { true }
fn default_primary_idle_secs() -> u64 { 300 }
/// Default `0` keeps existing operators on the historical "extras
/// stay loaded forever" behaviour — they explicitly opt in by
/// setting a positive value.
fn default_extras_idle_secs() -> u64 { 0 }
/// Default `60` enables foreground-yield with a one-minute window:
/// background ingest pauses for a minute after each chat request, then
/// resumes. Set to `0` in `config.toml` to disable on batch hosts
/// where ingest throughput trumps interactive latency.
fn default_yield_to_foreground_secs() -> u64 { 60 }

fn default_force_tool_calls() -> bool { false }

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
    /// The canonical config path: `~/.sovereign/config.toml`. Co-located
    /// with `~/.sovereign/`'s other user-scoped state (corpora, indexes,
    /// notes db, mesh.json) so operators only have one user directory
    /// to remember. Falls back to `./.sovereign/config.toml` if the home
    /// directory can't be resolved — matches `default_data_dir()`.
    pub fn default_path() -> PathBuf {
        default_data_dir().join("config.toml")
    }

    /// The pre-consolidation location: `dirs::config_dir()/sovereign/
    /// config.toml` (`~/.config/sovereign/...` on Linux,
    /// `~/Library/Application Support/sovereign/...` on macOS). Returned
    /// for migration only; new writes always go to `default_path()`.
    pub fn legacy_default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .map(|h| h.join(".config"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
            .join("sovereign")
            .join("config.toml")
    }

    /// If the canonical path doesn't exist but the legacy path does,
    /// move the file. Idempotent — after the first call the legacy path
    /// is gone, so subsequent calls are no-ops. Errors are logged via
    /// `eprintln!` and swallowed so a migration hiccup never blocks
    /// daemon startup; the caller will hit a normal "config not found"
    /// error path if the legacy file genuinely can't be migrated.
    fn migrate_legacy_if_needed() {
        migrate_config_between(&Self::legacy_default_path(), &Self::default_path());
    }

    /// Whether the config file exists on disk. Used by `sovereign setup`
    /// to short-circuit when the user has already configured, and by the
    /// desktop app's bootstrap probe to decide whether to skip the
    /// model-selection screens in the setup wizard.
    pub fn exists() -> bool {
        Self::migrate_legacy_if_needed();
        Self::default_path().exists()
    }

    /// Load from the canonical path.
    pub fn load() -> Result<Self, String> {
        Self::migrate_legacy_if_needed();
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
        if let Some(fast) = &self.models.fast {
            self.models.fast = Some(expand_home(fast));
        }
        self.models.embed = expand_home(&self.models.embed);
        if let Some(p) = self.models.code.as_mut() {
            *p = expand_home(p);
        }
        for path in self.models.extra.values_mut() {
            *path = expand_home(path);
        }
        self.data.dir = expand_home(&self.data.dir);
    }
}

/// Move `legacy → new_path` iff legacy exists and new_path doesn't.
/// Pure on its inputs so tests can drive it without overriding `$HOME`.
/// Logs to stderr on success or failure; never panics.
fn migrate_config_between(legacy: &Path, new_path: &Path) {
    if new_path.exists() || !legacy.exists() || new_path == legacy {
        return;
    }
    if let Some(parent) = new_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("sovereign: migrate config: create {}: {e}", parent.display());
            return;
        }
    }
    match std::fs::rename(legacy, new_path) {
        Ok(()) => eprintln!(
            "sovereign: migrated config {} → {}",
            legacy.display(),
            new_path.display()
        ),
        Err(e) => eprintln!(
            "sovereign: migrate config {} → {} failed: {e}",
            legacy.display(),
            new_path.display()
        ),
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

    fn models(primary: &str, fast: Option<&str>, embed: &str) -> ModelsSection {
        ModelsSection {
            primary: PathBuf::from(primary),
            fast: fast.map(PathBuf::from),
            embed: PathBuf::from(embed),
            code: None,
            context_size: None,
            extra: BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
        }
    }

    #[test]
    fn fast_path_returns_primary_when_fast_unset() {
        let m = models("/models/primary.gguf", None, "/models/embed.gguf");
        assert_eq!(m.fast_path(), Path::new("/models/primary.gguf"));
        assert!(!m.has_explicit_fast());
    }

    #[test]
    fn fast_path_returns_explicit_fast_when_set() {
        let m = models(
            "/models/primary.gguf",
            Some("/models/fast.gguf"),
            "/models/embed.gguf",
        );
        assert_eq!(m.fast_path(), Path::new("/models/fast.gguf"));
        assert!(m.has_explicit_fast());
    }

    #[test]
    fn parse_config_without_fast_field_succeeds() {
        // The pod entrypoint writes a `[models]` table with only
        // primary + embed when SINGLE_MODEL=primary is set. Before
        // this commit, deserializing that TOML failed with
        // "missing field `fast`" and killed every Vast.ai pod at
        // the daemon-launch stage. Lock the now-Optional behaviour
        // in so a future refactor can't silently reintroduce the
        // hard requirement.
        let toml = r#"
[models]
primary = "/models/primary.gguf"
embed = "/models/embed.gguf"

[daemon]
[data]
"#;
        let cfg: SetupConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.models.primary, PathBuf::from("/models/primary.gguf"));
        assert!(cfg.models.fast.is_none());
        assert_eq!(cfg.models.fast_path(), Path::new("/models/primary.gguf"));
    }

    #[test]
    fn roundtrip_minimal_config() {
        let cfg = SetupConfig {
            models: ModelsSection {
                primary: PathBuf::from("/models/primary.gguf"),
                fast: Some(PathBuf::from("/models/fast.gguf")),
                embed: PathBuf::from("/models/embed.gguf"),
                code: None,
                context_size: None,
                extra: BTreeMap::new(),
                max_extras_memory_gb: None,
                primary_pool: None,
            },
            daemon: DaemonSection::default(),
            data: DataSection::default(),
            watched_folders: WatchedFoldersSection::default(),
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
    fn yield_to_foreground_secs_defaults_to_60() {
        // A config that omits the field must come back with the
        // 60-second default so existing operators get yield enabled
        // without editing config.toml. Lock the default here so a
        // future bump (or zero-out) is intentional and reviewed.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.daemon.yield_to_foreground_secs, 60);
    }

    #[test]
    fn yield_to_foreground_secs_explicit_override() {
        // Operators can set 0 to disable, or a higher value for a
        // longer pause window.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[daemon]
yield_to_foreground_secs = 0
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.daemon.yield_to_foreground_secs, 0);
    }

    #[test]
    fn default_path_includes_sovereign_and_config_toml() {
        // Post-consolidation (2026-05-10): config lives at
        // `~/.sovereign/config.toml`, not `~/sovereign/config.toml`.
        // Path::ends_with matches whole components, so the dotted
        // directory needs the dot literal in the predicate.
        // `default_path()` resolves to `~/.sovereign/config.toml` —
        // hidden directory, per the canonical layout consolidated by
        // the `default_data_dir()` migration (the doc comment on
        // `default_path` is normative). The earlier assertion used
        // the bare `sovereign/config.toml` form which only matched
        // the legacy `~/.config/sovereign/config.toml` layout, so it
        // failed on any environment that had migrated. Match the
        // canonical hidden-dir layout instead.
        let p = SetupConfig::default_path();
        assert!(
            p.ends_with(".sovereign/config.toml"),
            "unexpected path: {}",
            p.display()
        );
    }

    #[test]
    fn migrate_moves_legacy_when_new_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy/config.toml");
        let new_path = tmp.path().join("new/config.toml");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "primary=\"/m/p.gguf\"\n").unwrap();

        migrate_config_between(&legacy, &new_path);

        assert!(new_path.exists(), "new path should exist after migration");
        assert!(!legacy.exists(), "legacy path should be gone after migration");
        assert_eq!(
            std::fs::read_to_string(&new_path).unwrap(),
            "primary=\"/m/p.gguf\"\n"
        );
    }

    #[test]
    fn migrate_is_noop_when_new_path_already_exists() {
        // Operator has already migrated (or is on a fresh install): the
        // legacy file may still exist as a stale leftover, but we must
        // not clobber the canonical file.
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy/config.toml");
        let new_path = tmp.path().join("new/config.toml");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(new_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "stale\n").unwrap();
        std::fs::write(&new_path, "canonical\n").unwrap();

        migrate_config_between(&legacy, &new_path);

        assert_eq!(std::fs::read_to_string(&new_path).unwrap(), "canonical\n");
        assert!(legacy.exists(), "legacy untouched when new path present");
    }

    #[test]
    fn migrate_is_noop_when_legacy_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy/config.toml");
        let new_path = tmp.path().join("new/config.toml");
        // Neither file exists.
        migrate_config_between(&legacy, &new_path);
        assert!(!new_path.exists());
    }

    #[test]
    fn extra_slots_default_empty_when_absent() {
        // Operators upgrading the binary keep their existing
        // config.toml. The `serde(default)` on `extra` means a config
        // without the `[models.extra]` table loads with an empty
        // map — preserving the legacy 3-slot lineup.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.models.extra.is_empty());
    }

    #[test]
    fn extra_slots_parse_from_toml_table() {
        // `[models.extra]` table → BTreeMap<String, PathBuf>.
        // BTreeMap iteration order is sorted, which makes startup
        // logging deterministic across reboots.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[models.extra]
reasoning = "/m/big.gguf"
bulk = "/m/small.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.models.extra.len(), 2);
        assert_eq!(
            cfg.models.extra.get("reasoning"),
            Some(&PathBuf::from("/m/big.gguf"))
        );
        assert_eq!(
            cfg.models.extra.get("bulk"),
            Some(&PathBuf::from("/m/small.gguf"))
        );
    }

    #[test]
    fn max_extras_memory_bytes_unset_returns_none() {
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.models.max_extras_memory_bytes().is_none());
    }

    #[test]
    fn max_extras_memory_bytes_converts_gigabytes() {
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
max_extras_memory_gb = 12.0
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        // 12 GiB = 12 * 2^30 bytes.
        assert_eq!(
            cfg.models.max_extras_memory_bytes(),
            Some(12 * (1u64 << 30))
        );
    }

    #[test]
    fn max_extras_memory_bytes_saturates_on_negative_input() {
        // Defensive: a negative or zero budget effectively forbids
        // any extras — return Some(0) rather than panicking or
        // overflowing.
        let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
max_extras_memory_gb = 0.0
"#;
        let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.models.max_extras_memory_bytes(), Some(0));
    }

    #[test]
    fn extra_slots_expand_home_at_load() {
        // `~/...` paths inside `[models.extra]` resolve like the
        // primary/fast/embed paths do — load-time expansion via
        // `expand_paths`.
        let home = dirs::home_dir().unwrap();
        let toml_str = r#"
[models]
primary = "~/dev/primary.gguf"
fast = "/abs/fast.gguf"
embed = "~/dev/embed.gguf"

[models.extra]
reasoning = "~/dev/big.gguf"
"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, toml_str).unwrap();
        let cfg = SetupConfig::load_from(&path).unwrap();
        assert_eq!(cfg.models.extra.get("reasoning"), Some(&home.join("dev/big.gguf")));
    }
}
