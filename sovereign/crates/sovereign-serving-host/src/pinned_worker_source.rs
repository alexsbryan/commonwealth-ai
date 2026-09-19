// SPDX-License-Identifier: AGPL-3.0-or-later
//! Endpoint sources for pinned worker pods.
//!
//! Spec: `sovereign/docs/PINNED_WORKER_AS_INFERENCE_PEER.md`.
//!
//! Two types live here:
//!
//! - [`PinnedWorkerEndpointSource`] — yields one
//!   [`InferenceVenue`] per registered pinned pod. The
//!   endpoint's `transport` field carries the TLS-pinned reqwest
//!   client + worker bearer; everything else mirrors the gossiped
//!   mesh peer shape so the scheduler in `peer_inference.rs` doesn't
//!   need to know the difference.
//!
//! - [`CompositeVenueSource`] — concatenates a mesh source (the
//!   live `EmbeddedDaemon`) with one or more pinned sources. The
//!   `InferenceRouter` is constructed against the composite so
//!   `select_peer` ranks pinned pods alongside gossiped peers under
//!   the same OICP scoring.
//!
//! ## What this file does NOT do
//!
//! - **Inference dispatch.** That's the scheduler's job; this file
//!   just publishes endpoint descriptors.
//! - **Capability scoring.** The OICP manifest fetch in
//!   `peer_inference.rs` calls the endpoint's `/oicp/v1/capabilities`
//!   — for pinned pods, that's the proxy-served manifest on `:9742`
//!   (see `worker_inference_proxy.rs`).
//! - **Pod lifecycle.** `pod up` / `pod down` (sovereign-cli) manage
//!   the snapshot files this source loads. This module owns nothing
//!   on disk.
//!
//! ## Affinity carve-out
//!
//! Pinned pods have no "users" beyond the owner; the mesh's
//! local-affinity scoring (which biases towards peers that prefer
//! their own users) makes no sense for them. The scoring path in
//! `oicp_select` treats `transport.is_some()` as the signal to use
//! `effective_affinity = 1.0` rather than reading the manifest's
//! advertised affinity. See [`peer_inference.rs`] for where that
//! carve-out lands.

use std::sync::Arc;

use async_trait::async_trait;
use commonwealth_core::ids::NodeId;
use tokio::sync::RwLock;

use sovereign_scheduler::venue::{InferenceVenue, VenueSource};

use crate::pinned_transport::{
    build_pinned_transport, synthetic_node_id_from_seed, PinnedTransport, TransportError,
};
use sovereign_contracts::worker_pod::BootstrapBlob;

/// Operator-stamped capabilities for a pinned pod. Defaults are
/// conservative; production callers fill in the real RAM/benchmark
/// pair from the Vast offer they picked when calling `pod up`.
///
/// The values feed the same scoring fields `EmbeddedDaemon` populates
/// from gossip — `system_ram_gb` for the RAM-floor heuristic,
/// `benchmark` for throughput extrapolation. A pod that misrepresents
/// itself here pays the cost the same way a misconfigured mesh peer
/// does: it gets routed too much or too little load. No correctness
/// risk, only ranking accuracy.
#[derive(Debug, Clone)]
pub struct PodCapabilities {
    pub system_ram_gb: u32,
    pub benchmark: Option<oicp_types::BenchmarkResult>,
    /// Self-reported concurrent inference count from the pod. `None`
    /// until the pod's `/internal/worker/health` is wired to report
    /// it — the scheduler falls back to `peer_observations`-based
    /// load tracking in that case, identical to gossiped peers that
    /// pre-date the in-flight field.
    pub current_in_flight: Option<u32>,
}

impl Default for PodCapabilities {
    fn default() -> Self {
        Self {
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
        }
    }
}

/// One row in the source — everything needed to build a
/// `InferenceVenue` for a single pinned pod.
#[derive(Clone)]
pub struct PinnedPod {
    /// Synthetic NodeId derived from the bootstrap blob's seed.
    /// Stable across CLI invocations against the same pod, so the
    /// scheduler's per-peer throughput cache + ledger keys keep
    /// accumulating after reconnects.
    pub node_id: NodeId,
    /// Display name used in routing-decision logs. By convention
    /// `pod-<short-vast-id>` so an operator can spot a pinned pod
    /// in a glance.
    pub name: String,
    /// HTTPS base URL the scheduler hands to the
    /// `RemoteApiProvider` shim. Single-entry by design — a pod has
    /// exactly one TLS-pinned address.
    pub base_url: String,
    /// TLS-pinned transport (client + bearer). Cloned onto every
    /// `InferenceVenue` we yield, so the cloning is cheap
    /// (Arc-shared client).
    pub transport: PinnedTransport,
    /// Operator-stamped capabilities; populated when the pod is
    /// registered via `pod up`. Defaults to a generic mid-range
    /// profile if the operator didn't supply explicit values.
    pub capabilities: PodCapabilities,
}

impl PinnedPod {
    /// Build a fresh `PinnedPod` from a bootstrap blob + the pod's
    /// public host:port. The label flows into the transport's tracing
    /// field; the synthetic node id is derived from the blob's seed.
    pub fn from_blob(
        blob: &BootstrapBlob,
        host: &str,
        port: u16,
        capabilities: PodCapabilities,
    ) -> Result<Self, TransportError> {
        let node_id = synthetic_node_id_from_seed(&blob.seed);
        // Take the leading 8 bytes of the job id (or the synthetic id
        // if the job id is short) for the display label. The job id is
        // operator-chosen and may be long; a short label keeps the
        // tracing line readable.
        let short = blob
            .job_id
            .split('-')
            .next()
            .map(|s| s.chars().take(8).collect::<String>())
            .unwrap_or_else(|| node_id.to_string());
        let name = format!("pod-{short}");
        let base_url = format!("https://{host}:{port}/v1");
        let transport = build_pinned_transport(blob, name.clone())?;
        Ok(Self {
            node_id,
            name,
            base_url,
            transport,
            capabilities,
        })
    }
}

/// Endpoint source for pinned worker pods. Hot-swappable at runtime —
/// `pod up` adds a pod, `pod down` removes one, `pipeline run`
/// reads the current set. Lock granularity is whole-source because
/// the pod list is small (<= a few dozen) and reads are rare.
#[derive(Default)]
pub struct PinnedWorkerEndpointSource {
    inner: Arc<RwLock<Vec<PinnedPod>>>,
}

impl PinnedWorkerEndpointSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from a fixed initial set of pods. Most useful for
    /// `pipeline run --extra-worker` invocations that load every
    /// snapshot up-front; long-running daemons (if we ever surface
    /// pinned pods there) would use [`Self::register`] /
    /// [`Self::deregister`] instead.
    pub fn from_pods(pods: Vec<PinnedPod>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(pods)),
        }
    }

    pub async fn register(&self, pod: PinnedPod) {
        let mut pods = self.inner.write().await;
        // Replace any existing entry for the same node id — a
        // re-registration carries fresh transport / capabilities.
        pods.retain(|p| p.node_id != pod.node_id);
        pods.push(pod);
    }

    pub async fn deregister(&self, node_id: &NodeId) -> bool {
        let mut pods = self.inner.write().await;
        let before = pods.len();
        pods.retain(|p| &p.node_id != node_id);
        before != pods.len()
    }

    pub async fn count(&self) -> usize {
        self.inner.read().await.len()
    }

    async fn endpoints(&self) -> Vec<InferenceVenue> {
        self.inner
            .read()
            .await
            .iter()
            .map(|p| InferenceVenue {
                node_id: p.node_id,
                name: p.name.clone(),
                base_urls: vec![p.base_url.clone()],
                system_ram_gb: p.capabilities.system_ram_gb,
                benchmark: p.capabilities.benchmark.clone(),
                current_in_flight: p.capabilities.current_in_flight,
                // Pinned pods don't gossip availability — neutral.
                inference_availability: None,
                gossip_last_seen_unix: 0,
                // The TLS handle itself does NOT travel with the venue (the
                // scheduler may not name `PinnedTransport`); this bool is the
                // only transport fact the ranker reads. The router resolves
                // the handle through `PinnedTransportResolver`.
                pinned_transport: true,
            })
            .collect()
    }
}

#[async_trait]
impl VenueSource for PinnedWorkerEndpointSource {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        self.endpoints().await
    }
}

#[async_trait]
impl crate::venue_host::PinnedTransportResolver for PinnedWorkerEndpointSource {
    /// A pinned pod's TLS handle, by its synthetic node id. This is what
    /// `InferenceRouter::set_pinned_transports` installs.
    async fn resolve(&self, node_id: &NodeId) -> Option<PinnedTransport> {
        self.inner
            .read()
            .await
            .iter()
            .find(|p| &p.node_id == node_id)
            .map(|p| p.transport.clone())
    }
}

/// Concatenates a mesh source (typically `EmbeddedDaemon`) with one
/// or more pinned sources. Hands a unified `VenueSource` to
/// `InferenceRouter::builder(...).candidates(..)` so the scheduler scores
/// pinned + gossiped peers in the same pool.
///
/// Ordering: mesh-source endpoints first, then pinned. The scheduler
/// doesn't care about order (it ranks by score), but a stable
/// ordering makes routing-decision logs reproducible.
pub struct CompositeVenueSource {
    mesh: Arc<dyn VenueSource>,
    pinned: Arc<PinnedWorkerEndpointSource>,
}

impl CompositeVenueSource {
    pub fn new(mesh: Arc<dyn VenueSource>, pinned: Arc<PinnedWorkerEndpointSource>) -> Self {
        Self { mesh, pinned }
    }
}

#[async_trait]
impl VenueSource for CompositeVenueSource {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        let mut out = self.mesh.candidates().await;
        out.extend(self.pinned.candidates().await);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use sovereign_contracts::worker_pod::{mint_bootstrap, BootstrapInputs};
    use std::collections::BTreeMap;

    fn fixed_owner_key() -> SigningKey {
        SigningKey::from_bytes(&[19u8; 32])
    }

    fn mint(seed: [u8; 32], job: &str) -> BootstrapBlob {
        let owner = fixed_owner_key();
        let (blob, _) = mint_bootstrap(BootstrapInputs {
            job_id: job.into(),
            owner_signing: &owner,
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 3600,
            seed_override: Some(seed),
        })
        .expect("mint");
        blob
    }

    #[tokio::test]
    async fn empty_pinned_source_yields_no_endpoints() {
        let source = PinnedWorkerEndpointSource::new();
        assert_eq!(source.candidates().await.len(), 0);
    }

    #[tokio::test]
    async fn pinned_pod_round_trips_through_source() {
        let blob = mint([1u8; 32], "abc12345-job");
        let pod = PinnedPod::from_blob(&blob, "203.0.113.10", 9742, PodCapabilities::default())
            .expect("pod from blob");
        let expected_id = pod.node_id;
        let source = PinnedWorkerEndpointSource::from_pods(vec![pod]);
        let endpoints = source.candidates().await;
        assert_eq!(endpoints.len(), 1);
        let ep = &endpoints[0];
        assert_eq!(ep.node_id, expected_id);
        assert_eq!(ep.base_urls, vec!["https://203.0.113.10:9742/v1"]);
        assert!(ep.pinned_transport, "transport must be populated");
        assert!(ep.name.starts_with("pod-"));
    }

    #[tokio::test]
    async fn register_replaces_same_node_id() {
        let blob = mint([2u8; 32], "same-seed");
        let source = PinnedWorkerEndpointSource::new();

        let pod1 = PinnedPod::from_blob(&blob, "host1", 9742, PodCapabilities::default()).unwrap();
        source.register(pod1).await;

        // Second registration with same blob (same seed → same node_id)
        // but different host should replace, not duplicate.
        let pod2 = PinnedPod::from_blob(&blob, "host2", 9742, PodCapabilities::default()).unwrap();
        source.register(pod2).await;

        assert_eq!(source.count().await, 1);
        let endpoints = source.candidates().await;
        assert_eq!(endpoints[0].base_urls, vec!["https://host2:9742/v1"]);
    }

    #[tokio::test]
    async fn deregister_removes_pod() {
        let blob = mint([3u8; 32], "gone");
        let source = PinnedWorkerEndpointSource::new();
        let pod = PinnedPod::from_blob(&blob, "h", 9742, PodCapabilities::default()).unwrap();
        let id = pod.node_id;
        source.register(pod).await;
        assert_eq!(source.count().await, 1);
        assert!(source.deregister(&id).await);
        assert_eq!(source.count().await, 0);
        // Idempotent: dropping again is a no-op.
        assert!(!source.deregister(&id).await);
    }

    /// Stub mesh source that yields a fixed peer list. It carries no ledger:
    /// the pinned-pod carve-out lives in `peer_inference`'s
    /// `ledger_emitter_for_venue`, keyed on the venue's `pinned_transport`.
    struct StubMesh {
        peers: Vec<InferenceVenue>,
    }

    impl StubMesh {
        fn new(peers: Vec<InferenceVenue>) -> Self {
            Self { peers }
        }
    }

    #[async_trait]
    impl VenueSource for StubMesh {
        async fn candidates(&self) -> Vec<InferenceVenue> {
            self.peers.clone()
        }
    }

    fn mesh_peer(node_id_seed: u128, name: &str) -> InferenceVenue {
        InferenceVenue {
            node_id: NodeId::from_u128(node_id_seed),
            name: name.into(),
            base_urls: vec!["http://10.0.0.1:9741/v1".into()],
            system_ram_gb: 32,
            benchmark: None,
            current_in_flight: None,
            inference_availability: None,
            gossip_last_seen_unix: 0,
            pinned_transport: false,
        }
    }

    #[tokio::test]
    async fn composite_concatenates_endpoints() {
        let mesh = Arc::new(StubMesh::new(vec![
            mesh_peer(1, "mesh-a"),
            mesh_peer(2, "mesh-b"),
        ]));
        let pinned = Arc::new(PinnedWorkerEndpointSource::from_pods(vec![
            PinnedPod::from_blob(
                &mint([4u8; 32], "pin"),
                "h",
                9742,
                PodCapabilities::default(),
            )
            .unwrap(),
        ]));
        let composite = CompositeVenueSource::new(mesh.clone(), pinned);
        let endpoints = composite.candidates().await;
        assert_eq!(endpoints.len(), 3);
        assert_eq!(endpoints[0].name, "mesh-a");
        assert_eq!(endpoints[1].name, "mesh-b");
        assert!(endpoints[2].name.starts_with("pod-"));
    }

    #[tokio::test]
    async fn composite_with_zero_pinned_matches_mesh() {
        let mesh = Arc::new(StubMesh::new(vec![mesh_peer(7, "solo")]));
        let pinned = Arc::new(PinnedWorkerEndpointSource::new());
        let composite = CompositeVenueSource::new(mesh.clone(), pinned);
        assert_eq!(composite.candidates().await.len(), 1);
    }
}
