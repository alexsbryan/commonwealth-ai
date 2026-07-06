// SPDX-License-Identifier: AGPL-3.0-or-later
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;

use commonwealth_core::ids::NodeId;
use commonwealth_inference::oicp::features;
use commonwealth_inference::oicp::{
    Capability, CapabilityClaim, CapabilityHint, CapabilityProfile, CorpusDescriptor,
    FederationManifest, KnowledgeManifest, LatencyClass, ModelStatus, PeerDescriptor, ProviderInfo,
    ProviderManifest, ProviderModel, ProviderType, OICP_VERSION,
};

use crate::state::AppState;

/// Models at or below this size also advertise a Fast-latency claim.
/// ~12 GB covers a 9B Q8 / 14B Q4 — the sizes that actually deliver
/// sub-second TTFT on the hardware this project targets; the 30B+
/// primaries stay Normal-only. Mirrors the FastShort claim shape in
/// `sovereign-mesh::oicp_synthesis` (2 048 ctx / 512 out, affinity
/// +0.05) so hub-served and Sovereign-served manifests agree on what
/// a fast claim looks like.
const FAST_CLAIM_MAX_SIZE_BYTES: u64 = 12_000_000_000;

/// Synthesize the v0.3 claims for a model from its name and v0.2
/// capability profile:
///
/// 1. **Hint:** code specialization detected by name substring
///    (coder / code-llama / codellama / deepseek-coder) claims
///    `code`; everything else claims `general`.
/// 2. **Affinity:** the v0.2 proficiency most relevant to the hint
///    (Code for `code`, max of {General, Analysis, Instruction} for
///    `general`), mapped from `[0, 4]` onto `[0.0, 1.0]`. Static at
///    synthesis time — see the reality note on `CapabilityClaim`.
/// 3. **Claims:** one Normal claim (`max_context`/2 048 out) per
///    model, plus a Fast claim for small models (PR-E gap closed
///    2026-06-10: previously one hardcoded Normal claim, so a small
///    model could never match a latency_class=Fast request without
///    the 0.8 adjacency penalty).
pub(crate) fn synthesize_default_claims(
    model_name: &str,
    profile: &CapabilityProfile,
    max_context: u32,
    size_bytes: u64,
) -> Vec<CapabilityClaim> {
    let lower = model_name.to_lowercase();
    let is_code_specialist = lower.contains("coder")
        || lower.contains("code-llama")
        || lower.contains("codellama")
        || lower.contains("deepseek-coder");

    let (hint, affinity) = if is_code_specialist {
        let proficiency = profile.get(&Capability::Code).copied().unwrap_or(0);
        (
            CapabilityHint::code(),
            (proficiency as f32 / 4.0).clamp(0.0, 1.0),
        )
    } else {
        // For the general hint, affinity tracks the best of the
        // general-adjacent capabilities. Models with `General: 4`
        // advertise strong general affinity; those with only
        // `Instruction: 2` advertise weaker.
        let best = [
            Capability::General,
            Capability::Analysis,
            Capability::Instruction,
        ]
        .into_iter()
        .map(|c| profile.get(&c).copied().unwrap_or(0))
        .max()
        .unwrap_or(0);
        (
            CapabilityHint::general(),
            (best as f32 / 4.0).clamp(0.0, 1.0),
        )
    };

    let normal = CapabilityClaim::new(
        hint.clone(),
        LatencyClass::Normal,
        max_context,
        2_048,
        affinity,
    );
    if size_bytes > 0 && size_bytes <= FAST_CLAIM_MAX_SIZE_BYTES {
        let fast = CapabilityClaim::new(
            hint,
            LatencyClass::Fast,
            2_048,
            512,
            (affinity + 0.05).clamp(0.0, 1.0),
        );
        vec![fast, normal]
    } else {
        vec![normal]
    }
}

/// OICP v0.4 §2.1 features the *embedded* llama.cpp inference path
/// honours. The embedded sampler enforces JSON-Schema and Lark grammars
/// and the sampler allow-lists, consumes the `oicp` request envelope,
/// honours `think_budget`, and stamps model fingerprints — the full set.
const EMBEDDED_FEATURES: &[&str] = &[
    features::CONSTRAINT_JSON_SCHEMA,
    features::CONSTRAINT_JSON_OBJECT,
    features::CONSTRAINT_LARK,
    features::CONSTRAINT_ALLOWLIST_URL,
    features::CONSTRAINT_ALLOWLIST_EVIDENCE_ID,
    features::CONSTRAINT_ALLOWLIST_CMD_PREFIX,
    features::THINK_BUDGET,
    features::OICP_REQUEST_PROPERTIES,
];

/// Conservative feature set for the standalone-Commonwealth orchestrator
/// path, which may front heterogeneous backends. We claim only what any
/// OpenAI-compatible llama-server the orchestrator spawns reliably
/// honours: JSON-Schema / JSON-object structured output and the routing
/// envelope. Lark grammar and the sampler allow-lists are embedded-only
/// and NOT advertised here.
const HUB_FEATURES: &[&str] = &[
    features::CONSTRAINT_JSON_SCHEMA,
    features::CONSTRAINT_JSON_OBJECT,
    features::OICP_REQUEST_PROPERTIES,
];

// NOTE: `features::MODEL_FINGERPRINT` is intentionally NOT advertised
// yet. We populate `ProviderModel.fingerprint` below (harmless extra
// data a client may read), but the feature's contract (§6) also requires
// the concrete model's fingerprint to be echoed in every response's OICP
// meta — that response-echo, and the PhaseCache keying that consumes it,
// land together in the behavior-change phase. Advertise the feature there.

/// Opaque model fingerprint (OICP v0.4 §6): changes when the served
/// weights / quantization / template change, so a client can key
/// model-dependent caches on `(id, fingerprint)`. Synthesized from the
/// stable identity a manifest already carries — id, quantization, size.
/// Alias rows (`primary` / `fast`) carry no quant/size and fall back to
/// the bare id; the concrete model rows they front carry the real ones,
/// and a response's `model` field resolves to a concrete id (§6).
pub(crate) fn synthesize_fingerprint(
    id: &str,
    quantization: Option<&str>,
    size_gb: Option<f32>,
) -> String {
    match (quantization.filter(|q| !q.is_empty()), size_gb) {
        (Some(q), Some(gb)) => format!("{id}@{q}#{gb:.3}"),
        (Some(q), None) => format!("{id}@{q}"),
        (None, Some(gb)) => format!("{id}#{gb:.3}"),
        (None, None) => id.to_string(),
    }
}

/// Apply OICP v0.4 additive enrichment to an outbound manifest (§2/§4/§6),
/// on BOTH the local-inference and hub-synthesized paths so a client sees
/// identical v0.4 posture regardless of daemon shape: advertise the
/// honoured `features`, stamp per-model fingerprints, and publish the
/// local embed model (with its query-instruction prefix) into the
/// knowledge section. The ingest surface (§5) is advertised separately by
/// the ingest routes once they are mounted.
fn apply_v04_enrichment(state: &AppState, embedded: bool, manifest: &mut ProviderManifest) {
    // §2 features.
    let feats = if embedded { EMBEDDED_FEATURES } else { HUB_FEATURES };
    manifest.features = feats.iter().map(|s| s.to_string()).collect();

    // §6 per-model fingerprints (leave any already set by the source).
    for model in &mut manifest.models {
        if model.fingerprint.is_none() {
            model.fingerprint = Some(synthesize_fingerprint(
                &model.id,
                model.quantization.as_deref(),
                model.size_gb,
            ));
        }
    }

    // §4 embed-model completeness. The embed model (with its v0.4
    // query-instruction prefix) already lives in the inference store,
    // set by the daemon at bootstrap. Publish it in the knowledge
    // section so a client can reconstruct bit-compatible query
    // embeddings for federated search.
    if let Some(embed_model) = state.inner.inference_store.get_local_embed_model() {
        match &mut manifest.knowledge {
            Some(k) => k.embed_model = Some(embed_model),
            None => {
                manifest.knowledge = Some(KnowledgeManifest {
                    corpora: Vec::new(),
                    search_endpoint: "/v1/knowledge/search".into(),
                    embed_model: Some(embed_model),
                    ingest: None,
                });
            }
        }
    }
}

/// GET /oicp/v1/capabilities — OICP provider manifest per spec §4.
///
/// Reads the optional `X-Node-Id` request header and, when present,
/// applies any per-peer affinity multiplier from
/// `state.inner.peer_preferences` before serializing. This is the
/// single integration point for the Ostrom-style sanction (Mesh
/// Health design §5): private adjustments live in the local
/// preference store, ride through this multiplication step on every
/// outbound manifest, and are never communicated to the requester
/// as a distinct signal — the requester simply sees a lower number
/// and routes elsewhere on its own.
pub async fn capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Json<ProviderManifest> {
    let requester = crate::headers::parse_x_node_id(&headers);

    // If we have a local inference service (Sovereign's
    // EmbeddedLlamaCpp), prefer its manifest — that's the one
    // that actually reflects what we can serve. The scheduler-
    // based manifest below is for the standalone Commonwealth
    // daemon where llama-servers are spawned by the orchestrator;
    // in the Sovereign+mesh embed, those are empty.
    if let Some(local) = state.inner.local_inference.as_ref() {
        if let Some(mut manifest) = local.provider_manifest() {
            // Enrich provider name with the mesh name so peer
            // MeshAwareSelector can tell "this is mac-peer's
            // Sovereign" vs a generic provider.
            let mesh = state.inner.mesh.read().await;
            if manifest.provider.is_none() {
                manifest.provider = Some(ProviderInfo {
                    name: Some(mesh.name.clone()),
                    provider_type: Some(ProviderType::Mesh),
                });
            }
            drop(mesh);
            apply_v04_enrichment(&state, true, &mut manifest);
            apply_peer_preference(&state, &requester, &mut manifest);
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

            let claims = synthesize_default_claims(
                &model.name,
                &model.oicp_capabilities,
                32_768,
                model.size_bytes,
            );
            ProviderModel {
                id: model.name.clone(),
                base_model: None,
                quantization: if model.quantization.is_empty() {
                    None
                } else {
                    Some(model.quantization.clone())
                },
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded,
                    estimated_tokens_per_sec: shard_plan.map(|p| p.estimated_tokens_per_sec),
                    estimated_ttft_ms: shard_plan.map(|p| p.estimated_ttft_ms),
                    estimated_load_time_sec: None,
                },
                // From `ModelInfo.size_bytes` (runtime discovery).
                // Feeds the SSOT tie-break (smaller wins score ties)
                // and the throughput size-ratio extrapolation. (The
                // pre-2026-06-10 `None` here was an outdated claim
                // that ModelInfo had no size — it always did.)
                size_gb: (model.size_bytes > 0).then(|| model.size_bytes as f32 / 1_000_000_000.0),
                claims,
                fingerprint: None,
            }
        })
        .collect();

    // NOT routed through the PeerTransport seam, deliberately: this
    // formats an *advertised* URL for a federated peer MESH
    // (`MeshPeering.contact_nodes` — no `MemberRecord`/`NodeId`
    // exists), embedded in the manifest for clients to read. It is
    // content, not a dial this daemon performs. NOTE (no-VPN mesh):
    // this stays IP-shaped on purpose — cross-mesh federation is a
    // separate, IP-reachable trust domain. This node's OWN
    // capabilities dial (peer inference scoring) rides the seam via
    // `peer_inference_endpoints`/`TrafficClass::Inference`, so a
    // no-IP peer is scored correctly; only the advertised
    // cross-mesh federation URL here is IP-shaped, and that is not a
    // W-track dial. Do not "seam-ify" this without a federation
    // trust-model change.
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

    let mut manifest = ProviderManifest {
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
            ingest: None,
        }),
        federation,
        features: Vec::new(),
    };
    drop(mesh);
    apply_v04_enrichment(&state, false, &mut manifest);
    apply_peer_preference(&state, &requester, &mut manifest);
    Json(manifest)
}

/// Apply any local-only peer preference for `requester` to the
/// outbound manifest, multiplying every claim's `affinity` by the
/// stored multiplier. No-op when the requester is unidentified or
/// the operator hasn't set a preference for them.
fn apply_peer_preference(
    state: &AppState,
    requester: &Option<NodeId>,
    manifest: &mut ProviderManifest,
) {
    let Some(requester_id) = requester else {
        return;
    };
    let pref = match state.inner.peer_preferences.get(requester_id) {
        Ok(Some(p)) => p,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "peer_pref: lookup failed during manifest fetch"
            );
            return;
        }
    };
    let multiplier = pref.multiplier() as f32;
    let mut claim_count = 0usize;
    for model in manifest.models.iter_mut() {
        for claim in model.claims.iter_mut() {
            claim.affinity = (claim.affinity * multiplier).clamp(0.0, 1.0);
            claim_count += 1;
        }
    }
    tracing::debug!(
        requester_node_id = %fmt_requester(requester_id),
        multiplier,
        claim_count,
        "peer_pref: applied"
    );
}

fn fmt_requester(id: &NodeId) -> String {
    id.as_bytes()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_inference::oicp::{
        CapabilityClaim, CapabilityHint, LatencyClass, ModelStatus, ProviderManifest, ProviderModel,
    };
    use commonwealth_state::{PeerPreference, PeerPreferenceStore};

    fn nid(byte: u8) -> NodeId {
        NodeId::from_u128(byte as u128)
    }

    #[test]
    fn fingerprint_distinguishes_quant_and_size() {
        // §6: the fingerprint must change when quant/size change, and
        // fall back to the bare id when neither is known (alias rows).
        let a = synthesize_fingerprint("qwen3-9b", Some("Q4_K_M"), Some(5.2));
        let b = synthesize_fingerprint("qwen3-9b", Some("Q6_K"), Some(7.1));
        let c = synthesize_fingerprint("qwen3-9b", None, None);
        assert_ne!(a, b, "different quant/size ⇒ different fingerprint");
        assert_eq!(c, "qwen3-9b", "alias row falls back to id");
        // Empty-string quant (the ModelInfo default) is treated as absent.
        assert_eq!(synthesize_fingerprint("m", Some(""), None), "m");
    }

    #[test]
    fn feature_sets_are_registered_and_hub_is_a_subset() {
        for f in EMBEDDED_FEATURES.iter().chain(HUB_FEATURES) {
            assert!(features::is_valid(f), "advertised feature {f} is registered");
        }
        // The hub is more conservative than embedded: every hub feature
        // is also honoured by the embedded path.
        for f in HUB_FEATURES {
            assert!(EMBEDDED_FEATURES.contains(f), "hub feature {f} ⊆ embedded");
        }
        // Lark + allow-lists are embedded-only.
        assert!(!HUB_FEATURES.contains(&features::CONSTRAINT_LARK));
    }

    fn manifest_with_affinity(affinity: f32) -> ProviderManifest {
        ProviderManifest {
            oicp_version: OICP_VERSION.to_string(),
            provider: None,
            models: vec![ProviderModel {
                id: "test-model".into(),
                base_model: None,
                quantization: None,
                context_tokens: 32_000,
                status: ModelStatus {
                    available: true,
                    loaded: true,
                    estimated_tokens_per_sec: None,
                    estimated_ttft_ms: None,
                    estimated_load_time_sec: None,
                },
                size_gb: None,
                claims: vec![CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Normal,
                    32_000,
                    4_000,
                    affinity,
                )],
                fingerprint: None,
            }],
            knowledge: None,
            federation: None,
            features: Vec::new(),
        }
    }

    fn id_to_hex(id: &NodeId) -> String {
        id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    // Parser tests live alongside the parser itself in
    // `crate::headers::tests` — DRY-out, no duplication.

    #[tokio::test]
    async fn apply_peer_preference_scales_all_claim_affinities() {
        let state = crate::state::test_app_state();
        // Set 0.5 preference for peer X.
        let target = nid(0x11);
        state
            .inner
            .peer_preferences
            .set(&target, PeerPreference::new(0.5, None).unwrap())
            .unwrap();
        let mut manifest = manifest_with_affinity(0.8);
        // Apply for the matching requester.
        apply_peer_preference(&state, &Some(target), &mut manifest);
        let scaled = manifest.models[0].claims[0].affinity;
        assert!((scaled - 0.4).abs() < 1e-6, "got {scaled}");
    }

    #[tokio::test]
    async fn apply_peer_preference_is_noop_for_unmatched_requester() {
        let state = crate::state::test_app_state();
        state
            .inner
            .peer_preferences
            .set(&nid(0x11), PeerPreference::new(0.5, None).unwrap())
            .unwrap();
        let mut manifest = manifest_with_affinity(0.8);
        // Different requester — preference shouldn't apply.
        apply_peer_preference(&state, &Some(nid(0x22)), &mut manifest);
        assert!((manifest.models[0].claims[0].affinity - 0.8).abs() < 1e-6);
    }

    #[tokio::test]
    async fn apply_peer_preference_is_noop_when_requester_unidentified() {
        let state = crate::state::test_app_state();
        state
            .inner
            .peer_preferences
            .set(&nid(0x11), PeerPreference::new(0.5, None).unwrap())
            .unwrap();
        let mut manifest = manifest_with_affinity(0.8);
        // No `X-Node-Id` from the requester — manifest unchanged.
        apply_peer_preference(&state, &None, &mut manifest);
        assert!((manifest.models[0].claims[0].affinity - 0.8).abs() < 1e-6);
    }

    #[tokio::test]
    async fn manifest_endpoint_applies_preference_when_x_node_id_present() {
        // Full GET roundtrip through the router. test_app_state has
        // no models registered; we set a preference via the
        // PeerPreferenceStore on AppState and verify the helper
        // path runs cleanly with `X-Node-Id` set. Empty-model
        // manifests pass the multiplier loop without panicking,
        // proving the integration is wired even when there are no
        // claims to scale.
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let state = crate::state::test_app_state();
        state
            .inner
            .peer_preferences
            .set(&nid(0x33), PeerPreference::new(0.25, None).unwrap())
            .unwrap();
        let _store = PeerPreferenceStore::new;
        let app = crate::server::mock_router(state);
        let resp = app
            .oneshot(
                Request::get("/oicp/v1/capabilities")
                    .header("x-node-id", id_to_hex(&nid(0x33)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    // ── synthesize_default_claims (PR-E gap closed 2026-06-10) ───

    fn profile_with(cap: Capability, level: u8) -> CapabilityProfile {
        let mut p = CapabilityProfile::new();
        p.insert(cap, level);
        p
    }

    #[test]
    fn small_model_advertises_fast_and_normal_claims() {
        let claims = synthesize_default_claims(
            "qwen3-9b",
            &profile_with(Capability::General, 3),
            32_768,
            9_000_000_000, // 9 GB — under the fast cutoff
        );
        assert_eq!(claims.len(), 2);
        let fast = &claims[0];
        assert_eq!(fast.latency_class, LatencyClass::Fast);
        assert_eq!(fast.max_context, 2_048);
        assert_eq!(fast.max_output, 512);
        // Fast claim carries the +0.05 affinity nudge (0.75 + 0.05).
        assert!((fast.affinity - 0.80).abs() < 1e-6);
        let normal = &claims[1];
        assert_eq!(normal.latency_class, LatencyClass::Normal);
        assert_eq!(normal.max_context, 32_768);
        assert!((normal.affinity - 0.75).abs() < 1e-6);
    }

    #[test]
    fn large_model_advertises_normal_only() {
        let claims = synthesize_default_claims(
            "qwopus-35b",
            &profile_with(Capability::General, 4),
            32_768,
            34_000_000_000, // 34 GB — over the cutoff
        );
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].latency_class, LatencyClass::Normal);
    }

    #[test]
    fn unknown_size_is_conservative_normal_only() {
        // size_bytes == 0 means discovery hasn't measured the file —
        // don't advertise sub-second latency on a guess.
        let claims = synthesize_default_claims(
            "mystery-model",
            &profile_with(Capability::General, 2),
            32_768,
            0,
        );
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].latency_class, LatencyClass::Normal);
    }

    #[test]
    fn code_specialist_keeps_code_hint_on_every_claim() {
        let claims = synthesize_default_claims(
            "deepseek-coder-7b",
            &profile_with(Capability::Code, 4),
            32_768,
            7_000_000_000,
        );
        assert_eq!(claims.len(), 2);
        for claim in &claims {
            assert_eq!(claim.hint, CapabilityHint::code());
        }
        assert!((claims[1].affinity - 1.0).abs() < 1e-6);
    }
}
