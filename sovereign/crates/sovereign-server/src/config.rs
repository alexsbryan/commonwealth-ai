// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Server configuration, loaded from TOML + env overrides.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_server")]
    pub server: ServerSection,
    #[serde(default)]
    pub auth: AuthSection,
    pub inference: InferenceSection,
    #[serde(default = "default_store")]
    pub store: StoreSection,
    #[serde(default)]
    pub skills: SkillsSection,
    #[serde(default)]
    pub mcp: McpSection,
    #[serde(default)]
    pub commonwealth: CommonwealthSection,
    #[serde(default)]
    pub knowledge_view: KnowledgeViewSection,
    #[serde(default)]
    pub retrieval: RetrievalSection,
    #[serde(default)]
    pub iroh: IrohSection,
}

/// Dial-by-key access for clients (Track M of
/// `docs/specs/TRANSPORT_MIGRATION.md`). When enabled, the server
/// binds an iroh endpoint (QUIC by Ed25519 key, hole-punching, relay
/// fallback) and forwards accepted streams to the local HTTP
/// listener — a phone can then reach this host with no VPN. Off by
/// default; the tailnet path is unaffected either way.
///
/// NOT `sovereign_contracts::setup_config::IrohSection`: that one is the
/// daemon's tri-state AUTO (`Option<bool>`, on iff the node joined a mesh)
/// with per-class `transport` routing and no `key_path`. This server has no
/// mesh-participation marker, so its `[iroh]` is a plain on/off plus its
/// own identity path.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IrohSection {
    #[serde(default)]
    pub enabled: bool,
    /// Where the Ed25519 identity seed lives. Default: `node_key`
    /// beside the store DB (`store.path`'s directory), so a sandboxed
    /// host gets its own stable identity. Point this at the
    /// Commonwealth daemon's `<data_dir>/node_key` to share the mesh
    /// identity instead.
    #[serde(default)]
    pub key_path: Option<PathBuf>,
    /// Self-hosted iroh relays (W4 of TRANSPORT_MIGRATION.md). Empty =
    /// n0's public relays. Non-empty points this host's endpoint at the
    /// listed relays (e.g. a fleet's own `iroh-relay` on an allowlisted
    /// domain). Consumed by `build_relayed_endpoint`.
    #[serde(default)]
    pub relay_urls: Vec<String>,
    /// Sovereignty knob (H1). `"n0"`/absent = n0 relays + n0 DNS
    /// lookup; `"none"`/`"self"`/`"local"` = sever all n0 contact
    /// (self-hosted `relay_urls` and/or direct addrs only).
    #[serde(default)]
    pub discovery: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RetrievalSection {
    /// Allow-list of corpus ids this host searches. Empty/absent = search
    /// every installed corpus. When set, only these are enumerated by the
    /// engine — scoping both retrieval and the `/v1/corpora` listing — so
    /// a machine with experiment/partial/temp corpora doesn't pay to open
    /// or search the ones the operator doesn't want. Ids match the index
    /// directory name (the corpus_id for canonical installs).
    #[serde(default)]
    pub corpora: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Max concurrent inference turns — the real slot budget. Past it,
    /// turns queue (WS) or shed with `503 + Retry-After` (REST). See
    /// `crate::scheduler::FairScheduler`. Clamped to >= 1.
    #[serde(default = "default_max_concurrent_turns")]
    pub max_concurrent_turns: usize,
    /// Seconds advertised in `Retry-After` / the busy stream frame on shed.
    #[serde(default = "default_retry_after_secs")]
    pub retry_after_secs: u64,
    /// Per-origin concurrency cap — at most this many in-flight turns per
    /// tenant/peer, so one chatty origin can't hold every slot even when
    /// slots are free. The fairness floor. Clamped to >= 1.
    #[serde(default = "default_max_per_user")]
    pub max_per_user: u32,
    /// Max *waiting* turns before the scheduler sheds rather than growing
    /// the queue unboundedly — preserves the never-hang property.
    #[serde(default = "default_max_queue_depth")]
    pub max_queue_depth: usize,
    /// Reciprocity gain: queue weight = `1.0 + k · normalize(contribution)`.
    /// `0.0` disables reciprocity (pure FIFO within the per-origin cap);
    /// higher values let contributors jump the line further.
    #[serde(default = "default_reciprocity_k")]
    pub reciprocity_k: f64,
    /// Explicit opt-in to serve a non-loopback bind with auth disabled.
    /// Off by default: an unauthenticated LAN/tailnet surface must be a
    /// deliberate operator choice, so startup refuses the combination
    /// rather than warning past it. See `validate_exposure`.
    #[serde(default)]
    pub allow_unauthenticated_remote: bool,
    /// Browser CORS posture: `"auto"` (permissive only when auth is
    /// enabled), `"permissive"`, or `"off"`. Auto keeps a token-holding
    /// browser client working while denying cross-origin drive-by calls
    /// against an unauthenticated server — CORS is a browser-only gate,
    /// so native and mobile clients are unaffected either way.
    #[serde(default = "default_cors")]
    pub cors: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthSection {
    #[serde(default = "default_auth_mode")]
    pub mode: String,
    #[serde(default)]
    pub keys: HashMap<String, String>, // api_key → tenant_id
}

#[derive(Debug, Clone, Deserialize)]
pub struct InferenceSection {
    pub model: PathBuf,
    pub primary_model: Option<PathBuf>,
    /// Dedicated embedding model (e.g. `qwen-embedding-0.6b.gguf`). The chat
    /// model is the wrong tool for embeddings, and `load_dual` left this
    /// unset — so `embed()` errored and corpus retrieval was silently dead.
    /// We now ALWAYS load a real embed slot: when this is absent we default
    /// to `qwen-embedding-0.6b.gguf` co-located with the chat model (see
    /// `main.rs::resolve_embed_model`). It must match the dimension the
    /// installed corpora were embedded with (1024 for qwen-embedding-0.6b).
    #[serde(default)]
    pub embed_model: Option<PathBuf>,
    #[serde(default = "default_context_size")]
    pub context_size: u32,
    /// Response-length budget: max tokens generated per reply. The
    /// server-side equivalent of the desktop's "Response length"
    /// setting (`InferenceConfig.max_tokens`) — the knob the mobile
    /// cutoff chip / Continue affordance points at. Honoured by every
    /// synthesis path. Defaults to the core `InferenceConfig` default
    /// (2048) so existing configs are unchanged; raise it on a host
    /// whose clients ask for long-form answers.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    /// Multi-backend configuration. When present, overrides `model`/`primary_model`.
    #[serde(default)]
    pub backends: Vec<BackendConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackendConfig {
    pub name: String,
    /// Which engine serves this backend, in the workspace's engine
    /// vocabulary (`sovereign_core::setup_config::EngineKind`): `"llama"`
    /// or `"remote"`, plus `"embedded"` as the accepted legacy alias for
    /// `"llama"` (`main.rs::backend_engine_kind`). An unrecognised value is
    /// refused at startup rather than warned past — a skipped backend is a
    /// silently smaller fleet. Locality is derived from this.
    #[serde(rename = "type")]
    pub backend_type: String,
    #[serde(default = "default_priority")]
    pub priority: u32,
    // Embedded fields
    pub model: Option<PathBuf>,
    pub primary_model: Option<PathBuf>,
    // Remote fields
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub model_id: Option<String>,
    #[serde(default = "default_context_size")]
    pub context_size: u32,
}

fn default_priority() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreSection {
    #[serde(default = "default_store_path")]
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillsSection {
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpSection {
    #[serde(default)]
    pub servers: Vec<sovereign_tools::mcp::McpServerConfig>,
}

/// Commonwealth mesh integration. Optional — when absent, activity reporting
/// and mesh-routing features are disabled.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CommonwealthSection {
    /// URL of the local Commonwealth internal API (e.g. `http://127.0.0.1:9742`).
    pub url: Option<String>,
}

/// KnowledgeView master-toggle section. When `enabled = false` the
/// server skips the three enriched views (personal / conversational
/// / institutional) + cross-view resonance entirely — no ingest,
/// no observer, no landscape-digest splice. Equivalent to the
/// desktop app's Settings → Knowledge → "Enable KnowledgeView"
/// toggle. Default on; existing configs without the section read
/// as enabled.
///
/// TOML shape:
///
/// ```toml
/// [knowledge_view]
/// enabled = false
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeViewSection {
    #[serde(default = "default_knowledge_view_enabled")]
    pub enabled: bool,
}

impl Default for KnowledgeViewSection {
    fn default() -> Self {
        Self {
            enabled: default_knowledge_view_enabled(),
        }
    }
}

fn default_knowledge_view_enabled() -> bool {
    true
}

fn default_bind() -> String {
    "127.0.0.1:8080".to_string()
}
fn default_cors() -> String {
    "auto".to_string()
}

/// True when `bind` can only be reached from this machine. Fail-closed:
/// anything unparseable or non-loopback counts as remote.
pub fn bind_is_loopback(bind: &str) -> bool {
    if let Ok(addr) = bind.parse::<std::net::SocketAddr>() {
        return addr.ip().is_loopback();
    }
    // Hostname form — only `localhost:<port>` is known-loopback.
    matches!(bind.rsplit_once(':'), Some((host, _)) if host.eq_ignore_ascii_case("localhost"))
}

/// Startup exposure check: a non-loopback bind with auth disabled is
/// refused unless the operator opted in explicitly. Returning `Err` here
/// must abort startup — the whole point is that an open unauthenticated
/// listener can never happen by accident.
pub fn validate_exposure(server: &ServerSection, auth_enabled: bool) -> Result<(), String> {
    if bind_is_loopback(&server.bind) || auth_enabled || server.allow_unauthenticated_remote {
        return Ok(());
    }
    Err(format!(
        "refusing to serve on {} without authentication: every host that can reach \
         this machine could drive inference and read conversations. Fix one of: \
         configure `[auth] mode = \"api_key\"` with at least one key; bind loopback \
         (`[server] bind = \"127.0.0.1:8080\"`); or set `[server] \
         allow_unauthenticated_remote = true` if this network is a deliberate trust \
         boundary (e.g. a firewalled tailnet).",
        server.bind
    ))
}
fn default_max_concurrent_turns() -> usize {
    4
}
fn default_retry_after_secs() -> u64 {
    2
}
fn default_max_per_user() -> u32 {
    1
}
fn default_max_queue_depth() -> usize {
    32
}
fn default_reciprocity_k() -> f64 {
    0.5
}
fn default_auth_mode() -> String {
    "none".to_string()
}
fn default_context_size() -> u32 {
    2048
}
fn default_max_tokens() -> usize {
    // Matches `sovereign_core::types::InferenceConfig::default().max_tokens`
    // so a config without `[inference] max_tokens` behaves exactly as
    // before this field existed.
    2048
}
fn default_store_path() -> PathBuf {
    PathBuf::from("data/sovereign.db")
}
fn default_server() -> ServerSection {
    ServerSection {
        bind: default_bind(),
        max_concurrent_turns: default_max_concurrent_turns(),
        retry_after_secs: default_retry_after_secs(),
        max_per_user: default_max_per_user(),
        max_queue_depth: default_max_queue_depth(),
        reciprocity_k: default_reciprocity_k(),
        allow_unauthenticated_remote: false,
        cors: default_cors(),
    }
}
fn default_store() -> StoreSection {
    StoreSection {
        path: default_store_path(),
    }
}

impl ServerConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config {}: {e}", path.display()))?;

        let mut config: ServerConfig =
            toml::from_str(&content).map_err(|e| format!("Failed to parse config: {e}"))?;

        // Env var overrides.
        if let Ok(bind) = std::env::var("SOVEREIGN_BIND") {
            config.server.bind = bind;
        }
        if let Ok(model) = std::env::var("SOVEREIGN_MODEL") {
            config.inference.model = PathBuf::from(model);
        }
        if let Ok(embed) = std::env::var("SOVEREIGN_EMBED_MODEL") {
            config.inference.embed_model = Some(PathBuf::from(embed));
        }
        if let Ok(db_path) = std::env::var("SOVEREIGN_DB_PATH") {
            config.store.path = PathBuf::from(db_path);
        }

        Ok(config)
    }
}

#[cfg(test)]
mod exposure_tests {
    use super::*;

    fn server(bind: &str, allow_unauthenticated_remote: bool) -> ServerSection {
        let mut s = default_server();
        s.bind = bind.to_string();
        s.allow_unauthenticated_remote = allow_unauthenticated_remote;
        s
    }

    #[test]
    fn default_bind_is_loopback() {
        assert!(bind_is_loopback(&default_bind()));
    }

    #[test]
    fn loopback_binds_pass_without_auth() {
        for bind in [
            "127.0.0.1:8080",
            "[::1]:8080",
            "localhost:8080",
            "LOCALHOST:9000",
        ] {
            assert!(
                validate_exposure(&server(bind, false), false).is_ok(),
                "loopback bind {bind} must not require auth"
            );
        }
    }

    #[test]
    fn wildcard_binds_refused_without_auth() {
        assert!(validate_exposure(&server("0.0.0.0:8080", false), false).is_err());
        assert!(validate_exposure(&server("[::]:8080", false), false).is_err());
    }

    #[test]
    fn lan_ip_refused_without_auth() {
        assert!(validate_exposure(&server("192.168.1.20:8080", false), false).is_err());
    }

    #[test]
    fn unresolvable_hostname_is_fail_closed() {
        assert!(validate_exposure(&server("myhost.local:8080", false), false).is_err());
    }

    #[test]
    fn auth_unlocks_remote_bind() {
        assert!(validate_exposure(&server("0.0.0.0:8080", false), true).is_ok());
    }

    #[test]
    fn explicit_optin_unlocks_remote_bind() {
        assert!(validate_exposure(&server("0.0.0.0:8080", true), false).is_ok());
    }
}
