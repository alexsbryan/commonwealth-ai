use axum::extract::State;
use axum::Json;

use commonwealth_inference::oicp::{
    Capability, CapabilityClaim, CapabilityHint, CapabilityProfile,
    CorpusDescriptor, FederationManifest, KnowledgeManifest, LatencyClass,
    ModelStatus, PeerDescriptor, ProviderInfo, ProviderManifest, ProviderModel,
    ProviderType, OICP_VERSION,
};

use crate::state::AppState;

/// Synthesize a v0.3 `CapabilityClaim` for a model from its name and
/// v0.2 capability profile. Two-step heuristic for PR-B (replaced by
/// structured model config in PR-E):
///
/// 1. **Hint:** code specialization detected by name suffix — models
///    whose name contains "coder", "code-llama", or "codellama" claim
///    `code`; everything else claims `general`.
/// 2. **Affinity:** derived from the v0.2 proficiency level of the
///    capability most relevant to the hint (Code for `code`, max of
///    {General, Analysis, Instruction} for `general`), mapped from
///    `[0, 4]` onto `[0.0, 1.0]`.
///
/// `max_context` passes through verbatim; `max_output` gets a fixed
/// 2048 tokens for `Normal` latency (refined when skills declare
/// explicit output budgets in PR-D).
fn synthesize_default_claim(
    model_name: &str,
    profile: &CapabilityProfile,
    max_context: u32,
) -> CapabilityClaim {
    let lower = model_name.to_lowercase();
    let is_code_specialist = lower.contains("coder")
        || lower.contains("code-llama")
        || lower.contains("codellama")
        || lower.contains("deepseek-coder");

    let (hint, relevant_capability) = if is_code_specialist {
        (CapabilityHint::code(), Capability::Code)
    } else {
        // For the general hint, affinity tracks the best of the
        // general-adjacent capabilities. Models with `General: 4`
        // advertise strong general affinity; those with only
        // `Instruction: 2` advertise weaker.
        let best = [Capability::General, Capability::Analysis, Capability::Instruction]
            .into_iter()
            .map(|c| profile.get(&c).copied().unwrap_or(0))
            .max()
            .unwrap_or(0);
        // Return a sentinel; we'll compute affinity from `best`
        // below rather than re-looking-up.
        return CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            max_context,
            2_048,
            (best as f32 / 4.0).clamp(0.0, 1.0),
        );
    };
    let proficiency = profile.get(&relevant_capability).copied().unwrap_or(0);
    let affinity = (proficiency as f32 / 4.0).clamp(0.0, 1.0);
    CapabilityClaim::new(hint, LatencyClass::Normal, max_context, 2_048, affinity)
}

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

            let claim = synthesize_default_claim(
                &model.name,
                &model.oicp_capabilities,
                32_768,
            );
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
                // Commonwealth's ModelInfo doesn't carry a size_gb
                // today — it's sourced from runtime discovery, not
                // a manifest. Leave unpopulated; the OICP tiebreaker
                // treats unknown sizes as sorted-after any known
                // size, so this path is safe under mesh routing.
                size_gb: None,
                claims: vec![claim],
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
            embed_model: None,
        }),
        federation,
    })
}
