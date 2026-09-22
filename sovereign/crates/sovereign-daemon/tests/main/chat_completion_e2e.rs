// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end mesh inference routing test.
//!
//! Exercises the full Joiner-side path without needing a real
//! `EmbeddedDaemon`:
//!   1. `InferenceRouter::complete_stream_with_id` selects a
//!      peer based on OICP scoring + the 60s manifest cache.
//!   2. It calls `GET /oicp/v1/capabilities` on the peer to fetch
//!      the manifest.
//!   3. It compares the peer's best candidate against the local
//!      manifest and decides to route.
//!   4. It POSTs `/v1/chat/completions?stream=true` to the peer
//!      and returns the stream + an attribution string.
//!
//! A `MockPeerServer` plays the Founder side — a minimal axum
//! router that serves a curated OICP manifest at the capabilities
//! endpoint and a canned SSE stream at the chat endpoint. No real
//! llama.cpp or EmbeddedDaemon is involved.
//!
//! Guards the two bugs we fixed in this body of work:
//!   * Multi-slot manifest advertisement (peer must see both Fast
//!     and Slow slots; the 9B picks up the request even though a
//!     27B is also advertised).
//!   * Streaming provenance attribution (the returned `model_id`
//!     string must carry `@ peer <name>`).
//!
//! Split into topic parts under `chat_completion_e2e/` for the
//! §3.2 size ceiling (behaviour-preserving move): `routing` (OICP
//! routing, failover, explicit `model` dispatch), `model_resolution`
//! (the model-resolving peer), `identity_and_health` (M5 stamp +
//! shed exemption).
#[path = "chat_completion_e2e/identity_and_health.rs"]
mod identity_and_health;
#[path = "chat_completion_e2e/model_resolution.rs"]
mod model_resolution;
#[path = "chat_completion_e2e/routing.rs"]
mod routing;

use std::sync::Arc;

use async_trait::async_trait;
use axum::response::IntoResponse;
use axum::Json;
use commonwealth_core::ids::NodeId;
use futures::StreamExt;
use oicp_types::{
    CapabilityClaim, CapabilityHint, LatencyClass, ModelStatus, ProviderManifest, ProviderModel,
    OICP_VERSION,
};
use serde::Deserialize;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_daemon::daemon::InferenceVenue;
use sovereign_mesh::peer_inference::{InferenceRouter, VenueHost, VenueSource};

use crate::common;
use crate::common::TestProvider;

// Local provider — a weak BYOM-like model whose `model_id_for`
// resolves to `profiles.byom_qwen25.thoughtful` in `models.toml`
// (caps={General:2, Analysis:1, Instruction:2}). It cannot score
// 1.0 against a DeepQuery's {Analysis:3, General:3}, so the
// peer's 9B wins and routing flows to the peer. The local stub
// is left in the unconfigured state — any local-path attempt
// surfaces `TestProvider::*_not_configured` as the bubbled error,
// which is what the LocalOnly test asserts on.
pub(crate) fn local_byom() -> Arc<dyn InferenceProvider> {
    Arc::new(TestProvider::new().with_model_id("qwen2.5-3b-instruct-q4_k_m"))
}

// ── Peer endpoint source stub ───────────────────────────────
//
// Provides a fixed peer list that `InferenceRouter` uses in
// place of `EmbeddedDaemon::peer_inference_endpoints()`.

pub(crate) struct StubVenueSource {
    pub(crate) peers: Vec<InferenceVenue>,
}

/// The id this stub claims as its own. A real `EmbeddedDaemon`
/// answers `local_node_id` from its joined mesh identity; the stub
/// answers with this so the routing path under test stamps
/// `X-Node-Id` exactly as production does.
pub(crate) const STUB_NODE_ID: u128 = 0x00C0_FFEE;

#[async_trait]
impl VenueSource for StubVenueSource {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        self.peers.clone()
    }
}

#[async_trait]
impl VenueHost for StubVenueSource {
    /// Overridden deliberately. The trait's default is `None`, and a
    /// `None` here would make every routing test in this file forward
    /// UNSTAMPED — i.e. would keep asserting the pre-M5 behaviour
    /// while production stamps. The absence case is covered where its
    /// decider lives, at the wire, in `oicp-client`'s own tests.
    async fn local_node_id(&self) -> Option<NodeId> {
        Some(NodeId::from_u128(STUB_NODE_ID))
    }
}

/// One `InferenceRouter` over a fixed peer list, wired to a stub host
/// that claims [`STUB_NODE_ID`]. The source and host are the same object.
pub(crate) fn mip_with_peers(
    local: Arc<dyn InferenceProvider>,
    peers: Vec<InferenceVenue>,
) -> InferenceRouter {
    let src = Arc::new(StubVenueSource { peers });
    InferenceRouter::with_peer_source(
        local,
        src.clone(),
        src,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    )
}

// ── Mock peer HTTP server (the "Founder" role) ──────────────
//
// Serves:
//   * GET /oicp/v1/capabilities  → JSON ProviderManifest (9B + 27B)
//   * POST /v1/chat/completions  → SSE stream of canned deltas
// Nothing else. This is the minimum surface `InferenceRouter`
// consults when routing a single streaming completion.

pub(crate) const PEER_RESPONSE_TEXT: &str = "Hello from Founder's 9B slot.";

#[derive(Deserialize)]
pub(crate) struct StreamQuery {
    #[serde(default)]
    pub(crate) stream: Option<bool>,
}

pub(crate) fn two_slot_manifest(features: Vec<String>) -> ProviderManifest {
    // Two slots, same shape as `build_self_manifest` produces on a
    // real high-profile Founder after the multi-slot change:
    // Qwen3.5-9B (5.5 GB, score ~0.75 on DeepQuery) and
    // Qwen3.5-27B (16.5 GB, score ~0.85 on DeepQuery). Since both
    // are general-hint at Normal latency, the request's Extended
    // latency hits both at the adjacent-class bonus and the 27B's
    // higher affinity wins — unless the pick_better tie-break
    // (smaller size_gb) promotes the 9B. The v0.3 wire tests below
    // use a different scoring assumption; the key assertion is that
    // routing reaches the peer at all, not which slot it lands on.
    ProviderManifest {
        oicp_version: OICP_VERSION.into(),
        provider: None,
        models: vec![
            ProviderModel {
                id: "Qwen3.5-9B.test".into(),
                base_model: None,
                quantization: None,
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded: true,
                    estimated_tokens_per_sec: None,
                    estimated_ttft_ms: None,
                    estimated_load_time_sec: None,
                },
                size_gb: Some(5.5),
                claims: vec![CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Normal,
                    32_768,
                    4_000,
                    0.80,
                )],
                fingerprint: None,
            },
            ProviderModel {
                id: "Qwen3.5-27B.test".into(),
                base_model: None,
                quantization: None,
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded: true,
                    estimated_tokens_per_sec: None,
                    estimated_ttft_ms: None,
                    estimated_load_time_sec: None,
                },
                size_gb: Some(16.5),
                // Equal affinity to the 9B so the score_manifest_for_request
                // pick_better tiebreaker falls to the smaller size_gb
                // (5.5 < 16.5 → Qwen3.5-9B wins). Mirrors the original
                // v0.2 assumption that DeepQuery scores 1.0 on both
                // slots.
                claims: vec![CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Normal,
                    32_768,
                    4_000,
                    0.80,
                )],
                fingerprint: None,
            },
        ],
        knowledge: None,
        federation: None,
        features,
    }
}

pub(crate) async fn capabilities_handler() -> impl IntoResponse {
    Json(two_slot_manifest(Vec::new()))
}

/// Capabilities of a peer that DOES advertise the forced-choice feature.
pub(crate) async fn capabilities_handler_fc() -> impl IntoResponse {
    Json(two_slot_manifest(vec![
        oicp_types::features::X_FORCED_CHOICE.to_string(),
    ]))
}

/// Drain a peer stream into the joined text, asserting every chunk is Ok.
/// Shared by the model-resolution and identity parts.
pub(crate) async fn drain(
    stream: impl futures::Stream<Item = Result<String, sovereign_contracts::error::Error>>,
) -> String {
    let mut collected = String::new();
    let mut stream = Box::pin(stream);
    while let Some(chunk) = stream.next().await {
        collected.push_str(&chunk.expect("stream chunk should be Ok"));
    }
    collected
}
