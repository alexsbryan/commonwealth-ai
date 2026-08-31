// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a plain OpenAI client gets from `POST /v1/chat/completions`.
//!
//! Measured defect (2026-08-29): the daemon synthesised a URL allowlist
//! from `role: tool` messages and installed it as a SAMPLER CONSTRAINT
//! on every locally-served chat. One `docs.rust-lang.org` link in a
//! cargo error made that the only URL the model could reach, so a
//! request to emit `https://api.stripe.com/v1/charges` came back
//! holding the rust-lang URL — HTTP 200, no warning, wrong bytes. The
//! control run, identical but for that one link, was correct.
//!
//! These pin the gate that stopped it. See
//! `turn_fidelity::auto_allowlist_enabled` and `sovereign/DEFAULTS_LEDGER.md`.

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use commonwealth_api::openai_types::{ChatCompletionRequest, ChatCompletionResponse, StreamFrame};
use commonwealth_api::routes_inference::chat_completions;
use commonwealth_api::state::{AppState, LocalInferenceError, LocalInferenceService};
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use futures::Stream;

/// Minimal single-member mesh, the shape `AppState::new` wants. Mirrors
/// `tests/app_state_privacy.rs` — an integration test cannot reach the
/// crate's `#[cfg(test)]` helpers.
pub(crate) fn solo_state(service: Arc<dyn LocalInferenceService>) -> AppState {
    let node = NodeId::from_u128(1);
    let mut members = std::collections::HashMap::new();
    members.insert(
        node,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: node,
            name: "A".into(),
            invited_by: node,
            joined_at: 0,
            last_seen: 100,
            status: NodeStatus::Online,
            capabilities: NodeCapabilities {
                hardware: HardwareProfile {
                    gpus: vec![],
                    system_ram_gb: 0,
                    cpu_cores: 0,
                    total_storage_gb: 0,
                    free_storage_gb: 0,
                    network_bandwidth_mbps: None,
                },
                available: AvailableResources::default(),
                active_processes: vec![],
                hosted_corpora: vec![],
                reported_at: 100,
                inference_availability: 1.0,
                inference_capable: false,
                loaded_models: vec![],
                embed_model: None,
                benchmark: None,
                current_in_flight: None,
                anchor: None,
            },
            addresses: vec!["192.168.1.1:9742".parse::<std::net::SocketAddr>().unwrap()],
        },
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(7),
        name: "Test".into(),
        invite_key_hash: [3u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    AppState::new(node, mesh).with_local_inference(service)
}

/// Captures the request as the service actually received it — after
/// every frontdoor pass has had its turn. Asserting on the request the
/// HANDLER forwards is the only way to see a mutation that is
/// otherwise invisible until sampling time.
#[derive(Clone, Default)]
pub(crate) struct CapturesRequest(pub(crate) Arc<Mutex<Option<ChatCompletionRequest>>>);

impl CapturesRequest {
    pub(crate) fn seen(&self) -> ChatCompletionRequest {
        self.0.lock().unwrap().clone().expect("service was called")
    }
}

#[async_trait::async_trait]
impl LocalInferenceService for CapturesRequest {
    async fn chat_completion(
        &self,
        r: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LocalInferenceError> {
        *self.0.lock().unwrap() = Some(r);
        Err(LocalInferenceError::Other("captured".into()))
    }

    async fn chat_completion_stream(
        &self,
        r: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, LocalInferenceError> {
        *self.0.lock().unwrap() = Some(r);
        Err(LocalInferenceError::Other("captured".into()))
    }

    fn provider_manifest(&self) -> Option<commonwealth_inference::oicp::ProviderManifest> {
        None
    }

    async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
        unimplemented!("embedding is not on this path")
    }
}

pub(crate) fn chat_with_url_in_tool_result() -> ChatCompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": "primary",
        "messages": [
            {"role": "user", "content": "build it"},
            {"role": "tool", "tool_call_id": "c1",
             "content": "error[E0433]: see https://doc.rust-lang.org/error_codes/E0433.html"},
            {"role": "user", "content": "now write the stripe endpoint"}
        ],
    }))
    .expect("test request builds")
}

/// THE regression. Default-off means a plain OpenAI client's request
/// reaches the model unconstrained, so a URL the user asks for is a URL
/// the model can still emit.
#[tokio::test]
async fn a_url_in_a_tool_result_does_not_constrain_sampling_by_default() {
    let svc = CapturesRequest::default();
    let state = solo_state(Arc::new(svc.clone()));
    let _ = chat_completions(
        State(state),
        HeaderMap::new(),
        None,
        Json(chat_with_url_in_tool_result()),
    )
    .await;
    assert_eq!(
        svc.seen().url_allowlist,
        None,
        "the daemon must not invent a URL allowlist for a caller that never asked for one — \
         an allowlist here masks every unlisted URL at sampling time, so the model answers \
         with a URL from the tool result instead of the one the user requested"
    );
}

/// The other half of the contract: an EXPLICIT allowlist is the
/// caller's own decision and the gate must not touch it. This is what
/// keeps deep-research and the search gym working with the flag off —
/// both set the field themselves.
#[tokio::test]
async fn a_caller_supplied_allowlist_survives_the_gate() {
    let svc = CapturesRequest::default();
    let state = solo_state(Arc::new(svc.clone()));
    let mut request = chat_with_url_in_tool_result();
    request.url_allowlist = Some(vec!["https://example.test/a".to_string()]);
    let _ = chat_completions(State(state), HeaderMap::new(), None, Json(request)).await;
    assert_eq!(
        svc.seen().url_allowlist.as_deref(),
        Some(&["https://example.test/a".to_string()][..]),
        "an explicit allowlist is the caller's decision; the gate governs only whether the \
         daemon invents one"
    );
}

// ── the 503 a busy anchor node returns ────────────────────────────
//
// A queue shed is backpressure, and this route is advertised as
// OpenAI-compatible — so the one thing a shed has to say ("busy, come
// back in 30s") has to arrive where an OpenAI client looks for it.
// Serialising `error` as a bare string put it out of reach of every
// SDK on the route.

use commonwealth_api::admission::{AdmissionReason, AdmissionRejection};

/// `as_str` and the serde `rename_all` are two spellings of one name
/// (§10.6), read by different consumers: `as_str` feeds the OpenAI
/// `error.code`, serde feeds the top-level `reason`. A drift between
/// them ships a body whose two fields disagree about why the request
/// was refused.
#[test]
fn admission_reason_code_matches_serde() {
    for reason in [
        AdmissionReason::Paused,
        AdmissionReason::YieldedToLocal,
        AdmissionReason::CeilingExceeded,
        AdmissionReason::LocalQueueFull,
        AdmissionReason::PrincipalShareExceeded,
    ] {
        let serialized = serde_json::to_value(reason).expect("reason serializes");
        assert_eq!(
            serialized.as_str(),
            Some(reason.as_str()),
            "{reason:?}: as_str and serde must spell the reason identically"
        );
    }
}

/// Both halves matter: the OpenAI envelope for SDKs, and the flat
/// `reason` / `retry_after_secs` for the peer load balancer and
/// `deep_research`'s substring shed classifier.
#[test]
fn a_rejection_carries_both_the_openai_envelope_and_the_flat_reason() {
    let body = serde_json::to_value(AdmissionRejection::new(
        "host busy: ~34746 ms predicted wait at queue position 6",
        AdmissionReason::LocalQueueFull,
        35,
    ))
    .expect("rejection serializes");

    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "local_queue_full");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("host busy"));

    assert_eq!(body["reason"], "local_queue_full");
    assert_eq!(body["retry_after_secs"], 35);
}
