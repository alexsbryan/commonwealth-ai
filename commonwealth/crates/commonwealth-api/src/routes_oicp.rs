use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

/// GET /oicp/v1/capabilities — OICP provider manifest.
pub async fn capabilities(State(state): State<AppState>) -> Json<ProviderManifest> {
    let mesh = state.inner.mesh.read().await;
    let models = state.inner.models.read().await;
    let plan = state.inner.inference_plan.read().await;
    let addresses = state.inner.llama_server_addresses.read().await;

    let model_entries: Vec<OicpModelEntry> = models
        .values()
        .map(|model| {
            let shard_plan = plan.model_plans.iter().find(|p| p.model == model.id);
            let loaded = addresses.contains_key(&model.id);

            OicpModelEntry {
                id: model.name.clone(),
                quantization: model.quantization.clone(),
                capabilities: serde_json::to_value(&model.oicp_capabilities).unwrap_or_default(),
                context_tokens: 32768, // TODO: derive from model metadata
                status: OicpModelStatus {
                    available: true,
                    loaded,
                    estimated_tokens_per_sec: shard_plan
                        .map(|p| p.estimated_tokens_per_sec)
                        .unwrap_or(0.0),
                    estimated_ttft_ms: shard_plan.map(|p| p.estimated_ttft_ms).unwrap_or(0),
                },
            }
        })
        .collect();

    let peers: Vec<FederationPeer> = mesh
        .peers
        .iter()
        .map(|p| FederationPeer {
            name: p.peer_mesh_name.clone(),
            capabilities_url: p
                .contact_nodes
                .first()
                .map(|addr| format!("http://{}:9741/oicp/v1/capabilities", addr.ip()))
                .unwrap_or_default(),
            trust_level: format!("{:?}", p.trust_level).to_lowercase(),
        })
        .collect();

    Json(ProviderManifest {
        oicp_version: "0.2.0".into(),
        provider: ProviderInfo {
            name: mesh.name.clone(),
            provider_type: "mesh".into(),
        },
        models: model_entries,
        knowledge: KnowledgeManifest {
            corpora: vec![], // Populated in Phase 11.
            search_endpoint: "/v1/knowledge/search".into(),
        },
        federation: FederationManifest { peers },
    })
}

#[derive(Debug, Serialize)]
pub struct ProviderManifest {
    pub oicp_version: String,
    pub provider: ProviderInfo,
    pub models: Vec<OicpModelEntry>,
    pub knowledge: KnowledgeManifest,
    pub federation: FederationManifest,
}

#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: String,
}

#[derive(Debug, Serialize)]
pub struct OicpModelEntry {
    pub id: String,
    pub quantization: String,
    pub capabilities: serde_json::Value,
    pub context_tokens: u32,
    pub status: OicpModelStatus,
}

#[derive(Debug, Serialize)]
pub struct OicpModelStatus {
    pub available: bool,
    pub loaded: bool,
    pub estimated_tokens_per_sec: f32,
    pub estimated_ttft_ms: u32,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeManifest {
    pub corpora: Vec<serde_json::Value>,
    pub search_endpoint: String,
}

#[derive(Debug, Serialize)]
pub struct FederationManifest {
    pub peers: Vec<FederationPeer>,
}

#[derive(Debug, Serialize)]
pub struct FederationPeer {
    pub name: String,
    pub capabilities_url: String,
    pub trust_level: String,
}
