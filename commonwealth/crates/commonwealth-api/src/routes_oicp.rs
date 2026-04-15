use axum::extract::State;
use axum::Json;

use commonwealth_inference::oicp::{
    CorpusDescriptor, FederationManifest, KnowledgeManifest, ModelStatus, OICP_VERSION,
    PeerDescriptor, ProviderInfo, ProviderManifest, ProviderModel, ProviderType,
};

use crate::state::AppState;

/// GET /oicp/v1/capabilities — OICP provider manifest per spec §4.
pub async fn capabilities(State(state): State<AppState>) -> Json<ProviderManifest> {
    // If we have a local inference service (Sovereign's
    // EmbeddedLlamaCpp), prefer its manifest — that's the one
    // that actually reflects what we can serve. The scheduler-
    // based manifest below is for the standalone Commonwealth
    // daemon where llama-servers are spawned by the orchestrator;
    // in the Sovereign+mesh embed, those are empty.
    if let Some(local) = state.inner.local_inference.as_ref() {
        if let Some(mut manifest) = local.provider_manifest() {
            // Enrich provider name with the mesh name so peer
            // MeshAwareSelector can tell "this is BeefyMac's
            // Sovereign" vs a generic provider.
            let mesh = state.inner.mesh.read().await;
            if manifest.provider.is_none() {
                manifest.provider = Some(ProviderInfo {
                    name: Some(mesh.name.clone()),
                    provider_type: Some(ProviderType::Mesh),
                });
            }
            return Json(manifest);
        }
    }

    let mesh = state.inner.mesh.read().await;
    let models = state.inner.inference_store.list_models();
    let plan = state.inner.inference_store.get_plan().unwrap_or_default();

    let model_entries: Vec<ProviderModel> = models
        .values()
        .map(|model| {
            let shard_plan = plan.model_plans.iter().find(|p| p.model == model.id);
            let loaded = state
                .inner
                .inference_store
                .get_llama_address(model.id)
                .is_some();

            ProviderModel {
                id: model.name.clone(),
                base_model: None,
                quantization: if model.quantization.is_empty() {
                    None
                } else {
                    Some(model.quantization.clone())
                },
                capabilities: model.oicp_capabilities.clone(),
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded,
                    estimated_tokens_per_sec: shard_plan.map(|p| p.estimated_tokens_per_sec),
                    estimated_ttft_ms: shard_plan.map(|p| p.estimated_ttft_ms),
                    estimated_load_time_sec: None,
                },
            }
        })
        .collect();

    let peers: Vec<PeerDescriptor> = mesh
        .peers
        .iter()
        .map(|p| PeerDescriptor {
            name: p.peer_mesh_name.clone(),
            capabilities_url: p
                .contact_nodes
                .first()
                .map(|addr| format!("http://{}:9741/oicp/v1/capabilities", addr.ip()))
                .unwrap_or_default(),
            trust_level: Some(format!("{:?}", p.trust_level).to_lowercase()),
        })
        .collect();

    let federation = if peers.is_empty() {
        None
    } else {
        Some(FederationManifest { peers })
    };

    Json(ProviderManifest {
        oicp_version: OICP_VERSION.to_string(),
        provider: Some(ProviderInfo {
            name: Some(mesh.name.clone()),
            provider_type: Some(ProviderType::Mesh),
        }),
        models: model_entries,
        knowledge: Some(KnowledgeManifest {
            corpora: Vec::<CorpusDescriptor>::new(),
            search_endpoint: "/v1/knowledge/search".into(),
        }),
        federation,
    })
}
