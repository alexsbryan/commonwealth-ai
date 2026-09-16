// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/v1/models` promises DISPATCHABILITY. These pin that promise to the
//! one thing that can keep it: the list is a function of the manifests
//! name resolution reads, and of nothing else.
//!
//! The failure they encode was measured on a live 2-node mesh
//! (2026-08-27), not imagined. `/v1/models` returned 12 entries against
//! 6 in the capability manifest, listed `Qwen3.8-27B-UD-Q6_K_XL` twice,
//! and advertised `Qwen3-Embedding-0.6B-Q8_0`, which chat completions
//! refused with "no node in this mesh advertises model
//! 'Qwen3-Embedding-0.6B-Q8_0' — check `/v1/models` for available
//! names". The refusal named the list that was wrong.
//!
//! ## Which of these actually has teeth
//!
//! Measured, by forcing `manifest_rows` to return `None` and watching:
//! FOUR go red — `a_store_entry_no_manifest_carries_is_not_listed`,
//! `one_name_held_by_two_nodes_is_one_row_naming_both`,
//! `a_name_is_resident_when_any_holder_has_it_resident`,
//! `a_held_but_unloaded_model_lists_as_cold_not_missing`.
//!
//! `every_listed_id_is_advertised_by_some_manifest` does NOT, and it
//! reads like the headline gate, so say so plainly: it passes
//! vacuously whenever the list is empty, which is what the store path
//! produces in this fixture. It states the contract; it does not
//! defend it. **The load-bearing one is
//! `a_store_entry_no_manifest_carries_is_not_listed`** — it fails
//! exactly when a non-manifest source gets back into the listing,
//! which is the whole regression class. Reach for that one first if
//! you are changing this handler.

use super::*;
use crate::state::{test_app_state, LocalInferenceError, LocalInferenceService};
use futures::Stream;
use oicp_types::{ModelStatus, ProviderManifest, ProviderModel};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};
use std::pin::Pin;
use std::sync::Arc;

fn model(id: &str, loaded: bool) -> ProviderModel {
    ProviderModel {
        id: id.into(),
        base_model: None,
        quantization: None,
        context_tokens: 32_768,
        status: ModelStatus {
            available: true,
            loaded,
            estimated_tokens_per_sec: None,
            estimated_ttft_ms: None,
            estimated_load_time_sec: None,
        },
        size_gb: None,
        claims: vec![],
        fingerprint: None,
    }
}

/// A node whose manifest carries `local`, and whose one reachable peer
/// carries `shared` (which it also holds, cold) plus `peer-only`.
struct TwoNodeMesh;

#[async_trait::async_trait]
impl InferenceProvider for TwoNodeMesh {
    async fn complete(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        unimplemented!("listing does not generate")
    }
    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
    > {
        unimplemented!("listing does not generate")
    }
    async fn embed(&self, _i: &str) -> sovereign_core::error::Result<Vec<f32>> {
        unimplemented!("listing does not embed")
    }
    fn capabilities(&self) -> ProviderCapabilities {
        unimplemented!()
    }
    async fn peer_manifests(&self) -> Vec<(String, ProviderManifest)> {
        vec![(
            "RuggedFox".into(),
            ProviderManifest::new(vec![
                // Same name, other machine, and WARM there.
                model("shared-primary", true),
                model("peer-only", true),
            ]),
        )]
    }
}

#[async_trait::async_trait]
impl LocalInferenceService for TwoNodeMesh {
    /// Echoes back the model it was asked to serve.
    ///
    /// It used to `unimplemented!()` — listing does not generate. The
    /// guest tests need the ADMITTED arm of the scope gate to be
    /// observable, and "the request reached dispatch" is only observable
    /// if dispatch answers. Echoing the model id also makes a silent
    /// substitution visible: if the gate ever let a request through and
    /// something downstream swapped the name, this response says so.
    async fn chat_completion(
        &self,
        r: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LocalInferenceError> {
        Ok(ChatCompletionResponse {
            id: "test".into(),
            object: "chat.completion".into(),
            created: 0,
            model: r.model.unwrap_or_default(),
            choices: vec![],
            usage: None,
        })
    }
    async fn chat_completion_stream(
        &self,
        _r: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, LocalInferenceError> {
        unimplemented!("listing does not generate")
    }
    fn provider_manifest(&self) -> Option<ProviderManifest> {
        Some(ProviderManifest::new(vec![
            model("local-fast", true),
            // Held here, idle-unloaded. The lazy primary's steady state.
            model("shared-primary", false),
        ]))
    }
}

async fn rows(service: Arc<dyn LocalInferenceService>) -> Vec<ModelObject> {
    let state = test_app_state().with_local_inference(service);
    let resp = list_models(State(state), None).await.into_response();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body reads");
    serde_json::from_slice::<ModelListResponse>(&body)
        .expect("body is a model list")
        .data
}

/// THE gate. Every id returned must come from a manifest, because a
/// manifest is what `locate_named_model` resolves against — so an id
/// here is an id that dispatches. An entry sourced from anywhere else
/// (the gossiped KV store, a synthesised alias) is the regression.
#[tokio::test]
async fn every_listed_id_is_advertised_by_some_manifest() {
    let advertised: HashSet<String> = ["local-fast", "shared-primary", "peer-only"]
        .into_iter()
        .map(String::from)
        .collect();
    for row in rows(Arc::new(TwoNodeMesh)).await {
        assert!(
            advertised.contains(&row.id),
            "'{}' is listed but no manifest advertises it — a chat \
             completion naming it would be refused with 'no node in this \
             mesh advertises model', pointing the operator back at this list",
            row.id
        );
    }
}

/// The KV store is no longer an input. Registering a model there — the
/// only thing the pre-fix handler read — must not put it on the list.
#[tokio::test]
async fn a_store_entry_no_manifest_carries_is_not_listed() {
    let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
    state.register_model(commonwealth_core::model::ModelInfo {
        id: commonwealth_core::ModelId::from_u128(7),
        name: "ghost-from-gossip".into(),
        repo: String::new(),
        file: "ghost.gguf".into(),
        size_bytes: 1,
        total_layers: 0,
        architecture: commonwealth_core::model::ModelArchitecture::Other,
        available_on: std::collections::HashMap::new(),
        oicp_capabilities: Default::default(),
        quantization: String::new(),
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    });

    let resp = list_models(State(state), None).await.into_response();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: ModelListResponse = serde_json::from_slice(&body).unwrap();
    assert!(
        !list.data.iter().any(|m| m.id == "ghost-from-gossip"),
        "a gossiped store entry is not evidence that anything can serve it"
    );
}

/// One name, one row, both holders. The store keyed on
/// `hash(role, absolute path)`, so the same weights on two machines were
/// two entries with one name — which is what put the 27B on the live
/// list twice.
#[tokio::test]
async fn one_name_held_by_two_nodes_is_one_row_naming_both() {
    let rows = rows(Arc::new(TwoNodeMesh)).await;
    let shared: Vec<&ModelObject> = rows.iter().filter(|m| m.id == "shared-primary").collect();
    assert_eq!(shared.len(), 1, "one dispatchable name is one row");
    assert_eq!(
        shared[0].advertised_by,
        vec!["local".to_string(), "RuggedFox".to_string()],
        "both holders are named — `owned_by: \"mesh\"` could not say this"
    );
}

/// Cold here, warm on the peer: the name is warm, because the resolver
/// load-balances across holders and will pick the peer. The reverse
/// reading would let one idle-unloaded node mask a warm mesh.
#[tokio::test]
async fn a_name_is_resident_when_any_holder_has_it_resident() {
    let rows = rows(Arc::new(TwoNodeMesh)).await;
    let by_id =
        |id: &str| -> ModelObject { rows.iter().find(|m| m.id == id).cloned().expect("listed") };
    assert_eq!(by_id("shared-primary").residency, Some(Residency::Resident));
    assert_eq!(by_id("local-fast").residency, Some(Residency::Resident));
    assert_eq!(
        by_id("shared-primary")
            .performance
            .expect("performance block is always present on this path")
            .loaded,
        true,
        "the legacy flag agrees with the new field rather than contradicting it"
    );
}

/// A node with weights nobody has loaded is still dispatchable — the
/// first request pays a cold load. That is normal operation for a lazy
/// primary and must not read as unavailable.
#[tokio::test]
async fn a_held_but_unloaded_model_lists_as_cold_not_missing() {
    struct ColdOnly;
    #[async_trait::async_trait]
    impl InferenceProvider for ColdOnly {
        async fn complete(
            &self,
            _r: &CompletionRequest,
        ) -> sovereign_core::error::Result<CompletionResponse> {
            unimplemented!()
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> sovereign_core::error::Result<
            Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
        > {
            unimplemented!()
        }
        async fn embed(&self, _i: &str) -> sovereign_core::error::Result<Vec<f32>> {
            unimplemented!()
        }
        fn capabilities(&self) -> ProviderCapabilities {
            unimplemented!()
        }
    }
    #[async_trait::async_trait]
    impl LocalInferenceService for ColdOnly {
        async fn chat_completion(
            &self,
            _r: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, LocalInferenceError> {
            unimplemented!()
        }
        async fn chat_completion_stream(
            &self,
            _r: ChatCompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, LocalInferenceError> {
            unimplemented!()
        }
        fn provider_manifest(&self) -> Option<ProviderManifest> {
            Some(ProviderManifest::new(vec![model("big-primary", false)]))
        }
    }
    let rows = rows(Arc::new(ColdOnly)).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "big-primary");
    assert_eq!(rows[0].residency, Some(Residency::Cold));
}

/// A provider with no manifest (the orchestrator daemon) falls back to
/// the store rather than returning nothing. The fallback is narrower
/// than the manifest path and says so in its docs; what it must not do
/// is disappear.
#[tokio::test]
async fn no_local_inference_falls_back_to_the_store() {
    let state = test_app_state();
    state.register_model(commonwealth_core::model::ModelInfo {
        id: commonwealth_core::ModelId::from_u128(9),
        name: "orchestrated".into(),
        repo: String::new(),
        file: "o.gguf".into(),
        size_bytes: 1,
        total_layers: 0,
        architecture: commonwealth_core::model::ModelArchitecture::Other,
        available_on: std::collections::HashMap::new(),
        oicp_capabilities: Default::default(),
        quantization: String::new(),
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    });
    let resp = list_models(State(state), None).await.into_response();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: ModelListResponse = serde_json::from_slice(&body).unwrap();
    assert!(list.data.iter().any(|m| m.id == "orchestrated"));
}

// ── guest scope refinement ───────────────────────────────────────────
//
// `client_auth` decided this caller may reach the ROUTE. These pin the
// half it cannot decide, because it lives in the body: WHICH MODEL.
//
// Both were watched fail — see `guest_falsification` in
// `tests/client_auth.rs` for the probe and what went red.

use sovereign_grants::{GuestGrant, Scope};

/// A live grant over `models`, as `client_auth` would have inserted it.
fn guest_for(models: &[&str]) -> Option<axum::Extension<crate::client_auth::Guest>> {
    Some(axum::Extension(crate::client_auth::Guest(Arc::new(
        GuestGrant {
            token: "t".into(),
            scopes: vec![Scope::Models(
                models.iter().map(|m| m.to_string()).collect(),
            )],
            label: None,
            issued_at_ms: 0,
            expires_at_ms: u64::MAX,
            revoked: false,
        },
    ))))
}

async fn chat_as_guest(model: Option<&str>, granted: &[&str]) -> (StatusCode, serde_json::Value) {
    let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
    let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "hello"}],
    }))
    .expect("request shape");
    let resp = chat_completions(
        State(state),
        HeaderMap::new(),
        guest_for(granted),
        Json(request),
    )
    .await;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// **THE §18.3 gate**, and the reason this refinement is in the handler
/// rather than the auth layer. Without the refusal, an out-of-scope
/// `model` walks down to Priority 4 and is served by `default_model_id()`:
/// asked for one model, got another, HTTP 200, no way to tell. That is
/// `d45489a3` verbatim — so the assertion is on the BODY, not the status.
#[tokio::test]
async fn a_guest_naming_an_ungranted_model_is_refused_not_served_the_default() {
    let (status, body) = chat_as_guest(Some("peer-only"), &["shared-primary"]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["type"], "guest_scope");
    assert_eq!(body["error"]["code"], "model_not_granted");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("peer-only") && message.contains("shared-primary"),
        "the refusal names what was asked AND what is granted: {message}"
    );
}

/// The subtler half. An absent `model` is exactly what reaches the default
/// today — so "no model named" must refuse too, or the gate is bypassed by
/// omitting a field rather than by naming the wrong one.
#[tokio::test]
async fn a_guest_naming_no_model_at_all_is_refused_rather_than_defaulted() {
    let (status, body) = chat_as_guest(None, &["shared-primary"]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "model_not_granted");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("no model named"));
}

/// Whitespace is not a widening. `" shared-primary "` trims to the granted
/// name; `"shared-primary-v2"` does not become it.
#[tokio::test]
async fn the_model_match_is_exact_after_trimming() {
    let (padded, _) = chat_as_guest(Some("  shared-primary  "), &["shared-primary"]).await;
    assert_ne!(
        padded,
        StatusCode::FORBIDDEN,
        "a trimmed exact name is inside the grant"
    );
    let (prefixed, _) = chat_as_guest(Some("shared-primary-v2"), &["shared-primary"]).await;
    assert_eq!(
        prefixed,
        StatusCode::FORBIDDEN,
        "a longer name that merely starts with a granted one is NOT granted"
    );
}

/// The ADMITTED arm, and the reason the refusal tests are not vacuous: a
/// granted model reaches dispatch, and comes back as ITSELF. Without this
/// the whole gate could be "refuse every guest" and every other test here
/// would still pass.
#[tokio::test]
async fn a_guest_naming_a_granted_model_is_served_that_model() {
    let (status, body) = chat_as_guest(Some("shared-primary"), &["shared-primary"]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["model"], "shared-primary",
        "served the model that was asked for, not a default"
    );
}

/// The listing keeps the same contract for one caller that it keeps for
/// the node: every id returned is dispatchable BY THEM. A guest shown a
/// name they would be refused reintroduces the `/v1/models` defect in a
/// new place — the list would advertise and the request would refuse, and
/// the refusal would point back at the list.
#[tokio::test]
async fn a_guest_sees_only_the_models_its_grant_names() {
    let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
    let ungated = list_models(State(state.clone()), None)
        .await
        .into_response();
    let ungated: ModelListResponse = serde_json::from_slice(
        &axum::body::to_bytes(ungated.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    // The fixture advertises more than one name, or the filter below would
    // pass vacuously.
    assert!(
        ungated.data.len() > 1,
        "fixture must list several models for the filter to mean anything"
    );

    let gated = list_models(State(state), guest_for(&["shared-primary"]))
        .await
        .into_response();
    let gated: ModelListResponse = serde_json::from_slice(
        &axum::body::to_bytes(gated.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let ids: Vec<&str> = gated.data.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["shared-primary"]);
}

/// A grant naming a model this node cannot serve lists NOTHING — it does
/// not conjure a row from the grant. The mint route is what stops such a
/// grant existing; this pins that the listing never papers over one.
#[tokio::test]
async fn a_grant_naming_an_unserved_model_lists_nothing() {
    let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
    let resp = list_models(State(state), guest_for(&["not-on-this-node"]))
        .await
        .into_response();
    let list: ModelListResponse = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(list.data.is_empty());
}

/// `dispatchable_ids` is what the mint route gates `--model` against. It
/// must be the SAME set `/v1/models` reports to an ungated caller — a
/// second answer here is how a grant gets minted for a name the request
/// path will refuse (§10.6).
#[tokio::test]
async fn dispatchable_ids_matches_what_an_ungated_listing_reports() {
    let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
    let resp = list_models(State(state.clone()), None)
        .await
        .into_response();
    let listed: ModelListResponse = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let mut from_listing: Vec<String> = listed.data.iter().map(|m| m.id.clone()).collect();
    let mut from_mint_gate = dispatchable_ids(&state).await;
    from_listing.sort();
    from_mint_gate.sort();
    assert_eq!(from_mint_gate, from_listing);
    assert!(
        !from_mint_gate.is_empty(),
        "fixture must advertise something"
    );
}
