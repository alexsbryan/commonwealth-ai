use serde::{Deserialize, Serialize};

use crate::ledger::FairnessPolicy;

/// Per-node daemon configuration. Loaded from `~/.commonwealth/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub node: NodeConfig,
    #[serde(default)]
    pub contribution: ContributionConfig,
    #[serde(default)]
    pub inference: InferenceConfig,
    #[serde(default)]
    pub knowledge: KnowledgeConfig,
    #[serde(default)]
    pub fairness: FairnessConfig,
    #[serde(default)]
    pub network: NetworkConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub name: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    #[serde(default = "default_internal_port")]
    pub internal_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionConfig {
    #[serde(default = "default_schedule")]
    pub schedule: String,
    #[serde(default)]
    pub reserve_vram_gb: u32,
    #[serde(default)]
    pub reserve_ram_gb: u32,
    #[serde(default)]
    pub reserve_storage_gb: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceConfig {
    #[serde(default = "default_llama_server")]
    pub llama_server: String,
    #[serde(default = "default_rpc_server")]
    pub rpc_server: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeConfig {
    #[serde(default = "default_index_dir")]
    pub index_dir: String,
    #[serde(default)]
    pub grounding: GroundingConfig,
}

/// Configuration for knowledge grounding of non-OICP requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundingConfig {
    #[serde(default = "default_grounding_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub corpora: Vec<String>,
    #[serde(default = "default_max_chunks")]
    pub max_chunks: usize,
    #[serde(default = "default_min_relevance")]
    pub min_relevance: f32,
}

fn default_grounding_enabled() -> bool {
    true
}
fn default_max_chunks() -> usize {
    5
}
fn default_min_relevance() -> f32 {
    0.65
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FairnessConfig {
    #[serde(default)]
    pub policy: FairnessPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub vpn_interface: Option<String>,
}

/// Mesh-wide configuration, propagated via gossip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshConfig {
    pub policy: MeshPolicyConfig,
    #[serde(default)]
    pub models: MeshModelsConfig,
    #[serde(default)]
    pub corpora: MeshCorporaConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPolicyConfig {
    #[serde(default)]
    pub fairness: FairnessPolicy,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_requests: u32,
    #[serde(default = "default_redundancy")]
    pub redundancy_target: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferredModel {
    pub repo: String,
    pub quant: String,
    pub oicp_profile: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeshModelsConfig {
    #[serde(default)]
    pub preferred: Vec<PreferredModel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeshCorporaConfig {
    #[serde(default)]
    pub preferred: Vec<String>,
}

// Defaults

fn default_data_dir() -> String {
    "~/.commonwealth".into()
}
fn default_api_port() -> u16 {
    9741
}
fn default_internal_port() -> u16 {
    9742
}
fn default_schedule() -> String {
    "always".into()
}
fn default_llama_server() -> String {
    "llama-server".into()
}
fn default_rpc_server() -> String {
    "rpc-server".into()
}
fn default_index_dir() -> String {
    "~/.sovereign/indexes".into()
}
fn default_max_concurrent() -> u32 {
    10
}
fn default_redundancy() -> usize {
    2
}

impl Default for ContributionConfig {
    fn default() -> Self {
        Self {
            schedule: default_schedule(),
            reserve_vram_gb: 4,
            reserve_ram_gb: 8,
            reserve_storage_gb: 50,
        }
    }
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            llama_server: default_llama_server(),
            rpc_server: default_rpc_server(),
        }
    }
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            index_dir: default_index_dir(),
            grounding: GroundingConfig::default(),
        }
    }
}

impl Default for GroundingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            corpora: Vec::new(),
            max_chunks: 5,
            min_relevance: 0.65,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_config_from_toml() {
        let toml_str = r#"
[node]
name = "Alice's Desktop"
data_dir = "~/.commonwealth"
api_port = 9741
internal_port = 9742

[contribution]
schedule = "always"
reserve_vram_gb = 4
reserve_ram_gb = 8
reserve_storage_gb = 50

[inference]
llama_server = "/usr/local/bin/llama-server"
rpc_server = "/usr/local/bin/rpc-server"

[knowledge]
index_dir = "~/.commonwealth/indexes"

[fairness]
policy = { type = "transparent" }

[network]
vpn_interface = "wg0"
"#;
        let config: DaemonConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.node.name, "Alice's Desktop");
        assert_eq!(config.node.api_port, 9741);
        assert_eq!(config.contribution.reserve_vram_gb, 4);
        assert_eq!(config.network.vpn_interface.as_deref(), Some("wg0"));
    }

    #[test]
    fn daemon_config_defaults() {
        let toml_str = r#"
[node]
name = "Test Node"
"#;
        let config: DaemonConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.node.api_port, 9741);
        assert_eq!(config.node.internal_port, 9742);
        assert_eq!(config.contribution.schedule, "always");
        assert_eq!(config.inference.llama_server, "llama-server");
    }

    #[test]
    fn mesh_config_from_toml() {
        let toml_str = r#"
[policy]
fairness = { type = "transparent" }
max_concurrent_requests = 10
redundancy_target = 2

[models]
preferred = [
    { repo = "Qwen/Qwen3-Coder-30B-GGUF", quant = "Q4_K_M", oicp_profile = "qwen/qwen3-coder-30b-Q4_K_M" },
]

[corpora]
preferred = ["wikipedia", "openalex"]
"#;
        let config: MeshConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.policy.max_concurrent_requests, 10);
        assert_eq!(config.models.preferred.len(), 1);
        assert_eq!(config.corpora.preferred.len(), 2);
    }
}
