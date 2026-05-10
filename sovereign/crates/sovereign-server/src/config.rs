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
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    #[serde(default = "default_bind")]
    pub bind: String,
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
    #[serde(default = "default_context_size")]
    pub context_size: u32,
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
fn default_auth_mode() -> String {
    "none".to_string()
}
fn default_context_size() -> u32 {
    2048
}
fn default_store_path() -> PathBuf {
    PathBuf::from("data/sovereign.db")
}
fn default_server() -> ServerSection {
    ServerSection {
        bind: default_bind(),
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
        if let Ok(db_path) = std::env::var("SOVEREIGN_DB_PATH") {
            config.store.path = PathBuf::from(db_path);
        }

        Ok(config)
    }
}
