// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance for the named-model routing fix (note c5678d34, finding 1).
//!
//! Observed live on 2026-07-28, on a healthy two-node cluster: `POST
//! /v1/chat/completions` naming the shared model returned
//! 503 "no node in this mesh advertises model X", while `GET /v1/models` listed
//! that exact id and an UNNAMED request answered correctly from the same child.
//! A peer addressing the shared model by name — the whole point of sharing a
//! model on a mesh — got a refusal from a cluster that was working.
//!
//! Cause: `build_self_manifest` is a SNAPSHOT of the local provider, taken once
//! when `MeshInferenceProvider` is constructed. At that moment the distributed
//! slot has deliberately not spawned, so the facade's `is_serving()` gate is
//! false, the Slow tier answers with the small fast model, and the heavyweight
//! model is absent from the manifest entirely. Minutes later the discovery tick
//! warms the workers and respawns the child into Serving — and nothing rebuilt
//! the snapshot.
//!
//! This test drives the REAL refresher (`spawn_self_manifest_refresh`), not a
//! copy of it, because the defect was a missing subscription: a
//! reimplementation here would paper over exactly the thing under test.
//!
//! The properties, in order:
//!   1. before the child serves, naming the model is refused — a node must not
//!      advertise what it cannot serve;
//!   2. once the child reaches Serving, the SAME named request routes to it;
//!   3. it routes under BOTH the slot name and the GGUF stem, since the
//!      distributed route claims both;
//!   4. after `retire()`, naming it is refused again — un-advertising has to be
//!      as prompt as advertising, or peers route into a parked slot.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::Stream;
use sovereign_compute::child::ChildLifecycle;
use sovereign_compute::manager::{
    build_compute_layer_with_distributed, DistributedPrimarySpec, DynamicChildSlot,
};
use sovereign_contracts::setup_config::ComputeSection;
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, Depth, InferenceProvider, ProviderCapabilities, Speed,
};
use sovereign_core::error::{Error, Result};
use sovereign_mesh::daemon::PeerInferenceEndpoint;
use sovereign_mesh::peer_inference::{MeshInferenceProvider, PeerEndpointSource};

/// The daemon binary — its `--compute-child` arm runs the mock child.
const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-daemon");

const SLOT_NAME: &str = "shared-primary";
const STEM: &str = "Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("named-route-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn spec(dir: &PathBuf) -> DistributedPrimarySpec {
    DistributedPrimarySpec {
        name: SLOT_NAME.to_string(),
        model: dir.join("unused-for-mock.gguf"),
        context_size: None,
        n_gpu_layers: None,
        // The stem the GGUF would be addressed by — the distributed route
        // claims this alongside the slot name.
        model_ids: vec![STEM.to_string()],
        handoff_path: dir.join("distribution.json"),
    }
}

/// Stands in for the in-process engine the daemon keeps for the FAST slot while
/// the primary is withheld to the child. Answering every tier with a small
/// model is exactly what makes the bug reproducible: the manifest's Slow row is
/// minted from this, not from the child, until a refresh happens.
struct InProcessFastOnly;

#[async_trait::async_trait]
impl InferenceProvider for InProcessFastOnly {
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
        Err(Error::NotImplemented("fast-only stub".into()))
    }

    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("fast-only stub".into()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("fast-only stub".into()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        "Qwen3.5-0.8B-UD-Q6_K_XL".to_string()
    }
}

struct NoPeers;

#[async_trait::async_trait]
impl PeerEndpointSource for NoPeers {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        Vec::new()
    }
}

async fn wait_serving(slot: &DynamicChildSlot, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if matches!(slot.status().lifecycle, ChildLifecycle::Serving) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn named(model: &str) -> CompletionRequest {
    let mut r = CompletionRequest::default();
    r.prompt = "hello".into();
    r.model_id = Some(model.to_string());
    r.preferred_speed = Speed::Slow;
    r
}

/// Poll a named request until it stops being refused — the refresh is driven by
/// an async task reacting to a lifecycle transition, so it is eventually, not
/// instantly, consistent.
async fn wait_named_routes(mip: &MeshInferenceProvider, model: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if mip.complete(&named(model)).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn wait_named_refused(mip: &MeshInferenceProvider, model: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if mip.complete(&named(model)).await.is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn a_named_request_for_the_distributed_primary_routes_once_the_child_serves() {
    let dir = scratch("named");
    let section = ComputeSection {
        enabled: true,
        slot: vec![],
        distributed_primary: true,
        ..Default::default()
    };

    let (facade, manager) = build_compute_layer_with_distributed(
        &section,
        Arc::new(InProcessFastOnly),
        BIN.into(),
        dir.clone(),
        Some(spec(&dir)),
    )
    .expect("distributed compute layer");

    let slot = manager
        .distributed_slot()
        .expect("distributed_primary = true builds the slot");

    // The mesh wrapper takes its manifest snapshot HERE — while the slot exists
    // but has never spawned. This is the exact ordering that produced the bug.
    let mip = Arc::new(MeshInferenceProvider::with_peer_source(
        facade,
        Arc::new(NoPeers),
    ));

    // The real wiring under test.
    sovereign_cli_daemon::spawn_self_manifest_refresh(Arc::clone(&mip), Some(Arc::clone(&slot)));

    // ── 1. A model we cannot serve must not be advertised.
    let before = mip.complete(&named(SLOT_NAME)).await;
    assert!(
        before.is_err(),
        "an unspawned slot must not answer a named request: {before:?}"
    );

    // ── 2. The child reaches Serving; nothing else in the system is touched.
    slot.respawn_mock(8, 0);
    assert!(
        wait_serving(&slot, Duration::from_secs(20)).await,
        "child never reached serving"
    );

    assert!(
        wait_named_routes(&mip, SLOT_NAME, Duration::from_secs(10)).await,
        "THE REGRESSION: the child is serving but naming the model is still \
         refused — the self-manifest was never rebuilt"
    );
    let resp = mip
        .complete(&named(SLOT_NAME))
        .await
        .expect("named request routes");
    assert_eq!(
        resp.text, "mock response",
        "the answer must come from the CHILD, not the in-process fast slot"
    );

    // ── 3. Both ids the distributed route claims must work. Pinned because the
    // slot name and the GGUF stem are distinct advertisement rows (the Slow slot
    // row and an extras row) and it is easy to fix one and leave the other 503ing.
    let by_stem = mip
        .complete(&named(STEM))
        .await
        .expect("the GGUF stem must route too");
    assert_eq!(by_stem.text, "mock response");

    // ── 4. Un-advertise on retire, or peers route into a parked slot.
    slot.retire("no eligible RPC workers");
    assert!(
        wait_named_refused(&mip, SLOT_NAME, Duration::from_secs(10)).await,
        "a retired slot must stop being advertised"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
