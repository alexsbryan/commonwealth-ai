//! `MeshInferenceProvider` — the Joiner-side wrapper that routes
//! synthesis-slot (`Speed::Slow`) inference to the best available
//! mesh peer, with automatic fallback to local on any remote error.
//!
//! Wrapping design (vs. extending `HybridProvider`): the mesh peer
//! set is dynamic — peers join and leave, addresses rotate — but
//! `HybridProvider` takes a static backend list. Instead of rewiring
//! its guts, we put a thin router in front that asks the live
//! `EmbeddedDaemon` on every request whether any peer is reachable,
//! and synthesises a `RemoteApiProvider` on-demand when one is.
//!
//! Routing rules (v1, intentionally simple):
//!
//! 1. `embed` / `embed_query` — always local. Corpus retrieval is
//!    per-query and latency-critical; peer embeddings would dwarf
//!    the retrieval budget.
//! 2. `request.oicp.sharding == LocalOnly` — always local. This is
//!    the `inner-work` skill path ("privacy = local_only") and the
//!    contract is explicit.
//! 3. `request.preferred_speed == Speed::Slow` — try peer. Slow is
//!    the Primary synthesis slot (DeepQuery / ComplexTask), exactly
//!    what federated inference is for. On peer error, fall back to
//!    local so a flaky peer doesn't tank the whole UX.
//! 4. `Speed::Fast` / `Speed::Medium` — local. Router, compression,
//!    title generation — latency-sensitive and not worth shipping
//!    over the network.
//!
//! Stage 2.1 will layer OICP capability scoring on top of this; for
//! now the "is the peer online?" signal gets us the user-visible
//! win (Joiner's 3B synthesis → Founder's 27B synthesis).
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use sovereign_core::error::Result;
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities, Speed};
use sovereign_inference::remote::RemoteApiProvider;

use crate::daemon::{EmbeddedDaemon, PeerInferenceEndpoint};

/// Wraps a local `InferenceProvider` and, when a peer is reachable
/// and the request is synthesis-slot non-local-only, delegates to
/// a `RemoteApiProvider` pointed at the peer's `:9741`.
pub struct MeshInferenceProvider {
    local: Arc<dyn InferenceProvider>,
    mesh: Arc<EmbeddedDaemon>,
    /// This host's `system_ram_gb`, captured at construction so
    /// `pick_peer_provider` can compare gossiped peer RAM against
    /// ours without re-probing hardware every request. Set via
    /// `commonwealth_discovery::hardware::detect_hardware()` — the
    /// exact source as the capability publisher, so comparisons
    /// are apples-to-apples.
    local_ram_gb: u32,
}

impl MeshInferenceProvider {
    pub fn new(local: Arc<dyn InferenceProvider>, mesh: Arc<EmbeddedDaemon>) -> Self {
        let local_ram_gb =
            commonwealth_discovery::hardware::detect_hardware().system_ram_gb;
        tracing::info!(
            local_ram_gb,
            "mesh-inference: wrapper initialised"
        );
        Self {
            local,
            mesh,
            local_ram_gb,
        }
    }

    fn should_route_remote(&self, request: &CompletionRequest) -> bool {
        if request.preferred_speed != Speed::Slow {
            return false;
        }
        if let Some(oicp) = &request.oicp {
            if oicp.sharding() == ShardingPrivacy::LocalOnly {
                return false;
            }
        }
        true
    }

    /// Try to build a `RemoteApiProvider` for the first reachable
    /// peer. `None` when no peer is online or when the mesh is
    /// stopped. The choice here is naive (first online peer) —
    /// a follow-up pass will rank by OICP score / model size.
    async fn pick_peer_provider(
        &self,
    ) -> Option<(PeerInferenceEndpoint, Arc<RemoteApiProvider>)> {
        let peers = self.mesh.peer_inference_endpoints().await;
        // Only route to peers that are strictly beefier than us by
        // RAM — a Founder (64GB) offloading synthesis to a Joiner
        // (32GB) would be a regression. Pick the single peer with
        // the most RAM above ours; ties broken by first-seen.
        let best = peers
            .into_iter()
            .filter(|p| p.system_ram_gb > self.local_ram_gb)
            .max_by_key(|p| p.system_ram_gb);
        let peer = match best {
            Some(p) => p,
            None => {
                tracing::debug!(
                    local_ram_gb = self.local_ram_gb,
                    "mesh-inference: no peer with more RAM than us — staying local"
                );
                return None;
            }
        };
        if let Some(url) = peer.base_urls.first() {
            // `RemoteApiProvider::new` is infallible; reachability
            // errors surface at request time, not construction.
            // Multi-URL retry on connect failure is a future
            // polish — today we pick the first advertised URL
            // (routable IPs first thanks to link-local filtering).
            let rp = RemoteApiProvider::new(url, None, "mesh-peer", 32_768);
            return Some((peer, Arc::new(rp)));
        }
        None
    }

    /// Stamp the response's `model_id` with a peer-attribution
    /// suffix so `ResponseProvenance.inference_backend` becomes
    /// e.g. `Qwen3.5-27B.Q8_0 @ peer BeefyMac`. `RoutingMeta.svelte`
    /// renders it verbatim.
    fn annotate(mut resp: CompletionResponse, peer_name: &str) -> CompletionResponse {
        resp.model_id = format!("{} @ peer {}", resp.model_id, peer_name);
        resp
    }
}

#[async_trait]
impl InferenceProvider for MeshInferenceProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        if self.should_route_remote(request) {
            if let Some((peer, rp)) = self.pick_peer_provider().await {
                tracing::info!(
                    peer = %peer.name,
                    speed = ?request.preferred_speed,
                    "mesh-inference: routing complete() to peer"
                );
                match rp.complete(request).await {
                    Ok(resp) => return Ok(Self::annotate(resp, &peer.name)),
                    Err(e) => {
                        // Fall back to local on any remote error —
                        // flaky peer shouldn't break UX. Info-level
                        // so you can see WHY we ended up local.
                        tracing::info!(
                            peer = %peer.name,
                            error = %e,
                            "mesh-inference: peer complete() failed, falling back to local"
                        );
                    }
                }
            }
        }
        self.local.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        if self.should_route_remote(request) {
            if let Some((peer, rp)) = self.pick_peer_provider().await {
                tracing::info!(
                    peer = %peer.name,
                    speed = ?request.preferred_speed,
                    "mesh-inference: routing complete_stream() to peer"
                );
                match rp.complete_stream(request).await {
                    Ok(stream) => return Ok(stream),
                    Err(e) => {
                        tracing::info!(
                            peer = %peer.name,
                            error = %e,
                            "mesh-inference: peer complete_stream() failed, falling back to local"
                        );
                    }
                }
            }
        }
        self.local.complete_stream(request).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Embeddings stay local — see module header, rule 1.
        self.local.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.local.embed_batch(texts).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.local.embed_query(text).await
    }

    /// Match the Slow-slot model name to the local provider's so
    /// the Runtime's provenance still shows a meaningful model
    /// name when the request is served locally. When a peer serves
    /// it, `annotate` above overrides this with the peer-attribution
    /// suffix before the response hits the Runtime.
    fn model_id_for(&self, speed: Speed) -> String {
        self.local.model_id_for(speed)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // Honest floor: whatever local can do. Mesh peers may add
        // more reach dynamically but that's a moving target we
        // can't summarise in a sync call.
        self.local.capabilities()
    }
}
