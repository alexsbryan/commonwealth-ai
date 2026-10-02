// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one router construction, read from what it routes (pb-serve-ranks).
//! Each test points the svrnmesh root at its own temp dir, so the pinned
//! pods `rank` loads are the test's, never the operator's.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use oicp_types::{CapabilityHint, InferenceRequirements, LatencyClass, ShardingPrivacy};
use sovereign_contracts::reloadable_provider::ReloadableProvider;
use sovereign_contracts::venue::{InferenceVenue, VenueSource};
use sovereign_contracts::venue_host::VenueHost;
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, InferenceProvider, ProviderCapabilities, Result, Speed,
};

use super::rank;

struct NoPeers;

#[async_trait]
impl VenueSource for NoPeers {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        Vec::new()
    }
}

#[async_trait]
impl VenueHost for NoPeers {}

/// The mock, holding `id` as its primary and recording the model each
/// request reached it naming: the only way to see what the router pinned.
struct Holds {
    id: &'static str,
    saw: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl InferenceProvider for Holds {
    async fn complete(&self, r: &CompletionRequest) -> Result<CompletionResponse> {
        self.saw.lock().unwrap().push(r.model_id.clone());
        let inner = sovereign_compute::mock::MockProvider {
            tokens: 1,
            delay: std::time::Duration::ZERO,
        };
        inner.complete(r).await
    }
    async fn complete_stream(
        &self,
        r: &CompletionRequest,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        self.saw.lock().unwrap().push(r.model_id.clone());
        Err(sovereign_contracts::error::Error::NotImplemented(
            "stub".into(),
        ))
    }
    async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
        Err(sovereign_contracts::error::Error::NotImplemented(
            "stub".into(),
        ))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Slow,
            relative_reasoning: sovereign_contracts::Depth::Moderate,
        }
    }
    fn model_id_for(&self, _speed: Speed) -> String {
        self.id.to_string()
    }
    fn resident_slots(&self) -> Vec<sovereign_contracts::traits::ResidentSlot> {
        vec![sovereign_contracts::traits::ResidentSlot {
            role: "primary".to_string(),
            model_id: self.id.to_string(),
            resident: true,
            size_bytes: None,
            transitioning: false,
            placement: None,
        }]
    }
}

/// The node ids of the venues the router would rank now.
async fn peer_ids(
    router: &sovereign_serving_host::peer_inference::InferenceRouter,
) -> Vec<Option<String>> {
    router
        .observation_snapshot()
        .await
        .peers
        .into_iter()
        .map(|p| p.node_id)
        .collect()
}

/// The process environment these tests set, held for a test's whole body:
/// `cargo test` (the lift sandbox's runner) runs them as threads of one
/// process, where nextest gives each its own.
static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// This test's own svrnmesh root, with the env lock held until the guard
/// drops, and the shared-model pin cleared.
async fn own_root() -> (tokio::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    let held = ENV.lock().await;
    let root = tempfile::tempdir().expect("temp root");
    std::env::set_var("SVRNMESH_DATA_DIR", root.path());
    std::env::remove_var("SOVEREIGN_SHARED_MODEL_ID");
    (held, root)
}

/// On a shared-model fleet config, the FIRST primary turn after cold start
/// goes to the shared model: `rank` sets it at the one construction, where
/// before pb-serve-ranks only a reload did (provider.rs:179-181 at
/// 474236221). Failing input: drop `set_shared_model_id` from `rank`, and the
/// turn reaches the provider naming no model.
#[tokio::test]
async fn the_first_primary_turn_after_cold_start_goes_to_the_shared_model() {
    let (_held, _root) = own_root().await;
    std::env::set_var("SOVEREIGN_SHARED_MODEL_ID", "shared-model");
    let saw = Arc::new(Mutex::new(Vec::new()));
    let local = Arc::new(Holds {
        id: "shared-model",
        saw: Arc::clone(&saw),
    });
    let ranking = rank(local, Arc::new(NoPeers), Arc::new(NoPeers)).await;
    let turn = CompletionRequest::new("hi")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(ShardingPrivacy::MeshAllowed),
        );
    ranking.provider.complete(&turn).await.expect("served");
    assert_eq!(
        saw.lock().unwrap().first().cloned(),
        Some(Some("shared-model".to_string())),
        "the first primary turn after cold start must name the shared model"
    );
}

/// The router `rank` builds ranks the pinned worker pods persisted on disk,
/// and a reload, which swaps serve's cell under that router and hands the
/// same router back (edf4215fd), keeps them: the reload router had none
/// before pb-serve-ranks (pb-serving-proofs (a)). Failing input: build the
/// router over the roster's venues alone, without the pinned source.
#[tokio::test]
async fn a_reload_keeps_the_pinned_pods_the_one_router_ranks() {
    let (_held, root) = own_root().await;
    let owner = sovereign_contracts::worker_pod::derive_signing_key(&[55u8; 32]);
    let (blob, _) = sovereign_contracts::worker_pod::mint_bootstrap(
        sovereign_contracts::worker_pod::BootstrapInputs {
            job_id: "pinned-job".into(),
            owner_signing: &owner,
            expected_uploads: std::collections::BTreeMap::new(),
            ttl_seconds: 3600,
            seed_override: Some([9u8; 32]),
        },
    )
    .expect("a bootstrap blob");
    let pod_id = sovereign_serving_host::pinned_transport::synthetic_node_id_from_seed(&blob.seed);
    let snapshot = sovereign_serving_host::pinned_pod_snapshot::PinnedPodSnapshot::new(
        "vast-pinned",
        "203.0.113.5",
        9742,
        blob,
        sovereign_serving_host::pinned_worker_source::PodCapabilities {
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
        },
    );
    sovereign_serving_host::pinned_pod_snapshot::save_snapshot(
        &root.path().join("worker-pods"),
        &snapshot,
    )
    .expect("snapshot saved");

    let cell = Arc::new(ReloadableProvider::new(
        Arc::new(sovereign_compute::mock::MockProvider {
            tokens: 1,
            delay: std::time::Duration::ZERO,
        }),
        Default::default(),
    ));
    let ranking = rank(
        Arc::clone(&cell) as Arc<dyn InferenceProvider>,
        Arc::new(NoPeers),
        Arc::new(NoPeers),
    )
    .await;
    let pod = pod_id.to_hex();
    let ranks_pod =
        |peers: Vec<Option<String>>| peers.iter().any(|p| p.as_deref() == Some(pod.as_str()));
    assert!(
        ranks_pod(peer_ids(&ranking.router).await),
        "the router ranks the pinned pod on disk"
    );

    // serve's reload: the cell swaps under the router boot built.
    cell.swap(
        Arc::new(sovereign_compute::mock::MockProvider {
            tokens: 2,
            delay: std::time::Duration::ZERO,
        }),
        Default::default(),
    );
    assert!(
        ranks_pod(peer_ids(&ranking.router).await),
        "after a reload the router still ranks the pinned pod"
    );
}
