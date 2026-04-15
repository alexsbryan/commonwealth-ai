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

/// A scored model pick from a single manifest. Carried through
/// selection so tie-breaks can see both the OICP score and the
/// model's declared size — and so logs can attribute decisions to
/// a specific model id, not just a numeric score.
#[derive(Debug, Clone)]
struct ModelCandidate {
    score: f32,
    size_gb: Option<f32>,
    model_id: String,
}

/// Score-floor below which score-ties are considered "the same".
/// Floating-point noise in the OICP scorer (division-by-max-level
/// produces 1/3, 2/3, 1.0 type values) shouldn't cause spurious
/// decisions where a 5.5 GB model beats a 16.5 GB model by a
/// rounding blip.
const SCORE_TIE_EPSILON: f32 = 1e-3;

/// Compare two `ModelCandidate`s under the OICP selection policy
/// and return the winner:
///
/// 1. Strictly higher `score` wins.
/// 2. Scores tied (within `SCORE_TIE_EPSILON`): smaller known
///    `size_gb` wins.
/// 3. Known size always beats unknown size on a score tie — an
///    annotated manifest entry represents curated data we trust
///    over a silent BYOM default.
/// 4. Full tie (same score bucket, both sizes unknown or equal):
///    incumbent (`cur`) wins for stability. Caller uses this to
///    encode "local wins ties" and "earlier peer wins duplicate-
///    score ties".
fn pick_better(cur: ModelCandidate, new: ModelCandidate) -> ModelCandidate {
    if new.score > cur.score + SCORE_TIE_EPSILON {
        return new;
    }
    if cur.score > new.score + SCORE_TIE_EPSILON {
        return cur;
    }
    match (cur.size_gb, new.size_gb) {
        (Some(c), Some(n)) if n < c => new,
        (None, Some(_)) => new,
        _ => cur,
    }
}

/// Used to detect "peer pick is identical to local pick" so the
/// peer-wins check doesn't trip a network hop for a zero-delta
/// routing decision (e.g. both sides advertise the same Qwen3.5-9B).
fn candidates_equal(a: &ModelCandidate, b: &ModelCandidate) -> bool {
    (a.score - b.score).abs() <= SCORE_TIE_EPSILON
        && a.size_gb == b.size_gb
        && a.model_id == b.model_id
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
    /// `Some(candidate)` when at least one model satisfies
    /// `required`; `None` when no model in the manifest can serve
    /// this request.
    ///
    /// "Best" has a tiebreaker: among models with the same score,
    /// prefer the one with the smallest declared `size_gb`. This
    /// is the closest proxy we have to "fastest at this capability
    /// level" without a live latency measurement — a 9B satisfying
    /// `{Analysis:3, General:3}` is the right pick over a 27B that
    /// scores identically for the same request. Unknown sizes sort
    /// after any known size so an unannotated BYOM entry can't
    /// sneak past an annotated one on a score tie.
    fn score_manifest(
        manifest: &ProviderManifest,
        required: &CapabilityProfile,
        preferred: &CapabilityProfile,
    ) -> Option<ModelCandidate> {
        let mut best: Option<ModelCandidate> = None;
        for model in &manifest.models {
            if !oicp::satisfies_required(&model.capabilities, required) {
                continue;
            }
            let score = oicp::score_preferred(&model.capabilities, preferred);
            let cand = ModelCandidate {
                score,
                size_gb: model.size_gb,
                model_id: model.id.clone(),
            };
            best = Some(match best {
                None => cand,
                Some(cur) => pick_better(cur, cand),
            });
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

        // Local is always a candidate. `None` means no loaded
        // model satisfies `required` — any peer that CAN satisfy
        // it then wins automatically. For a typical DeepQuery
        // (required={}, preferred={Analysis:3,General:3}) local
        // will produce a real 0..1.0 candidate reflecting its
        // capability profile.
        let local_cand = Self::score_manifest(&self.self_manifest, &required, &preferred);
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
            let cand = match Self::score_manifest(&manifest, &required, &preferred) {
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
                    Some(peer)
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

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::oicp::{
        Capability, CapabilityProfile, ModelStatus, ProviderManifest, ProviderModel,
        OICP_VERSION,
    };

    fn cand(score: f32, size_gb: Option<f32>, id: &str) -> ModelCandidate {
        ModelCandidate { score, size_gb, model_id: id.into() }
    }

    #[test]
    fn pick_better_higher_score_wins() {
        let a = cand(0.5, Some(5.5), "small");
        let b = cand(1.0, Some(16.5), "big");
        assert_eq!(pick_better(a, b).model_id, "big");
    }

    #[test]
    fn pick_better_score_tied_smaller_size_wins() {
        // The whole point of the tiebreaker: two models both score
        // 1.0 against the preferred profile; the smaller one ought
        // to win. This is the Founder-with-9B-and-27B scenario.
        let nine = cand(1.0, Some(5.5), "qwen-9b");
        let twenty_seven = cand(1.0, Some(16.5), "qwen-27b");
        // Incumbent = 27B; new = 9B → 9B wins.
        assert_eq!(pick_better(twenty_seven.clone(), nine.clone()).model_id, "qwen-9b");
        // And the reverse order (incumbent = 9B, new = 27B) keeps 9B.
        assert_eq!(pick_better(nine, twenty_seven).model_id, "qwen-9b");
    }

    #[test]
    fn pick_better_known_size_beats_unknown_on_tie() {
        // Annotated (size known) outranks BYOM (size unknown) when
        // scores tie. Reason: an annotated entry represents curated
        // data we trust; an unannotated one is a null-signal.
        let annotated = cand(1.0, Some(5.5), "annotated");
        let unannotated = cand(1.0, None, "byom");
        assert_eq!(pick_better(unannotated.clone(), annotated.clone()).model_id, "annotated");
        assert_eq!(pick_better(annotated, unannotated).model_id, "annotated");
    }

    #[test]
    fn pick_better_full_tie_keeps_incumbent() {
        // Same score, same size — stability. Used by the caller
        // to encode "local wins ties" and "first peer wins dup ties".
        let a = cand(1.0, Some(5.5), "incumbent");
        let b = cand(1.0, Some(5.5), "challenger");
        assert_eq!(pick_better(a, b).model_id, "incumbent");
    }

    #[test]
    fn pick_better_epsilon_ignores_floating_point_noise() {
        // OICP scores are ratios — 2/3 = 0.6666..., 1.0 = 1.0, etc.
        // A 1e-6 drift between two "identical" scores shouldn't
        // hand the win to the bigger model.
        let nine = cand(1.0, Some(5.5), "qwen-9b");
        let twenty_seven = cand(1.0 - 1e-6, Some(16.5), "qwen-27b");
        assert_eq!(pick_better(twenty_seven, nine).model_id, "qwen-9b");
    }

    fn model(id: &str, caps: &[(Capability, u8)], size_gb: Option<f32>) -> ProviderModel {
        let capabilities: CapabilityProfile = caps.iter().copied().collect();
        ProviderModel {
            id: id.into(),
            base_model: None,
            quantization: None,
            capabilities,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb,
        }
    }

    fn manifest(models: Vec<ProviderModel>) -> ProviderManifest {
        ProviderManifest {
            oicp_version: OICP_VERSION.to_string(),
            provider: None,
            models,
            knowledge: None,
            federation: None,
        }
    }

    #[test]
    fn score_manifest_picks_smaller_model_on_tie() {
        // This is the demo-scenario guard: Founder's manifest
        // advertises both Qwen3.5-9B (5.5 GB, analysis=3, general=3)
        // and Qwen3.5-27B (16.5 GB, analysis=4, general=3). For a
        // DeepQuery with preferred={Analysis:3, General:3}, both
        // models satisfy the profile at score 1.0 — the 9B wins
        // the tiebreaker on size. Previously we'd have picked the
        // 27B because it was advertised alone, wasting ~3× the
        // memory for a request the 9B could serve identically.
        let nine = model(
            "qwen-9b",
            &[
                (Capability::Analysis, 3),
                (Capability::General, 3),
                (Capability::Code, 3),
                (Capability::Instruction, 3),
                (Capability::Math, 2),
            ],
            Some(5.5),
        );
        let twenty_seven = model(
            "qwen-27b",
            &[
                (Capability::Analysis, 4),
                (Capability::General, 3),
                (Capability::Code, 3),
                (Capability::Instruction, 4),
                (Capability::Math, 3),
                (Capability::Creative, 3),
            ],
            Some(16.5),
        );
        let m = manifest(vec![twenty_seven, nine]);
        let preferred: CapabilityProfile =
            [(Capability::Analysis, 3), (Capability::General, 3)]
                .into_iter()
                .collect();
        let required = CapabilityProfile::new();
        let winner = MeshInferenceProvider::score_manifest(&m, &required, &preferred)
            .expect("at least one model satisfies required");
        assert_eq!(winner.model_id, "qwen-9b");
        assert_eq!(winner.size_gb, Some(5.5));
        // And the score should be exactly 1.0 — both models fully
        // satisfy preferred.
        assert!((winner.score - 1.0).abs() < 1e-3);
    }

    #[test]
    fn score_manifest_picks_higher_score_over_smaller_size() {
        // The tiebreaker only kicks in on score ties. If a bigger
        // model strictly outscores a smaller one, the bigger model
        // wins — size is a tiebreaker, not a cost function.
        let small_weak = model(
            "small-weak",
            &[(Capability::Analysis, 2), (Capability::General, 2)],
            Some(2.0),
        );
        let big_strong = model(
            "big-strong",
            &[(Capability::Analysis, 4), (Capability::General, 4)],
            Some(16.5),
        );
        let m = manifest(vec![small_weak, big_strong]);
        let preferred: CapabilityProfile =
            [(Capability::Analysis, 4), (Capability::General, 4)]
                .into_iter()
                .collect();
        let winner =
            MeshInferenceProvider::score_manifest(&m, &CapabilityProfile::new(), &preferred)
                .expect("at least one model scores");
        assert_eq!(winner.model_id, "big-strong");
    }

    #[test]
    fn score_manifest_returns_none_when_required_unmet() {
        let weak = model(
            "weak",
            &[(Capability::Analysis, 1)],
            Some(1.0),
        );
        let m = manifest(vec![weak]);
        let required: CapabilityProfile = [(Capability::Analysis, 3)].into_iter().collect();
        let preferred = CapabilityProfile::new();
        assert!(MeshInferenceProvider::score_manifest(&m, &required, &preferred).is_none());
    }
}
