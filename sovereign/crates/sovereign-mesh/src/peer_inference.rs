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
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::Stream;
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
}

#[async_trait]
impl PeerEndpointSource for EmbeddedDaemon {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        EmbeddedDaemon::peer_inference_endpoints(self).await
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
        }
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
        let key = peer.node_id.to_string();
        // Cache hit.
        {
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
        for base in &peer.base_urls {
            let root = base
                .trim_end_matches('/')
                .trim_end_matches("/v1")
                .trim_end_matches('/');
            let url = format!("{root}/oicp/v1/capabilities");
            let started = Instant::now();
            match self.http.get(&url).send().await {
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
        let local_cand = score_manifest_for_request(&self.self_manifest, req_oicp)
            .map(|c| adjust_for_observations(c, &local_obs, NodeLocality::Local));
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
            let cand = adjust_for_observations(raw, &obs, classify_rtt_ms(rtt_ms));
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
}

#[async_trait]
impl InferenceProvider for MeshInferenceProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
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
        if let Some((peer, peer_cand)) = self.select_peer(request).await {
            tracing::info!(
                peer = %peer.name,
                addrs = peer.base_urls.len(),
                peer_pick = %peer_cand.model_id,
                "mesh-inference: routing complete_stream() to peer"
            );
            for url in &peer.base_urls {
                let rp = RemoteApiProvider::new(url, None, "mesh-peer", 32_768);
                match rp.complete_stream(request).await {
                    Ok(stream) => {
                        let model_id =
                            format!("{} @ peer {}", peer_cand.model_id, peer.name);
                        return Ok((stream, model_id));
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
            tracing::info!(
                peer = %peer.name,
                "mesh-inference: all peer addresses failed, falling back to local"
            );
        }
        let stream = self.local.complete_stream(request).await?;
        Ok((stream, self.local.model_id_for(request.preferred_speed)))
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

// Selection primitive tests live in `crate::oicp_select` alongside
// the primitives themselves; this file only tests the peer-
// orchestration logic (HTTP manifest fetch, selection loop, etc.)
// once we need targeted coverage for that layer.
