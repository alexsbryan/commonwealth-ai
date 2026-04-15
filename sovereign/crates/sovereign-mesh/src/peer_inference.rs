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
    CapabilityProfile, ProviderManifest, ShardingPrivacy,
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
}

// OICP selection primitives live in `crate::oicp_select` so the
// same scoring + tie-break policy drives both sides of the wire:
// the Joiner picking a peer (here) AND the peer-side adapter
// picking which loaded slot to serve the request from. Importing
// here keeps the rest of this file unchanged.
use crate::oicp_select::{
    candidates_equal, pick_better, score_manifest, ModelCandidate,
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
        Self {
            local,
            mesh,
            self_manifest,
            peer_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
            http,
        }
    }

    /// Pull required + preferred profiles out of the request's
    /// OICP envelope. Returns `None` when either the envelope is
    /// missing or the capability section is empty — both signal
    /// "no contract, stay local".
    fn extract_caps(
        request: &CompletionRequest,
    ) -> Option<(CapabilityProfile, CapabilityProfile)> {
        let oicp = request.oicp.as_ref()?;
        let caps = oicp.capabilities.as_ref()?;
        if caps.required.is_empty() && caps.preferred.is_empty() {
            return None;
        }
        Some((caps.required.clone(), caps.preferred.clone()))
    }

    /// Fetch a peer's OICP manifest, honouring the 60s cache. On
    /// fetch failure, returns `None` — caller treats the peer as
    /// not a candidate this turn (next request retries).
    async fn get_peer_manifest(
        &self,
        peer: &PeerInferenceEndpoint,
    ) -> Option<ProviderManifest> {
        let key = peer.node_id.to_string();
        // Cache hit.
        {
            let cache = self.peer_cache.read().await;
            if let Some(entry) = cache.get(&key) {
                if entry.fetched_at.elapsed() < MANIFEST_TTL {
                    return Some(entry.manifest.clone());
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
            match self.http.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<ProviderManifest>().await {
                        Ok(m) => {
                            // Info-level so the happy path is
                            // visible in the default log filter
                            // alongside the selection decision.
                            // (Went through a 188s-local incident
                            // because a silent manifest fetch
                            // failure looked identical to "no
                            // peer available.")
                            tracing::info!(
                                peer = %peer.name,
                                url = %url,
                                models = m.models.len(),
                                "mesh-inference: fetched peer manifest"
                            );
                            let mut cache = self.peer_cache.write().await;
                            cache.insert(
                                key,
                                CachedManifest {
                                    manifest: m.clone(),
                                    fetched_at: Instant::now(),
                                },
                            );
                            return Some(m);
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
        let (required, preferred) = Self::extract_caps(request)?;
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
        // model satisfies `required` — any peer that CAN satisfy
        // it then wins automatically. For a typical DeepQuery
        // (required={}, preferred={Analysis:3,General:3}) local
        // will produce a real 0..1.0 candidate reflecting its
        // capability profile.
        let local_cand = score_manifest(&self.self_manifest, &required, &preferred);
        tracing::info!(
            local_models = self.self_manifest.models.len(),
            local_satisfies_required = local_cand.is_some(),
            local_score = local_cand.as_ref().map(|c| c.score).unwrap_or(f32::NEG_INFINITY),
            local_pick = local_cand.as_ref().map(|c| c.model_id.as_str()).unwrap_or("<none>"),
            local_size_gb = ?local_cand.as_ref().and_then(|c| c.size_gb),
            required_keys = required.len(),
            preferred_keys = preferred.len(),
            "mesh-inference: scoring local"
        );

        let peers = self.mesh.peer_inference_endpoints().await;
        let mut best_peer: Option<(PeerInferenceEndpoint, ModelCandidate)> = None;
        for peer in peers {
            let manifest = match self.get_peer_manifest(&peer).await {
                Some(m) => m,
                None => continue,
            };
            let cand = match score_manifest(&manifest, &required, &preferred) {
                Some(c) => c,
                None => continue,
            };
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

    fn capabilities(&self) -> ProviderCapabilities {
        self.local.capabilities()
    }
}

// Selection primitive tests live in `crate::oicp_select` alongside
// the primitives themselves; this file only tests the peer-
// orchestration logic (HTTP manifest fetch, selection loop, etc.)
// once we need targeted coverage for that layer.
