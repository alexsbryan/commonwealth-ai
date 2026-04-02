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
    #[serde(rename = "type")]
    pub backend_type: String, // "embedded" or "remote"
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default)]
    pub is_local: bool,
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
    #[serde(default = "default_store_mode")]
    pub mode: String,
    #[serde(default = "default_store_path")]
    pub path: PathBuf,
    /// PostgreSQL connection URL (when mode = "postgres").
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillsSection {
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpSection {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
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
fn default_store_mode() -> String {
    "sqlite".to_string()
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
        mode: default_store_mode(),
        path: default_store_path(),
        url: None,
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
