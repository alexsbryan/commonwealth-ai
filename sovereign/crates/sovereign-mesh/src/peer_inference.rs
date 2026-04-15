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
    self, CapabilityProfile, ProviderManifest, ShardingPrivacy,
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

pub struct MeshInferenceProvider {
    local: Arc<dyn InferenceProvider>,
    mesh: Arc<EmbeddedDaemon>,
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
    pub fn new(local: Arc<dyn InferenceProvider>, mesh: Arc<EmbeddedDaemon>) -> Self {
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

    /// Score a manifest's best-fitting model against the request.
    /// `Some(score)` when at least one model satisfies `required`;
    /// `None` when no model in the manifest can serve this request.
    fn score_manifest(
        manifest: &ProviderManifest,
        required: &CapabilityProfile,
        preferred: &CapabilityProfile,
    ) -> Option<f32> {
        let mut best: Option<f32> = None;
        for model in &manifest.models {
            if !oicp::satisfies_required(&model.capabilities, required) {
                continue;
            }
            let score = oicp::score_preferred(&model.capabilities, preferred);
            best = Some(best.map(|b| b.max(score)).unwrap_or(score));
        }
        best
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
    async fn select_peer(
        &self,
        request: &CompletionRequest,
    ) -> Option<PeerInferenceEndpoint> {
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

        // Local is always a candidate. A score of NEG_INFINITY
        // means local can't satisfy `required` (peer must win).
        let local_score =
            Self::score_manifest(&self.self_manifest, &required, &preferred)
                .unwrap_or(f32::NEG_INFINITY);

        let peers = self.mesh.peer_inference_endpoints().await;
        let mut best_peer: Option<(PeerInferenceEndpoint, f32)> = None;
        for peer in peers {
            let manifest = match self.get_peer_manifest(&peer).await {
                Some(m) => m,
                None => continue,
            };
            let score = match Self::score_manifest(&manifest, &required, &preferred) {
                Some(s) => s,
                None => continue,
            };
            match &best_peer {
                None => best_peer = Some((peer, score)),
                Some((_, cur)) if score > *cur => best_peer = Some((peer, score)),
                _ => {}
            }
        }

        match best_peer {
            // Strict > : local wins ties. Only cross the network
            // when a peer is measurably better on preferred caps.
            Some((peer, peer_score)) if peer_score > local_score => {
                tracing::info!(
                    peer = %peer.name,
                    peer_score,
                    local_score,
                    "mesh-inference: peer selected by OICP score"
                );
                Some(peer)
            }
            _ => {
                tracing::debug!(
                    local_score,
                    "mesh-inference: local wins on OICP score"
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
        if let Some(peer) = self.select_peer(request).await {
            tracing::info!(
                peer = %peer.name,
                addrs = peer.base_urls.len(),
                "mesh-inference: routing complete() to peer"
            );
            for url in &peer.base_urls {
                let rp = RemoteApiProvider::new(url, None, "mesh-peer", 32_768);
                match rp.complete(request).await {
                    Ok(resp) => return Ok(Self::annotate(resp, &peer.name)),
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
        if let Some(peer) = self.select_peer(request).await {
            tracing::info!(
                peer = %peer.name,
                addrs = peer.base_urls.len(),
                "mesh-inference: routing complete_stream() to peer"
            );
            for url in &peer.base_urls {
                let rp = RemoteApiProvider::new(url, None, "mesh-peer", 32_768);
                match rp.complete_stream(request).await {
                    Ok(stream) => return Ok(stream),
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
        self.local.complete_stream(request).await
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
