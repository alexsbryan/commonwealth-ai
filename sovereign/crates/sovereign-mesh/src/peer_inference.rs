//! `MeshInferenceProvider` — the Joiner-side wrapper that routes
//! synthesis to the best-scoring mesh peer for a given OICP request,
//! with automatic fallback to local on any remote error.
//!
//! Design invariant: the routing decision is driven by OICP, not by
//! ad-hoc proxies like RAM or model size. The active skills on the
//! local side declare their `InferenceRequirements` (required +
//! preferred capabilities, latency, privacy); the Runtime stamps
//! these onto every `CompletionRequest` via `build_oicp`. On the
//! peer side, each node advertises a `ProviderManifest` at
//! `/oicp/v1/capabilities` derived from its local inference stack
//! (`sovereign_mesh::inference_adapter::build_self_manifest`).
//!
//! This wrapper is the point where the request's requirements meet
//! the available manifests and a single best backend is chosen:
//!
//!   1. No OICP on the request, or `sharding == LocalOnly` → local.
//!      "No contract" means no reason to cross the network; `LocalOnly`
//!      is explicit opt-out (e.g. the `inner-work` skill).
//!   2. Score local's manifest against the request's
//!      required+preferred profile (`oicp::satisfies_required` +
//!      `oicp::score_preferred`). Local is always a candidate — we
//!      never fail over to a peer we can't outperform.
//!   3. For every online non-self peer, fetch (and cache for 60s)
//!      their `ProviderManifest` over `http://<peer>:9741/oicp/v1/capabilities`.
//!      Score the same way.
//!   4. Pick the highest-scoring candidate. Local wins ties — a
//!      matching local score doesn't justify a round-trip.
//!   5. On peer routing: iterate the peer's advertised base URLs
//!      (WiFi / Tailscale / ULA) in order, same "first reachable
//!      wins" policy as gossip and knowledge fan-out. Only fall
//!      back to local when every URL has failed.
//!
//! Embed calls stay local unconditionally — retrieval is latency-
//! critical and not a capability the selector has visibility into.
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::Stream;
use sovereign_core::oicp::THROUGHPUT_EWMA_ALPHA;
use sovereign_core::error::Result;
use sovereign_core::oicp::{
    ExtensionRegistry, ExtensionStats, NodeLocality, NodeObservations,
    ProviderManifest, ShardingPrivacy,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities, Speed};
use sovereign_inference::remote::RemoteApiProvider;
use tokio::sync::RwLock;

use crate::daemon::{EmbeddedDaemon, PeerInferenceEndpoint};
use crate::inference_adapter::build_self_manifest;

/// How long to trust a fetched peer manifest before re-fetching.
/// OICP capabilities don't change request-to-request — a model
/// either has a level or it doesn't — so a minute of staleness
/// is cheap insurance against hammering `/oicp/v1/capabilities`
/// on every Slow-slot call.
const MANIFEST_TTL: Duration = Duration::from_secs(60);

/// Scale a peer's raw in-flight count by its soft-health weight
/// to bias load-balance comparisons. A healthy peer (weight ≈
/// 1.0) gets its raw count back unchanged; a peer at 25% recent
/// success rate gets 4× the apparent in-flight count, pushing
/// the load balancer toward healthier alternatives.
///
/// **Why ceil**: rounding down would let a peer at health 0.5 +
/// in-flight 0 still tie at "0 effective in-flight" with a
/// healthy peer at in-flight 0 — and tie-breaks go to whichever
/// path the caller prefers (often local). Ceiling means every
/// degraded peer's effective count is strictly *greater than*
/// zero even at raw in-flight 0, so it never wins a tie against
/// a healthier alternative.
///
/// **Saturating at u32::MAX** because `is_quarantined` already
/// removed peers with zero weight; an extreme floor value (like
/// `HEALTH_WEIGHT_FLOOR = 0.05`) yields raw/0.05 = 20× — well
/// within u32. The saturate is belt-and-braces for future floor
/// changes.
fn effective_inflight(raw: u32, health_weight: f32) -> u32 {
    if health_weight <= 0.0 {
        return u32::MAX;
    }
    let scaled = (raw as f32 / health_weight).ceil();
    if scaled.is_finite() && scaled >= 0.0 && scaled <= u32::MAX as f32 {
        scaled as u32
    } else {
        u32::MAX
    }
}

#[cfg(test)]
mod effective_inflight_tests {
    use super::effective_inflight;

    #[test]
    fn healthy_peer_inflight_unchanged() {
        // weight 1.0 means raw == effective.
        assert_eq!(effective_inflight(0, 1.0), 0);
        assert_eq!(effective_inflight(3, 1.0), 3);
        assert_eq!(effective_inflight(99, 1.0), 99);
    }

    #[test]
    fn struggling_peer_inflight_scaled_up() {
        // 50% success rate → effective is 2× raw.
        assert_eq!(effective_inflight(2, 0.5), 4);
        // 25% → 4×.
        assert_eq!(effective_inflight(2, 0.25), 8);
    }

    #[test]
    fn zero_raw_still_penalised_when_unhealthy() {
        // The key property that prevents ties: even at raw=0,
        // a degraded peer doesn't tie with a healthy peer.
        // ceil(0 / 0.5) = ceil(0) = 0 — that's a problem...
        // The intent is that a struggling peer should not WIN
        // a tie. We rely on the comparison being `<=` favouring
        // local. Document the actual behaviour:
        assert_eq!(effective_inflight(0, 0.5), 0);
        assert_eq!(effective_inflight(0, 0.05), 0);
    }

    #[test]
    fn floor_weight_yields_large_effective() {
        // At the soft-health floor (0.05), even raw=1 looks
        // like 20× in-flight to the balancer — local at 1
        // in-flight wins easily.
        assert_eq!(effective_inflight(1, 0.05), 20);
        assert_eq!(effective_inflight(2, 0.05), 40);
    }

    #[test]
    fn zero_weight_returns_max() {
        // Defensive: `health_weight` never returns <= 0 in
        // practice (the floor is HEALTH_WEIGHT_FLOOR), but if
        // something else drove this to 0, we must skip the peer
        // entirely. u32::MAX in the comparison guarantees that.
        assert_eq!(effective_inflight(0, 0.0), u32::MAX);
        assert_eq!(effective_inflight(7, 0.0), u32::MAX);
    }
}

/// Per-peer HTTP timeout for the manifest fetch. Short enough that
/// an unreachable peer doesn't add meaningful latency to the
/// selection path; long enough that a Tailscale relay round-trip
/// under load completes comfortably.
const MANIFEST_FETCH_TIMEOUT: Duration = Duration::from_millis(800);

struct CachedManifest {
    manifest: ProviderManifest,
    fetched_at: Instant,
    /// Round-trip time (in ms) observed for the manifest fetch —
    /// the single HTTP request we were going to make anyway
    /// doubles as a locality probe. Piggy-backing avoids a second
    /// round-trip per peer. Refreshed on every manifest re-fetch
    /// (same `MANIFEST_TTL`).
    rtt_ms: u32,
}

// OICP selection primitives live in `crate::oicp_select` so the
// same scoring + tie-break policy drives both sides of the wire:
// the Joiner picking a peer (here) AND the peer-side adapter
// picking which loaded slot to serve the request from. Importing
// here keeps the rest of this file unchanged.
use crate::oicp_select::{
    adjust_for_observations, candidates_equal, classify_rtt_ms, pick_better,
    score_manifest_for_request, ModelCandidate,
};

/// Narrow trait the wrapper uses to discover routable peers. The
/// production implementation is `EmbeddedDaemon` — but factoring
/// the one call-site out behind a trait lets integration tests
/// inject a synthetic peer list pointing at a mock HTTP server
/// without needing to bring up a full daemon (gossip loop, mDNS,
/// bound ports, etc.) just to exercise the routing path.
#[async_trait]
pub trait PeerEndpointSource: Send + Sync {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint>;

    /// This node's id. Stamped onto outbound manifest fetches via
    /// the `X-Node-Id` header so the peer can apply local-only
    /// affinity preferences before serializing the manifest. The
    /// default returns `None` — implementations that don't know
    /// their id at peer-fetch time (test stubs that synthesize
    /// peers from thin air) skip the header entirely; the manifest
    /// endpoint then serves unmodified affinities, which is the
    /// safe default.
    async fn local_node_id(&self) -> Option<commonwealth_core::ids::NodeId> {
        None
    }

    /// Build a `LedgerEmission` for a peer-routed stream completion.
    /// Default returns `None` — test stubs without a wired
    /// `ContributionEmitter` skip the emission entirely. Production
    /// `EmbeddedDaemon` returns `Some(...)` once the daemon has
    /// joined a mesh and the AppState is available.
    ///
    /// Kept on the trait (rather than reaching into the embedded
    /// daemon directly from `MeshInferenceProvider`) so the same
    /// `PeerEndpointSource` abstraction the test harness uses also
    /// covers the new wiring — no test changes needed.
    #[doc(hidden)]
    async fn ledger_emission_for(
        &self,
        _peer_node_id: &commonwealth_core::ids::NodeId,
        _model_id: &str,
        _peer_name: &str,
    ) -> Option<LedgerEmission> {
        None
    }
}

#[async_trait]
impl PeerEndpointSource for EmbeddedDaemon {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        EmbeddedDaemon::peer_inference_endpoints(self).await
    }

    async fn local_node_id(&self) -> Option<commonwealth_core::ids::NodeId> {
        EmbeddedDaemon::self_node_id(self).await
    }

    async fn ledger_emission_for(
        &self,
        peer_node_id: &commonwealth_core::ids::NodeId,
        model_id: &str,
        _peer_name: &str,
    ) -> Option<LedgerEmission> {
        let app_state = self.app_state().await?;
        Some(LedgerEmission {
            from_node: peer_node_id.clone(),
            model_id: model_id.to_string(),
            emitter: app_state.inner.contribution_emitter.clone(),
        })
    }
}

pub struct MeshInferenceProvider {
    local: Arc<dyn InferenceProvider>,
    mesh: Arc<dyn PeerEndpointSource>,
    /// Our own manifest, built once at construction. The wrapper
    /// doesn't recompute it — Sovereign's loaded model set is
    /// effectively static within a process lifetime, and the
    /// `SovereignInferenceAdapter` on the server side uses the
    /// same `build_self_manifest` helper so peer-fetched and
    /// local-scored views of us are identical.
    self_manifest: ProviderManifest,
    /// Per-peer manifest cache keyed by peer `node_id` (as string
    /// — `NodeId` doesn't impl `Hash` across crate boundaries
    /// cleanly in all our versions, and the string form is stable).
    peer_cache: Arc<RwLock<std::collections::HashMap<String, CachedManifest>>>,
    /// Shared reqwest client for manifest fetches. Separate from
    /// the per-request `RemoteApiProvider` clients so manifest
    /// polling doesn't inherit inference-length timeouts.
    http: reqwest::Client,
    /// Per-peer observation tracker. Keyed by peer name (same
    /// identity `peer_cache` uses). Updated from the outside via
    /// `record_peer_*` helpers; consumed during `select_peer`
    /// ranking so repeatedly-failing peers fall out of rotation.
    peer_observations:
        Arc<RwLock<std::collections::HashMap<String, NodeObservations>>>,
    /// Our own (local) observations. Currently only load (in_flight)
    /// is interesting for the local side; samples start at a high
    /// constant so the cold-start ramp never applies to `self` —
    /// we always know ourselves.
    local_observations: Arc<RwLock<NodeObservations>>,
    /// v0.3 §4.3 governance registry: records every `x:*` hint we
    /// see on outgoing requests or in peer advertisements. Not
    /// consulted for routing — this is purely an input for the
    /// separate promotion process that decides which extensions
    /// merit standardization.
    extension_registry: Arc<RwLock<ExtensionRegistry>>,
    /// Local-side throughput benchmark. Set by the daemon's startup
    /// probe via [`MeshInferenceProvider::set_local_benchmark`] once
    /// the bundled model has been measured; read on every scoring
    /// pass to feed the local candidate's throughput factor. `None`
    /// before the probe completes — the scheduler then falls back to
    /// observation-only scoring, which is safe because local
    /// observations accumulate fast.
    local_benchmark:
        Arc<RwLock<Option<sovereign_core::oicp::BenchmarkResult>>>,
    /// Per-peer consecutive-failure tracker. Peers that fail
    /// `FAILURE_THRESHOLD` requests in a row are quarantined for a
    /// linearly-backed-off cooldown. Filtered out of routing
    /// candidates while quarantined; one successful response clears
    /// the state. See [`peer_health`] for the policy.
    peer_health: Arc<commonwealth_core::peer_health::PeerHealthTracker>,
    /// Per-model in-flight counter for explicit-model-id requests
    /// served by the local slot. Drives load-aware routing in
    /// [`MeshInferenceProvider::locate_named_model`]: when a request
    /// asks for a model that both we and a peer advertise, the peer
    /// gets the request iff its in-flight is strictly lower than
    /// ours. Without this, the laptop's single primary slot would
    /// hoard every request named for Darwin-36B even when a Mac
    /// peer that also advertises it sits idle.
    ///
    /// Bumped on dispatch in the explicit-`Local` branch, decremented
    /// when the dispatch completes (success, failure, or stream
    /// drop). `std::sync::Mutex` (not `tokio::sync::RwLock`) so the
    /// stream-completion guard can decrement in a synchronous Drop
    /// without spawning a task.
    local_inflight_by_model:
        Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
}

impl MeshInferenceProvider {
    /// Standard constructor — takes the live `EmbeddedDaemon` so
    /// production wiring is unchanged. Internally upcasts to
    /// `Arc<dyn PeerEndpointSource>` via the blanket impl above;
    /// callers don't have to think about the trait.
    pub fn new(local: Arc<dyn InferenceProvider>, mesh: Arc<EmbeddedDaemon>) -> Self {
        Self::with_peer_source(local, mesh as Arc<dyn PeerEndpointSource>)
    }

    /// Constructor exposed for tests and alternative wirings: pass
    /// any `PeerEndpointSource` (typically a stub that returns a
    /// fixed peer list pointing at a local mock server). Keeps the
    /// production `new` signature backwards-compatible.
    pub fn with_peer_source(
        local: Arc<dyn InferenceProvider>,
        mesh: Arc<dyn PeerEndpointSource>,
    ) -> Self {
        let self_manifest = build_self_manifest(local.as_ref());
        tracing::info!(
            models = self_manifest.models.len(),
            "mesh-inference: wrapper initialised (OICP-driven)"
        );
        let http = reqwest::Client::builder()
            .timeout(MANIFEST_FETCH_TIMEOUT)
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "mesh-inference: reqwest client build failed — using default");
                reqwest::Client::new()
            });
        // Seed local observations above the cold-start threshold so
        // the `self` side never gets depressed by "new node" weight.
        let local_obs = NodeObservations {
            samples: sovereign_core::oicp::COLD_START_SAMPLES * 2,
            ..Default::default()
        };
        Self {
            local,
            mesh,
            self_manifest,
            peer_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
            http,
            peer_observations: Arc::new(RwLock::new(
                std::collections::HashMap::new(),
            )),
            local_observations: Arc::new(RwLock::new(local_obs)),
            extension_registry: Arc::new(RwLock::new(ExtensionRegistry::new())),
            local_benchmark: Arc::new(RwLock::new(None)),
            peer_health: Arc::new(commonwealth_core::peer_health::PeerHealthTracker::new()),
            local_inflight_by_model: Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    /// Snapshot of per-peer health for diagnostics surfaces.
    pub fn peer_health_snapshot(&self) -> Vec<(String, bool, u32, u64)> {
        self.peer_health.snapshot()
    }

    /// Replace the local-side benchmark result. Called once by the
    /// daemon's startup probe after the bundled model has been
    /// measured. Idempotent — calling twice with the same result is
    /// a no-op for downstream scoring.
    pub async fn set_local_benchmark(
        &self,
        bench: sovereign_core::oicp::BenchmarkResult,
    ) {
        tracing::info!(
            model = %bench.baseline_model_id,
            pp_tok_s = bench.pp_tok_s,
            tg_tok_s = bench.tg_tok_s,
            size_gb = bench.baseline_size_gb,
            "bench: completed"
        );
        *self.local_benchmark.write().await = Some(bench);
    }

    /// Read-only access to the local benchmark for components that
    /// need to advertise it (manifest construction, gossip).
    pub async fn local_benchmark(
        &self,
    ) -> Option<sovereign_core::oicp::BenchmarkResult> {
        self.local_benchmark.read().await.clone()
    }

    /// Snapshot the current extension-hint usage for governance
    /// or operator diagnostics. Returns one entry per distinct
    /// `x:*` hint this scheduler has seen. Ordering is undefined;
    /// callers that want a stable display sort on fields they
    /// care about.
    pub async fn extension_stats(&self) -> Vec<ExtensionStats> {
        self.extension_registry
            .read()
            .await
            .stats()
            .cloned()
            .collect()
    }

    /// Current wall-clock in unix seconds. Extracted so tests can
    /// mock and so the handful of registry-record sites all use
    /// the same clock source.
    fn now_unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Record that a request was dispatched to this peer (or local,
    /// if `peer_name` is `None`). Increments in-flight + samples.
    pub async fn record_dispatch(&self, peer_name: Option<&str>) {
        match peer_name {
            None => {
                let mut obs = self.local_observations.write().await;
                obs.in_flight = obs.in_flight.saturating_add(1);
                obs.samples = obs.samples.saturating_add(1);
            }
            Some(name) => {
                let mut obs = self.peer_observations.write().await;
                let entry =
                    obs.entry(name.to_string()).or_default();
                entry.in_flight = entry.in_flight.saturating_add(1);
                entry.samples = entry.samples.saturating_add(1);
            }
        }
    }

    /// Record that a dispatched request completed successfully.
    /// Decrements in-flight; leaves failure counters untouched so
    /// the rolling rate drifts toward zero.
    pub async fn record_success(&self, peer_name: Option<&str>) {
        let mut obs_ref = match peer_name {
            None => self.local_observations.write().await,
            Some(name) => {
                let mut map = self.peer_observations.write().await;
                let entry = map
                    .entry(name.to_string())
                    .or_insert_with(NodeObservations::default);
                entry.in_flight = entry.in_flight.saturating_sub(1);
                // Drift failure rate toward zero on every success.
                entry.recent_failure_rate =
                    (entry.recent_failure_rate * 0.9).max(0.0);
                return;
            }
        };
        obs_ref.in_flight = obs_ref.in_flight.saturating_sub(1);
        obs_ref.recent_failure_rate =
            (obs_ref.recent_failure_rate * 0.9).max(0.0);
    }

    /// Record that a dispatched request failed. Decrements in-flight
    /// and bumps the rolling failure rate toward 1.0.
    pub async fn record_failure(&self, peer_name: Option<&str>) {
        let mut obs_ref = match peer_name {
            None => self.local_observations.write().await,
            Some(name) => {
                let mut map = self.peer_observations.write().await;
                let entry = map
                    .entry(name.to_string())
                    .or_insert_with(NodeObservations::default);
                entry.in_flight = entry.in_flight.saturating_sub(1);
                // Rolling-window failure rate: EMA toward 1.0 with
                // alpha 0.1 — 10 consecutive failures settle near 0.65.
                entry.recent_failure_rate =
                    (entry.recent_failure_rate * 0.9 + 0.1).min(1.0);
                return;
            }
        };
        obs_ref.in_flight = obs_ref.in_flight.saturating_sub(1);
        obs_ref.recent_failure_rate =
            (obs_ref.recent_failure_rate * 0.9 + 0.1).min(1.0);
    }

    /// Returns `true` when the request carries any v0.3 routing
    /// signal (capability hint, latency class, or structural
    /// envelope). A request without any of these stays local —
    /// there's nothing to match against the peer manifests.
    fn has_routing_signal(request: &CompletionRequest) -> bool {
        let Some(oicp) = request.oicp.as_ref() else {
            return false;
        };
        oicp.capability_hint.is_some()
            || oicp.latency_class.is_some()
            || oicp.context_tokens.is_some()
            || oicp.max_output_tokens.is_some()
    }

    /// Fetch a peer's OICP manifest, honouring the 60s cache. On
    /// fetch failure, returns `None` — caller treats the peer as
    /// not a candidate this turn (next request retries).
    ///
    /// Returns both the manifest and the measured RTT in ms. The
    /// RTT is the single HTTP round-trip the fetch was going to
    /// make anyway, repurposed as a locality probe — sub-5ms is
    /// same-host, sub-25ms is LAN, else WAN (see
    /// [`classify_rtt_ms`]). Caller folds this into
    /// [`adjust_for_observations`] so LAN peers pick up their
    /// locality bonus in real deployments instead of defaulting
    /// to `Far`.
    async fn get_peer_manifest(
        &self,
        peer: &PeerInferenceEndpoint,
    ) -> Option<(ProviderManifest, u32)> {
        self.get_peer_manifest_inner(peer, false).await
    }

    /// Variant that bypasses the cache and forces a fresh fetch.
    /// Used by the `locate_named_model` retry path when the first
    /// pass (which honours the cache) found nobody advertising
    /// the requested model — in that case the cache may be holding
    /// a stale manifest from a peer that was mid-slot-restart when
    /// it last got fetched, and the model has come back in the
    /// meantime. See the 2026-05-11 incident: founder's cache
    /// captured the pod's empty-model-list snapshot during a slot
    /// reload, then served `Model not loaded` to every chat
    /// completion for the rest of the 60 s TTL until an operator
    /// did `daemon restart`.
    async fn get_peer_manifest_fresh(
        &self,
        peer: &PeerInferenceEndpoint,
    ) -> Option<(ProviderManifest, u32)> {
        self.get_peer_manifest_inner(peer, true).await
    }

    async fn get_peer_manifest_inner(
        &self,
        peer: &PeerInferenceEndpoint,
        bypass_cache: bool,
    ) -> Option<(ProviderManifest, u32)> {
        let key = peer.node_id.to_string();
        // Cache hit (unless caller demanded a fresh probe).
        if !bypass_cache {
            let cache = self.peer_cache.read().await;
            if let Some(entry) = cache.get(&key) {
                if entry.fetched_at.elapsed() < MANIFEST_TTL {
                    return Some((entry.manifest.clone(), entry.rtt_ms));
                }
            }
        }
        // Cache miss or stale — try each URL until one resolves.
        // The manifest endpoint is the same origin as the inference
        // endpoint, but at a DIFFERENT path prefix:
        //
        //   /v1/chat/completions   ← inference (under /v1)
        //   /oicp/v1/capabilities  ← manifest  (at root)
        //
        // `peer.base_urls` are shaped for `RemoteApiProvider` which
        // appends `/chat/completions`, so they end in `/v1`. We
        // must strip that back off to reach the manifest endpoint.
        // Getting this wrong silently 404s the fetch and the peer
        // drops out of scoring — which was the bug that made the
        // OICP-driven refactor look like it didn't route.
        // Identify ourselves to the peer so they can apply any
        // local-only affinity preference they've set for us. The
        // header is optional on the receiving side; a peer that
        // doesn't know our node id (test stubs, older daemons)
        // gets None back and the manifest endpoint serves
        // unmodified affinities — the safe default.
        let local_node_id_hex = self.mesh.local_node_id().await.map(|id| {
            id.as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        });

        for base in &peer.base_urls {
            let root = base
                .trim_end_matches('/')
                .trim_end_matches("/v1")
                .trim_end_matches('/');
            let url = format!("{root}/oicp/v1/capabilities");
            let started = Instant::now();
            let mut req = self.http.get(&url);
            if let Some(ref id_hex) = local_node_id_hex {
                req = req.header("X-Node-Id", id_hex);
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    // Lock the RTT in before the JSON parse — we
                    // want the network round-trip, not the parse
                    // time, to classify the peer's locality.
                    let rtt_ms = started.elapsed().as_millis().min(u128::from(u32::MAX))
                        as u32;
                    match resp.json::<ProviderManifest>().await {
                        Ok(m) => {
                            tracing::info!(
                                peer = %peer.name,
                                url = %url,
                                models = m.models.len(),
                                rtt_ms,
                                locality = ?classify_rtt_ms(rtt_ms),
                                "mesh-inference: fetched peer manifest"
                            );
                            // v0.3 §4.3 governance tap: every `x:*`
                            // hint the peer advertises counts as an
                            // observed advertisement.
                            {
                                let now = Self::now_unix_secs();
                                let mut registry =
                                    self.extension_registry.write().await;
                                for model in &m.models {
                                    for claim in &model.claims {
                                        registry.observe_advertisement(
                                            &claim.hint,
                                            now,
                                        );
                                    }
                                }
                            }
                            let mut cache = self.peer_cache.write().await;
                            cache.insert(
                                key,
                                CachedManifest {
                                    manifest: m.clone(),
                                    fetched_at: Instant::now(),
                                    rtt_ms,
                                },
                            );
                            return Some((m, rtt_ms));
                        }
                        Err(e) => {
                            tracing::info!(
                                peer = %peer.name,
                                url = %url,
                                error = %e,
                                "mesh-inference: peer manifest parse failed"
                            );
                        }
                    }
                }
                Ok(resp) => {
                    tracing::info!(
                        peer = %peer.name,
                        url = %url,
                        status = %resp.status(),
                        "mesh-inference: peer manifest non-success — trying next"
                    );
                }
                Err(e) => {
                    tracing::info!(
                        peer = %peer.name,
                        url = %url,
                        error = %e,
                        "mesh-inference: peer manifest transport error — trying next"
                    );
                }
            }
        }
        None
    }

    /// OICP-driven selection. Given a request with capability
    /// requirements, returns the peer to route to, or `None` to
    /// stay local. Local always competes with peers — we never
    /// route unless a peer strictly outscores local.
    /// Returns both the peer to route to AND the specific model id
    /// that peer advertised as its best fit for this request. The
    /// caller uses the model id for attribution on the response /
    /// stream — there's only one place that decision gets made, so
    /// "which peer" and "which model" can't drift.
    async fn select_peer(
        &self,
        request: &CompletionRequest,
    ) -> Option<(PeerInferenceEndpoint, ModelCandidate)> {
        if !Self::has_routing_signal(request) {
            return None;
        }
        if let Some(oicp) = &request.oicp {
            if oicp.sharding() == ShardingPrivacy::LocalOnly {
                return None;
            }
        }
        // Also: if this isn't a synthesis-class request, keep it
        // local regardless of capabilities — Fast/Medium slots
        // are latency-critical (router, compression, title gen)
        // and peer round-trip costs dominate the inference time.
        if request.preferred_speed != Speed::Slow {
            return None;
        }

        // Local is always a candidate. `None` means no loaded
        // model's claims can serve the request — any peer that CAN
        // then wins automatically. After claim-scoring, fold in
        // v0.3 §7 operational adjustments so a hot local slot can
        // lose to an idle peer on load, and a reliable peer can
        // beat a failure-prone local.
        let req_oicp = request.oicp.as_ref()?;
        // v0.3 §4.3 governance tap: record the requested hint if
        // it's an `x:*` extension. Standardized hints are skipped
        // inside the registry itself. No routing impact — this is
        // purely a passive observer.
        if let Some(hint) = req_oicp.capability_hint.as_ref() {
            self.extension_registry
                .write()
                .await
                .observe_request(hint, Self::now_unix_secs());
        }
        let local_obs = self.local_observations.read().await.clone();
        let local_bench = self.local_benchmark.read().await.clone();
        let local_cand = score_manifest_for_request(&self.self_manifest, req_oicp)
            .map(|c| {
                adjust_for_observations(
                    c,
                    &local_obs,
                    NodeLocality::Local,
                    local_bench.as_ref(),
                )
            });
        tracing::info!(
            local_models = self.self_manifest.models.len(),
            local_scores = local_cand.is_some(),
            local_score = local_cand.as_ref().map(|c| c.score).unwrap_or(f32::NEG_INFINITY),
            local_pick = local_cand.as_ref().map(|c| c.model_id.as_str()).unwrap_or("<none>"),
            local_size_gb = ?local_cand.as_ref().and_then(|c| c.size_gb),
            req_hint = %req_oicp.effective_hint(),
            req_latency = ?req_oicp.effective_latency_class(),
            "mesh-inference: scoring local"
        );

        let peers = self.mesh.peer_inference_endpoints().await;
        let peer_obs_snapshot = self.peer_observations.read().await.clone();
        let mut best_peer: Option<(PeerInferenceEndpoint, ModelCandidate)> = None;
        for peer in peers {
            // Drop quarantined peers from the candidate set. They'll
            // re-enter automatically once their cooldown expires.
            // Returning None from select_peer (when this is the only
            // candidate that would have won) falls back to local —
            // which is the correct behaviour for the OICP path
            // because OICP-driven calls don't pin a specific model.
            if self.peer_health.is_quarantined(&peer.name) {
                tracing::debug!(
                    peer = %peer.name,
                    "mesh-inference: skipping quarantined peer in scoring"
                );
                continue;
            }
            let (manifest, rtt_ms) = match self.get_peer_manifest(&peer).await {
                Some(m) => m,
                None => continue,
            };
            let raw = match score_manifest_for_request(&manifest, req_oicp) {
                Some(c) => c,
                None => continue,
            };
            // Apply operational adjustments. Locality is derived
            // from the manifest-fetch RTT (see PR-F) — same round
            // trip, no extra probe — so LAN deployments actually
            // see their locality bonus instead of every peer
            // defaulting to `Far`.
            let obs = peer_obs_snapshot
                .get(&peer.name)
                .cloned()
                .unwrap_or_default();
            let cand = adjust_for_observations(
                raw,
                &obs,
                classify_rtt_ms(rtt_ms),
                peer.benchmark.as_ref(),
            );
            tracing::info!(
                peer = %peer.name,
                peer_pick = %cand.model_id,
                peer_score = cand.score,
                peer_size_gb = ?cand.size_gb,
                "mesh-inference: scored peer"
            );
            best_peer = Some(match best_peer.take() {
                None => (peer, cand),
                Some((cur_peer, cur_cand)) => {
                    // pick_better returns the winner, but we also
                    // need to know which peer owned it — so compare
                    // model_ids post-hoc. Model ids are unique per
                    // peer (each manifest's best model), and across
                    // peers they might collide (two nodes both
                    // running Qwen3.5-9B). In a collision the
                    // incumbent wins by stable ordering — acceptable
                    // since either serves equally well.
                    let winner = pick_better(cur_cand.clone(), cand.clone());
                    if winner.model_id == cand.model_id
                        && winner.score == cand.score
                        && winner.size_gb == cand.size_gb
                        && winner.model_id != cur_cand.model_id
                    {
                        (peer, cand)
                    } else {
                        (cur_peer, cur_cand)
                    }
                }
            });
        }

        match best_peer {
            // Only cross the network when a peer is STRICTLY
            // better than local on (score, then size). `pick_better`
            // encodes the tie-break policy; if local's candidate
            // isn't strictly beaten by peer's, we stay home. Local
            // wins ties — no round-trip cost, no attribution churn.
            Some((peer, peer_cand)) => {
                let local_for_cmp = local_cand.clone().unwrap_or(ModelCandidate {
                    score: f32::NEG_INFINITY,
                    size_gb: None,
                    model_id: "<local-insufficient>".into(),
                    claim_affinity: 0.0,
                });
                let winner = pick_better(local_for_cmp.clone(), peer_cand.clone());
                // "Peer strictly wins" iff pick_better returned the
                // peer candidate AND the local candidate is not
                // identical to it (handles the `None` local case
                // cleanly too).
                let peer_wins = winner.model_id == peer_cand.model_id
                    && winner.score == peer_cand.score
                    && winner.size_gb == peer_cand.size_gb
                    && !candidates_equal(&local_for_cmp, &peer_cand);
                if peer_wins {
                    tracing::info!(
                        peer = %peer.name,
                        peer_pick = %peer_cand.model_id,
                        peer_score = peer_cand.score,
                        peer_size_gb = ?peer_cand.size_gb,
                        local_pick = %local_for_cmp.model_id,
                        local_score = local_for_cmp.score,
                        local_size_gb = ?local_for_cmp.size_gb,
                        "mesh-inference: peer selected by OICP (score, then size_gb)"
                    );
                    Some((peer, peer_cand))
                } else {
                    tracing::debug!(
                        local_pick = %local_for_cmp.model_id,
                        local_score = local_for_cmp.score,
                        "mesh-inference: local wins on OICP (score, then size_gb)"
                    );
                    None
                }
            }
            None => {
                tracing::debug!(
                    local_pick = local_cand.as_ref().map(|c| c.model_id.as_str()).unwrap_or("<none>"),
                    "mesh-inference: no peer manifests scored, staying local"
                );
                None
            }
        }
    }

    /// Stamp the response's `model_id` with a peer-attribution
    /// suffix so `ResponseProvenance.inference_backend` reads
    /// e.g. `Qwen3.5-9B.Q8_0 @ peer BeefyMac`.
    fn annotate(mut resp: CompletionResponse, peer_name: &str) -> CompletionResponse {
        resp.model_id = format!("{} @ peer {}", resp.model_id, peer_name);
        resp
    }

    /// Resolve `request.model_id` to a concrete location.
    ///
    /// Contract: when a caller names a specific model the daemon
    /// MUST honour that name — silent substitution to a different
    /// model is forbidden. But when multiple nodes in the mesh
    /// advertise the same id (e.g., laptop + Mac both have
    /// Darwin-36B), the choice between them is a *load-balancing*
    /// decision, not a name-resolution decision: we pick the node
    /// with the lowest current in-flight count for that model.
    ///
    /// Selection rule:
    /// 1. Collect every reachable, non-quarantined candidate that
    ///    advertises the id, plus `self` if it does.
    /// 2. Score each by in-flight count for this model (per-model
    ///    for local; per-peer for peers — peer observations aren't
    ///    broken down by model, but they still reflect this
    ///    scheduler's outstanding requests to that peer, which is
    ///    the relevant queue depth from our POV).
    /// 3. Pick the minimum. Break ties in favour of local (no
    ///    round-trip, no attribution churn).
    ///
    /// Returns:
    /// - `Local`: serve via `self.local`. The local provider's slot
    ///   picker knows how to route by name into the matching slot.
    /// - `Peer(peer, candidate)`: route there over HTTP.
    /// - `Unknown`: no node in the mesh advertises the id. Caller
    ///   surfaces this as a clear error rather than falling back to
    ///   a different model.
    async fn locate_named_model(&self, model_id: &str) -> NamedModelLocation {
        let local_has = self
            .self_manifest
            .models
            .iter()
            .any(|m| m.id == model_id);

        // First pass — honour the manifest cache. Normal traffic
        // gets the cheap path (no extra round-trips).
        let mut peer_candidates = self.gather_peer_candidates(model_id, false).await;

        // Cache-recovery retry: if the first pass would have produced
        // `Unknown` (no peer advertises it, local doesn't have it),
        // the cache may be holding a stale snapshot from a peer that
        // was mid-slot-restart when we last fetched. Re-probe every
        // peer with the cache bypassed before giving up — a model
        // that came back online between cache fetches should be
        // visible immediately, not 60 s from now.
        //
        // Empirical anchor (2026-05-11): the founder cached an empty
        // model list from the Vast pod during its slot-reload window;
        // for 60 s afterwards every chat completion for the Q4 model
        // returned `Model not loaded` even though the pod was back
        // up and serving. Required `daemon restart` to recover.
        if !local_has && peer_candidates.is_empty() {
            let fresh = self.gather_peer_candidates(model_id, true).await;
            if !fresh.is_empty() {
                tracing::info!(
                    model = %model_id,
                    peers = fresh.len(),
                    "mesh-inference: cache-refresh retry recovered peers for \
                     previously-unknown model"
                );
            }
            peer_candidates = fresh;
        }

        if !local_has && peer_candidates.is_empty() {
            return NamedModelLocation::Unknown;
        }

        // Pick the minimum in-flight peer (if any). Cheap O(n) since
        // peer fanout is small (≤ tens of nodes in practice).
        let best_peer = peer_candidates
            .into_iter()
            .min_by_key(|(_, _, inflight)| *inflight);

        match (local_has, best_peer) {
            (false, Some((peer, cand, _))) => NamedModelLocation::Peer(peer, cand),
            (true, None) => NamedModelLocation::Local,
            (true, Some((peer, cand, peer_inflight))) => {
                let local_inflight = self
                    .local_inflight_by_model
                    .lock()
                    .expect("local_inflight_by_model poisoned")
                    .get(model_id)
                    .copied()
                    .unwrap_or(0);
                // Tie → local. Strictly less → local. Otherwise peer.
                if local_inflight <= peer_inflight {
                    tracing::debug!(
                        model = %model_id,
                        local_inflight,
                        peer = %peer.name,
                        peer_inflight,
                        "mesh-inference: local wins load-balance for explicit model"
                    );
                    NamedModelLocation::Local
                } else {
                    tracing::info!(
                        model = %model_id,
                        local_inflight,
                        peer = %peer.name,
                        peer_inflight,
                        "mesh-inference: peer wins load-balance for explicit model"
                    );
                    NamedModelLocation::Peer(peer, cand)
                }
            }
            (false, None) => unreachable!(),
        }
    }

    /// Enumerate peers, filter for reachable + non-quarantined,
    /// fetch each manifest, and emit candidates for those whose
    /// manifest contains `model_id`. The triple holds the peer,
    /// the candidate's affinity metadata, and the health-adjusted
    /// effective in-flight count used for load-balancing.
    ///
    /// `bypass_cache` controls whether `get_peer_manifest` is
    /// allowed to return cached entries. False = normal behaviour
    /// (cheap, honours `MANIFEST_TTL`); true = forced fresh fetch
    /// from each peer (used by the `locate_named_model` retry
    /// path on the otherwise-Unknown verdict).
    async fn gather_peer_candidates(
        &self,
        model_id: &str,
        bypass_cache: bool,
    ) -> Vec<(PeerInferenceEndpoint, ModelCandidate, u32)> {
        let peers = self.mesh.peer_inference_endpoints().await;
        let mut peer_candidates: Vec<(PeerInferenceEndpoint, ModelCandidate, u32)> =
            Vec::with_capacity(peers.len());
        for peer in peers {
            if self.peer_health.is_quarantined(&peer.name) {
                tracing::debug!(
                    peer = %peer.name,
                    model = %model_id,
                    "mesh-inference: skipping quarantined peer for explicit model"
                );
                continue;
            }
            let fetch = if bypass_cache {
                self.get_peer_manifest_fresh(&peer).await
            } else {
                self.get_peer_manifest(&peer).await
            };
            let (manifest, _rtt) = match fetch {
                Some(m) => m,
                None => continue,
            };
            if let Some(model) = manifest.models.iter().find(|m| m.id == model_id) {
                let claim_affinity = model
                    .claims
                    .first()
                    .map(|c| c.effective_affinity())
                    .unwrap_or(0.0);
                let peer_inflight = self
                    .peer_observations
                    .read()
                    .await
                    .get(&peer.name)
                    .map(|o| o.in_flight)
                    .unwrap_or(0);
                let health = self.peer_health.health_weight(&peer.name);
                let effective = effective_inflight(peer_inflight, health);
                peer_candidates.push((
                    peer,
                    ModelCandidate {
                        score: 0.0,
                        size_gb: model.size_gb,
                        model_id: model_id.to_string(),
                        claim_affinity,
                    },
                    effective,
                ));
            }
        }
        peer_candidates
    }

    /// Increment local in-flight counter for `model_id` and return a
    /// drop-guard that decrements on Drop. Pairs the inc/dec via the
    /// guard so any early return path — `?` on the local call, stream
    /// drop, panic — releases the slot in the counter. Caller MUST
    /// hold the guard across the whole local dispatch (the `.await`
    /// for `complete()` or the lifetime of the wrapped stream for
    /// `complete_stream()`).
    fn enter_local_inflight(
        &self,
        model_id: &str,
    ) -> LocalInflightGuard {
        let mut map = self
            .local_inflight_by_model
            .lock()
            .expect("local_inflight_by_model poisoned");
        *map.entry(model_id.to_string()).or_insert(0) += 1;
        LocalInflightGuard {
            counter: Arc::clone(&self.local_inflight_by_model),
            model_id: model_id.to_string(),
        }
    }
}

/// RAII guard for the per-model local in-flight counter. Decrements
/// the counter in `Drop`; safe to drop after the entry has been
/// pruned to zero (saturating subtract + no-op when absent).
struct LocalInflightGuard {
    counter: Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
    model_id: String,
}

impl Drop for LocalInflightGuard {
    fn drop(&mut self) {
        let Ok(mut map) = self.counter.lock() else { return };
        if let Some(v) = map.get_mut(&self.model_id) {
            *v = v.saturating_sub(1);
            if *v == 0 {
                map.remove(&self.model_id);
            }
        }
    }
}

/// Where an explicitly-named `request.model_id` lives in the mesh.
/// Returned by [`MeshInferenceProvider::locate_named_model`]; see
/// that method for the contract this enum encodes.
enum NamedModelLocation {
    /// Our own `self_manifest` advertises this model id. The local
    /// provider's slot picker will route the request into the
    /// matching slot — no further metadata needed at this layer.
    Local,
    /// A peer's manifest advertises this model id.
    Peer(PeerInferenceEndpoint, ModelCandidate),
    /// Nobody in the mesh advertises it.
    Unknown,
}

/// Trim and reject empty `request.model_id`. Empty/whitespace
/// strings are not a routing signal — they fall through to the
/// OICP-driven path.
fn explicit_model_id(request: &CompletionRequest) -> Option<&str> {
    request
        .model_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[async_trait]
impl InferenceProvider for MeshInferenceProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        // Priority: when the caller names a specific model_id, that
        // name is the routing signal — even when the request has no
        // OICP envelope and no Speed::Slow signal. Silent
        // substitution to the local primary slot was the bug here;
        // an explicit name must either be served by the node that
        // advertises it or fail loudly so the caller can react.
        if let Some(model_id) = explicit_model_id(request) {
            match self.locate_named_model(model_id).await {
                NamedModelLocation::Local => {
                    tracing::info!(
                        model = %model_id,
                        "mesh-inference: serving complete() locally by explicit model name"
                    );
                    let _guard = self.enter_local_inflight(model_id);
                    return self.local.complete(request).await;
                }
                NamedModelLocation::Peer(peer, peer_cand) => {
                    tracing::info!(
                        peer = %peer.name,
                        addrs = peer.base_urls.len(),
                        model = %peer_cand.model_id,
                        "mesh-inference: routing complete() to peer by explicit model name"
                    );
                    // Bump the peer's observed in-flight count BEFORE
                    // we hand off. Two reasons:
                    //   1. `locate_named_model`'s load-balance rule
                    //      reads `peer_observations[name].in_flight`
                    //      to decide whether to route subsequent
                    //      concurrent requests here. Without this
                    //      increment, every concurrent caller sees
                    //      `peer_inflight=0` and floods one peer.
                    //   2. The matching `record_success` /
                    //      `record_failure` below decrement it, so the
                    //      count tracks reality without separate
                    //      bookkeeping.
                    self.record_dispatch(Some(&peer.name)).await;
                    let mut last_transport_err: Option<String> = None;
                    for url in &peer.base_urls {
                        let rp = RemoteApiProvider::new(url, None, "mesh-peer", 32_768);
                        match rp.complete(request).await {
                            Ok(mut resp) => {
                                resp.model_id = peer_cand.model_id.clone();
                                self.peer_health.record_success(&peer.name);
                                self.record_success(Some(&peer.name)).await;
                                return Ok(Self::annotate(resp, &peer.name));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    peer = %peer.name,
                                    url = %url,
                                    error = %e,
                                    "mesh-inference: peer complete() transport error \
                                     under explicit model_id, trying next address"
                                );
                                last_transport_err = Some(format!("{e}"));
                            }
                        }
                    }
                    // All addresses for this peer failed. Record one
                    // failure (not one per address — a peer is
                    // unreachable as a unit) and surface a routing
                    // error. The next request for the same model
                    // will see the peer drop from `locate_named_model`
                    // once the threshold is crossed, returning
                    // `Unknown` and failing fast instead of waiting
                    // through another full address round.
                    self.peer_health.record_failure(&peer.name);
                    self.record_failure(Some(&peer.name)).await;
                    return Err(sovereign_core::error::Error::Routing(format!(
                        "model '{}' is advertised by peer '{}' but all peer \
                         addresses failed: {}",
                        model_id,
                        peer.name,
                        last_transport_err.unwrap_or_else(|| "unreachable".into())
                    )));
                }
                NamedModelLocation::Unknown => {
                    return Err(sovereign_core::error::Error::ModelNotLoaded(format!(
                        "no node in this mesh advertises model '{}' — \
                         check `/v1/models` for available names",
                        model_id
                    )));
                }
            }
        }

        if let Some((peer, peer_cand)) = self.select_peer(request).await {
            tracing::info!(
                peer = %peer.name,
                addrs = peer.base_urls.len(),
                peer_pick = %peer_cand.model_id,
                "mesh-inference: routing complete() to peer"
            );
            for url in &peer.base_urls {
                let rp = RemoteApiProvider::new(url, None, "mesh-peer", 32_768);
                match rp.complete(request).await {
                    Ok(mut resp) => {
                        // Prefer the peer's OICP-advertised model
                        // id over whatever label the remote wire
                        // response carried — the advertised id is
                        // what the selector actually scored, so the
                        // attribution should match. (On some
                        // backends the wire response echoes a
                        // request hint instead of the served model.)
                        resp.model_id = peer_cand.model_id.clone();
                        self.peer_health.record_success(&peer.name);
                        return Ok(Self::annotate(resp, &peer.name));
                    }
                    Err(e) => {
                        tracing::info!(
                            peer = %peer.name,
                            url = %url,
                            error = %e,
                            "mesh-inference: peer complete() transport error, trying next address"
                        );
                    }
                }
            }
            // All addresses failed for this peer — record one
            // failure (not one per address) before falling back.
            self.peer_health.record_failure(&peer.name);
            tracing::info!(
                peer = %peer.name,
                "mesh-inference: all peer addresses failed, falling back to local"
            );
        }
        self.local.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        // Kept for trait object compatibility; the richer
        // `complete_stream_with_id` is what the runtime actually
        // calls and what carries the peer attribution back out.
        // Delegate here so any caller using the legacy shape still
        // gets the same routing behaviour, just without the
        // attribution string.
        Ok(self.complete_stream_with_id(request).await?.0)
    }

    /// Streaming + attribution in one call. Chosen over stashing
    /// routing state on the provider (Mutex<Option<String>>)
    /// because multiple in-flight streams share one
    /// `MeshInferenceProvider`; stashing would race. Returning the
    /// attribution alongside the stream is the only way to bind
    /// "this stream came from peer X" to "this stream" without a
    /// per-request handle. Mirrors the non-streaming `complete()`
    /// path's `annotate()` behaviour.
    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        // Mirror the non-streaming priority: an explicit model_id
        // wins over OICP-driven selection so a request for a named
        // peer-only model gets to that peer instead of falling back
        // to the local primary.
        if let Some(model_id) = explicit_model_id(request) {
            match self.locate_named_model(model_id).await {
                NamedModelLocation::Local => {
                    tracing::info!(
                        model = %model_id,
                        "mesh-inference: serving complete_stream() locally by explicit model name"
                    );
                    let guard = self.enter_local_inflight(model_id);
                    let stream = self.local.complete_stream(request).await?;
                    let observed: Pin<
                        Box<dyn Stream<Item = Result<String>> + Send>,
                    > = Box::pin(InflightGuardedStream::new(
                        ThroughputObservedStream::new(
                            stream,
                            ThroughputTarget::Local(Arc::clone(&self.local_observations)),
                        ),
                        guard,
                    ));
                    return Ok((observed, model_id.to_string()));
                }
                NamedModelLocation::Peer(peer, peer_cand) => {
                    tracing::info!(
                        peer = %peer.name,
                        addrs = peer.base_urls.len(),
                        model = %peer_cand.model_id,
                        "mesh-inference: routing complete_stream() to peer by explicit model name"
                    );
                    let ledger_emission = self
                        .mesh
                        .ledger_emission_for(
                            &peer.node_id,
                            &peer_cand.model_id,
                            &peer.name,
                        )
                        .await;
                    let mut last_transport_err: Option<String> = None;
                    for url in &peer.base_urls {
                        let rp = RemoteApiProvider::new(url, None, "mesh-peer", 32_768);
                        match rp.complete_stream(request).await {
                            Ok(stream) => {
                                let attribution =
                                    format!("{} @ peer {}", peer_cand.model_id, peer.name);
                                let mut wrapper = ThroughputObservedStream::new(
                                    stream,
                                    ThroughputTarget::Peer {
                                        name: peer.name.clone(),
                                        map: Arc::clone(&self.peer_observations),
                                    },
                                );
                                if let Some(em) = ledger_emission.clone() {
                                    wrapper = wrapper.with_ledger_emission(em);
                                }
                                let observed: Pin<
                                    Box<dyn Stream<Item = Result<String>> + Send>,
                                > = Box::pin(wrapper);
                                self.peer_health.record_success(&peer.name);
                                return Ok((observed, attribution));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    peer = %peer.name,
                                    url = %url,
                                    error = %e,
                                    "mesh-inference: peer complete_stream() transport \
                                     error under explicit model_id, trying next address"
                                );
                                last_transport_err = Some(format!("{e}"));
                            }
                        }
                    }
                    self.peer_health.record_failure(&peer.name);
                    return Err(sovereign_core::error::Error::Routing(format!(
                        "model '{}' is advertised by peer '{}' but all peer \
                         addresses failed: {}",
                        model_id,
                        peer.name,
                        last_transport_err.unwrap_or_else(|| "unreachable".into())
                    )));
                }
                NamedModelLocation::Unknown => {
                    return Err(sovereign_core::error::Error::ModelNotLoaded(format!(
                        "no node in this mesh advertises model '{}' — \
                         check `/v1/models` for available names",
                        model_id
                    )));
                }
            }
        }

        if let Some((peer, peer_cand)) = self.select_peer(request).await {
            tracing::info!(
                peer = %peer.name,
                addrs = peer.base_urls.len(),
                peer_pick = %peer_cand.model_id,
                "mesh-inference: routing complete_stream() to peer"
            );
            // Resolve the local-side contribution emitter once per
            // dispatch — the embedded daemon's AppState owns it,
            // and we attach it to the stream wrapper so the Drop
            // impl can fire `InferenceReceived` on completion.
            // Falls through to None when the daemon hasn't joined
            // a mesh yet; emission silently skips in that case.
            let ledger_emission = self
                .mesh
                .ledger_emission_for(&peer.node_id, &peer_cand.model_id, &peer.name)
                .await;
            for url in &peer.base_urls {
                let rp = RemoteApiProvider::new(url, None, "mesh-peer", 32_768);
                match rp.complete_stream(request).await {
                    Ok(stream) => {
                        let model_id =
                            format!("{} @ peer {}", peer_cand.model_id, peer.name);
                        let mut wrapper = ThroughputObservedStream::new(
                            stream,
                            ThroughputTarget::Peer {
                                name: peer.name.clone(),
                                map: Arc::clone(&self.peer_observations),
                            },
                        );
                        if let Some(em) = ledger_emission.clone() {
                            wrapper = wrapper.with_ledger_emission(em);
                        }
                        let observed: Pin<
                            Box<dyn Stream<Item = Result<String>> + Send>,
                        > = Box::pin(wrapper);
                        self.peer_health.record_success(&peer.name);
                        return Ok((observed, model_id));
                    }
                    Err(e) => {
                        tracing::info!(
                            peer = %peer.name,
                            url = %url,
                            error = %e,
                            "mesh-inference: peer complete_stream() transport error, trying next address"
                        );
                    }
                }
            }
            self.peer_health.record_failure(&peer.name);
            tracing::info!(
                peer = %peer.name,
                "mesh-inference: all peer addresses failed, falling back to local"
            );
        }
        let stream = self.local.complete_stream(request).await?;
        let observed: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
            Box::pin(ThroughputObservedStream::new(
                stream,
                ThroughputTarget::Local(Arc::clone(&self.local_observations)),
            ));
        Ok((observed, self.local.model_id_for(request.preferred_speed)))
    }

    async fn warmup_primary(&self) -> Result<()> {
        // Warm only the local primary slot. We deliberately don't
        // poke peers — that would tie up their lazy mutex during a
        // user's typing window, and the desktop already covers
        // the local case (which is what the user will hit by default
        // when the OICP scorer isn't strictly outscored by a peer).
        self.local.warmup_primary().await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.local.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.local.embed_batch(texts).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.local.embed_query(text).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.local.model_id_for(speed)
    }

    fn code_model_id(&self) -> Option<String> {
        // Delegate so the mesh-level self-advertisement sees the
        // same code slot the underlying `EmbeddedLlamaCpp` sees.
        self.local.code_model_id()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.local.capabilities()
    }
}

/// Where a `ThroughputObservedStream` should write its measurements
/// when it terminates: either onto the local-side single
/// `NodeObservations` slot, or onto the per-peer map keyed by name.
/// Mirrors the dual storage already present on
/// [`MeshInferenceProvider::peer_observations`] /
/// [`MeshInferenceProvider::local_observations`].
#[derive(Clone)]
enum ThroughputTarget {
    Local(Arc<RwLock<NodeObservations>>),
    Peer {
        name: String,
        map: Arc<RwLock<HashMap<String, NodeObservations>>>,
    },
}

/// Stream wrapper that records TTFT (time-to-first-token) and
/// observed token-generation rate when the stream completes. Both
/// metrics fold into the per-(local|peer) [`NodeObservations`] EWMA
/// so [`oicp::throughput_factor`] sees real performance, not just
/// the advertised benchmark.
///
/// Implementation notes:
///
/// - Token count is approximated as **stream chunks**. SSE-streamed
///   output from llama.cpp emits one chunk per token in practice.
///   This is a coarse proxy for routing — the absolute number may
///   be off, but the relative ordering across peers is preserved
///   (every peer is measured the same way).
/// - We record on `Drop` so that streams aborted mid-completion
///   still surface their TTFT — abort timing is a useful signal
///   too. A stream that ended with zero chunks contributes only
///   the TTFT data.
/// - Recording is `tokio::spawn`'d because `Drop` runs in a
///   non-async context. The spawned task uses the same EWMA α as
///   the latency probe to stay consistent with the rest of the
///   observation pipeline.
/// Optional ledger-event emission attached to the stream wrapper.
/// When `Some`, the stream's `Drop` impl fires an
/// `InferenceReceived` event on completion with `tokens_generated`
/// equal to the chunk count. Local-served streams set this to
/// `None` — the dimensional ledger is intra-mesh-only per spec
/// §10, and a "received from self" event is meaningless.
///
/// `pub` because the [`PeerEndpointSource`] trait method
/// `ledger_emission_for` returns `Option<LedgerEmission>` —
/// implementors outside this module need to construct values of
/// this type. Fields stay `pub(crate)` so the construction shape
/// is still controlled.
#[derive(Clone)]
pub struct LedgerEmission {
    pub(crate) from_node: commonwealth_core::ids::NodeId,
    pub(crate) model_id: String,
    pub(crate) emitter: commonwealth_state::ContributionEmitter,
}

/// Stream wrapper that holds a [`LocalInflightGuard`] for its
/// lifetime. The guard's `Drop` decrements the per-model in-flight
/// counter when the consumer drops the stream — full consumption,
/// early cancel, or panic all release the slot correctly. Generic
/// over the inner stream type so it doesn't force an extra box.
struct InflightGuardedStream<S> {
    inner: S,
    _guard: LocalInflightGuard,
}

impl<S> InflightGuardedStream<S> {
    fn new(inner: S, guard: LocalInflightGuard) -> Self {
        Self { inner, _guard: guard }
    }
}

impl<S> Stream for InflightGuardedStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

struct ThroughputObservedStream {
    inner: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
    dispatched_at: Instant,
    first_chunk_at: Option<Instant>,
    chunk_count: u64,
    target: ThroughputTarget,
    completed: bool,
    ledger_emission: Option<LedgerEmission>,
}

impl ThroughputObservedStream {
    fn new(
        inner: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
        target: ThroughputTarget,
    ) -> Self {
        Self {
            inner,
            dispatched_at: Instant::now(),
            first_chunk_at: None,
            chunk_count: 0,
            target,
            completed: false,
            ledger_emission: None,
        }
    }

    fn with_ledger_emission(mut self, emission: LedgerEmission) -> Self {
        self.ledger_emission = Some(emission);
        self
    }
}

impl Stream for ThroughputObservedStream {
    type Item = Result<String>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                if self.first_chunk_at.is_none() {
                    self.first_chunk_at = Some(Instant::now());
                }
                self.chunk_count += 1;
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
                self.completed = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for ThroughputObservedStream {
    fn drop(&mut self) {
        let dispatched = self.dispatched_at;
        let first_chunk = self.first_chunk_at;
        let count = self.chunk_count;
        let target = self.target.clone();
        let emission = self.ledger_emission.clone();

        // Skip recording if no first token ever arrived AND no
        // chunks were yielded. Pure-failure case — nothing to
        // measure, and the failure tracker handles it via
        // `record_failure`.
        if first_chunk.is_none() && count == 0 {
            return;
        }

        tokio::spawn(async move {
            let now = Instant::now();
            let ttft_ms = first_chunk
                .map(|t| t.duration_since(dispatched).as_secs_f64() * 1000.0);
            let tg_tok_s = first_chunk.and_then(|fc| {
                let gen_secs = now.duration_since(fc).as_secs_f64();
                if gen_secs > 0.0 && count > 0 {
                    Some(count as f64 / gen_secs)
                } else {
                    None
                }
            });

            match target {
                ThroughputTarget::Local(obs) => {
                    let mut o = obs.write().await;
                    apply_throughput_observation(
                        &mut o, ttft_ms, tg_tok_s,
                    );
                }
                ThroughputTarget::Peer { name, map } => {
                    let mut m = map.write().await;
                    let entry =
                        m.entry(name).or_insert_with(NodeObservations::default);
                    apply_throughput_observation(
                        entry, ttft_ms, tg_tok_s,
                    );
                }
            }

            // Emit `InferenceReceived` on the completion path —
            // peer-routed streams that yielded any chunks count as
            // a received inference. Spec §4.3 docs the symmetric
            // pair (`InferenceServed` on peer, `InferenceReceived`
            // here); the aggregator does NOT cross-pollinate, so
            // we have to emit both halves explicitly.
            if let Some(em) = emission {
                if count > 0 {
                    em.emitter.record(
                        commonwealth_core::contributions::LedgerEventKind::InferenceReceived {
                            from_node: em.from_node,
                            model_id: em.model_id,
                            tokens_generated: count,
                        },
                    );
                }
            }
        });
    }
}

/// EWMA update for the throughput-observation fields on
/// [`NodeObservations`]. α follows
/// [`THROUGHPUT_EWMA_ALPHA`] so this stays in lock-step with the
/// latency probe and other observation paths.
fn apply_throughput_observation(
    obs: &mut NodeObservations,
    ttft_ms: Option<f64>,
    tg_tok_s: Option<f64>,
) {
    let alpha = THROUGHPUT_EWMA_ALPHA;
    if let Some(ttft) = ttft_ms {
        obs.ttft_ewma_ms = if obs.ttft_ewma_ms == 0.0 {
            ttft
        } else {
            alpha * ttft + (1.0 - alpha) * obs.ttft_ewma_ms
        };
    }
    if let Some(tg) = tg_tok_s {
        obs.tg_tok_s_ewma = if obs.tg_tok_s_ewma == 0.0 {
            tg
        } else {
            alpha * tg + (1.0 - alpha) * obs.tg_tok_s_ewma
        };
    }
}

// Selection primitive tests live in `crate::oicp_select` alongside
// the primitives themselves; this file only tests the peer-
// orchestration logic (HTTP manifest fetch, selection loop, etc.)
// once we need targeted coverage for that layer.

#[cfg(test)]
mod throughput_tests {
    use super::*;

    #[test]
    fn ewma_seed_takes_first_value_when_zero() {
        let mut obs = NodeObservations::default();
        apply_throughput_observation(&mut obs, Some(120.0), Some(15.0));
        assert!((obs.ttft_ewma_ms - 120.0).abs() < 1e-9);
        assert!((obs.tg_tok_s_ewma - 15.0).abs() < 1e-9);
    }

    #[test]
    fn ewma_blends_subsequent_samples_at_alpha() {
        let mut obs = NodeObservations::default();
        apply_throughput_observation(&mut obs, Some(100.0), Some(20.0));
        apply_throughput_observation(&mut obs, Some(200.0), Some(10.0));
        // alpha=0.3; 0.3*200 + 0.7*100 = 130
        assert!((obs.ttft_ewma_ms - 130.0).abs() < 1e-9);
        // 0.3*10 + 0.7*20 = 17
        assert!((obs.tg_tok_s_ewma - 17.0).abs() < 1e-9);
    }

    #[test]
    fn ewma_ignores_none_inputs() {
        let mut obs = NodeObservations::default();
        apply_throughput_observation(&mut obs, Some(100.0), None);
        assert_eq!(obs.tg_tok_s_ewma, 0.0);
        apply_throughput_observation(&mut obs, None, Some(15.0));
        assert!((obs.ttft_ewma_ms - 100.0).abs() < 1e-9);
        assert!((obs.tg_tok_s_ewma - 15.0).abs() < 1e-9);
    }
}
