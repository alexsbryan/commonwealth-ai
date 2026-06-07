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
    /// Max concurrent inference turns before the host returns
    /// `503 + Retry-After` (REST) / a busy stream frame (WS). See
    /// `crate::busy::BusyGuard`. Clamped to >= 1.
    #[serde(default = "default_max_concurrent_turns")]
    pub max_concurrent_turns: usize,
    /// Seconds advertised in `Retry-After` when busy.
    #[serde(default = "default_retry_after_secs")]
    pub retry_after_secs: u64,
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
    /// `"embedded"` or `"remote"`. Locality is derived from this — see
    /// `main.rs::build_backends`.
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
    "0.0.0.0:8080".to_string()
}
fn default_max_concurrent_turns() -> usize {
    4
}
fn default_retry_after_secs() -> u64 {
    2
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
