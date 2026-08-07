// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! (`sovereign_mesh::oicp_synthesis::build_self_manifest`).
//!
//! This wrapper is the point where the request's requirements meet
//! the available manifests and a single best backend is chosen:
//!
//!   1. An envelope saying `sharding == LocalOnly` → local (or a refusal
//!      when this node cannot serve it). `LocalOnly` is the privacy
//!      opt-out (e.g. the `inner-work` skill) and it is also the OICP
//!      default, so an envelope that has not said `mesh_allowed` has not
//!      opted in.
//!
//!      **A request with NO envelope is NOT in this clause.** It has
//!      stated nothing, and it is the thin-client shape — an IDE or any
//!      OpenAI client pinning `model` and carrying no OICP. Forcing it
//!      local would refuse every laptop request for a model only a peer
//!      holds, which is the whole point of the mesh (M6-A). This clause
//!      previously read "No OICP on the request, or `sharding ==
//!      LocalOnly` → local"; that was never what the named path did, and
//!      implementing it literally would have broken the consumer story.
//!      Corrected 2026-08-06 alongside the B2 fix, which is the gate that
//!      makes the surviving half of this clause true on the named path
//!      (`resolve_named_dispatch`) — before it, a `LocalOnly` envelope
//!      was measured being served by a peer.
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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::Stream;
use sovereign_core::error::Result;
use sovereign_core::oicp::{
    ExtensionRegistry, ExtensionStats, NodeObservations, ProviderManifest,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities, Speed};
use sovereign_inference::remote::RemoteApiProvider;
use tokio::sync::RwLock;

use crate::daemon::{EmbeddedDaemon, PeerInferenceEndpoint};
use crate::decision_log::{
    self, DecisionBuilder, DecisionPath, DecisionSink, OutcomeContext, RequestFacts, ServedBy,
    Verdict,
};
use crate::oicp_synthesis::build_self_manifest;
use crate::scheduler_core::{
    self, LocalCandidateView, PeerCandidateView, PeerManifestView, RankInputs, RankObjective,
    RankResult,
};
use crate::throughput_tracking::{LedgerEmission, ThroughputObservedStream, ThroughputTarget};
use crate::tier::TierFloor;

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
/// Operator override that forces every inference call to stay
/// on the local node, bypassing the load-balance scoring that
/// might otherwise route to a peer. Read from the env on every
/// call so flips take effect without a daemon restart (set the
/// var, flip it back to "0" / unset to resume normal routing).
fn peer_inference_disabled() -> bool {
    std::env::var("SOVEREIGN_DISABLE_PEER_INFERENCE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

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

#[cfg(test)]
mod peer_inference_disabled_tests {
    use super::peer_inference_disabled;

    // Run env-var tests serially via a process-wide mutex —
    // std::env::set_var is not thread-safe and Cargo runs unit
    // tests in parallel by default.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn unset_means_routing_enabled() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("SOVEREIGN_DISABLE_PEER_INFERENCE");
        assert!(!peer_inference_disabled());
    }

    #[test]
    fn value_one_disables_routing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SOVEREIGN_DISABLE_PEER_INFERENCE", "1");
        assert!(peer_inference_disabled());
        std::env::remove_var("SOVEREIGN_DISABLE_PEER_INFERENCE");
    }

    #[test]
    fn value_true_case_insensitive_disables_routing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SOVEREIGN_DISABLE_PEER_INFERENCE", "TRUE");
        assert!(peer_inference_disabled());
        std::env::set_var("SOVEREIGN_DISABLE_PEER_INFERENCE", "true");
        assert!(peer_inference_disabled());
        std::env::remove_var("SOVEREIGN_DISABLE_PEER_INFERENCE");
    }

    #[test]
    fn value_zero_or_other_keeps_routing_enabled() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SOVEREIGN_DISABLE_PEER_INFERENCE", "0");
        assert!(!peer_inference_disabled());
        std::env::set_var("SOVEREIGN_DISABLE_PEER_INFERENCE", "no");
        assert!(!peer_inference_disabled());
        std::env::remove_var("SOVEREIGN_DISABLE_PEER_INFERENCE");
    }
}

/// Per-peer HTTP timeout for the manifest fetch. Short enough that
/// an unreachable peer doesn't add meaningful latency to the
/// selection path; long enough that a Tailscale relay round-trip
/// under load completes comfortably.
const MANIFEST_FETCH_TIMEOUT: Duration = Duration::from_millis(800);

/// A manifest read, plus the provenance P2 of
/// `docs/specs/SCHEDULER_QUALITY.md` requires: how old the copy the
/// scorer read actually was, and whether it came out of the 60s cache
/// or off the wire.
///
/// The claims a candidate is scored on are only as current as the
/// manifest they were read from. A cached manifest up to
/// [`MANIFEST_TTL`] old is a second, independent staleness channel
/// alongside gossip lag (F1), and one that no log has ever recorded —
/// so "the peer advertised that model" and "the peer advertised that
/// model a minute ago" have been indistinguishable in hindsight.
struct ManifestRead {
    manifest: ProviderManifest,
    rtt_ms: u32,
    /// Seconds since the manifest was fetched. `0` for a live fetch.
    age_secs: u64,
    from_cache: bool,
}


/// The result of a ranked OICP selection, plus the identity of the
/// decision record that produced it.
///
/// The `decision_id` is the whole point: it travels with the request
/// through the cascade and back out on the completion, which is what
/// makes decision→outcome a join rather than two unrelated log
/// streams. Without it the Tier-1 calibration contract
/// (`docs/specs/SCHEDULER_QUALITY.md` §5) has nothing to compare.
struct RankedSelection {
    /// Peers that strictly beat local, best-first.
    peers: Vec<(PeerInferenceEndpoint, ModelCandidate)>,
    decision_id: String,
    oicp_request_id: String,
}

/// A routing cascade plus the identity of the decision that produced
/// it. The cascade steps are tried in order; whichever one serves
/// closes the join by emitting an outcome carrying `decision_id` and
/// its own index (index > 0 means a failover happened, which is a
/// waste metric in `SCHEDULER_QUALITY.md` §5).
///
/// The identity fields sit outside the steps on purpose: a plan that
/// picked no peer still MADE a decision, and that decision still
/// needs an outcome. Folding the ids in beside a peer would silently
/// drop every `stay_local` record from the join.
///
/// This is now the only route representation. It replaced a
/// `SinglePeerSelection` that carried at most one peer and existed
/// solely for `complete()`, which is why the non-streaming path used
/// to give up after one declining peer while the streaming paths
/// walked the whole ranking.
struct RoutePlan {
    steps: Vec<RouteDecision>,
    decision_id: String,
    oicp_request_id: String,
}

/// Where a named request goes, plus the identity of the decision that says so.
///
/// Same shape and same reason as [`RoutePlan`]: the ids sit outside
/// the location because every named resolution — including `Unknown` — is a
/// decision that still needs an outcome to join back to. Produced only by
/// `resolve_named_dispatch`, which is the sole decider for this question.
struct NamedDispatch {
    located: NamedModelLocation,
    decision_id: String,
    oicp_request_id: String,
}

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
use crate::oicp_select::{classify_rtt_ms, ModelCandidate};

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
            from_node: *peer_node_id,
            model_id: model_id.to_string(),
            emitter: app_state.inner.contribution_emitter.clone(),
        })
    }
}

pub struct MeshInferenceProvider {
    local: Arc<dyn InferenceProvider>,
    mesh: Arc<dyn PeerEndpointSource>,
    /// Our own manifest. Built at construction and refreshed on
    /// runtime slot mutation (`load_extra_slot` / `unload_extra_slot`)
    /// via `refresh_self_manifest`.
    ///
    /// `ArcSwap` so readers (`locate_named_model`, the local scorer)
    /// take a cheap load-and-deref without ever blocking the writer.
    /// Writer is the runtime-extras handler: when an operator hot-
    /// loads a slot the new model id has to become visible to mesh
    /// routing immediately, otherwise `locate_named_model` returns
    /// Unknown and `/v1/chat/completions` 503s on the very slot we
    /// just installed.
    ///
    /// Confirmed 2026-05-20: a bench could hot-load Gemma into a
    /// daemon whose primary slot was Qwen3.6, but every chat call
    /// against gemma-* came back "no node in this mesh advertises
    /// model". Pre-refresh self_manifest was the source.
    self_manifest: arc_swap::ArcSwap<ProviderManifest>,
    /// The mesh-hosted shared model this node routes its primary (Slow) turns
    /// into (`[shared_model] model_id`), when set. `None` = ordinary local-first
    /// routing. Set post-construction by the daemon via `set_shared_model_id`;
    /// `ArcSwapOption` so the read path is lock-free like `self_manifest`.
    shared_model_id: arc_swap::ArcSwapOption<String>,
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
    peer_observations: Arc<RwLock<std::collections::HashMap<String, NodeObservations>>>,
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
    local_inflight_by_model: Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
    /// Slot-alias map mirrored from the daemon's `AppState`. Keyed by
    /// the alias name a caller might send (`commonwealth/primary`,
    /// `primary`, `fast`, …), valued at the GGUF stem currently bound
    /// to that slot. Populated by the daemon after slot registration
    /// (`set_slot_aliases`) and re-read on every request so a hot
    /// model swap takes effect on the next call.
    ///
    /// Used inside the `NamedModelLocation::Local` branch to rewrite
    /// `request.model_id` from the alias to the underlying GGUF before
    /// handing to the local provider. The mesh-routing decision in
    /// `locate_named_model` runs *before* this rewrite — so the alias
    /// is what the load-balancer sees, peers that advertise the same
    /// alias become routing candidates, and the resolution to a
    /// specific GGUF only happens on the node that actually serves
    /// the request.
    slot_aliases: arc_swap::ArcSwap<std::collections::HashMap<String, String>>,
    /// Total local in-flight inference count across every local-serve
    /// path on this node — explicit-model-id Local arm, OICP-routed
    /// fallback to local, and any other dispatch that ultimately runs
    /// against the local provider's slots.
    ///
    /// Published over gossip in [`commonwealth_core::capabilities::
    /// NodeCapabilities::current_in_flight`] so a remote scheduler
    /// (e.g. the founder selecting a peer) can see this node's
    /// *actual* load — including local-user traffic the remote side
    /// never originated. Without this, a workstation serving its own
    /// Claude-desktop inference appears phantom-idle to peers, who
    /// then route additional work here and contend with the local
    /// user. See `sovereign/docs/MESH_LOAD_AWARENESS.md` for the
    /// architectural backstory.
    ///
    /// Atomic so the gossip emitter can `.load()` lock-free without
    /// taking the `local_inflight_by_model` Mutex on every tick. All
    /// mutations route through `enter_local_total()` (returns an RAII
    /// guard that decrements on drop) — the only safe way to keep
    /// the counter in lock-step with reality across panic / stream
    /// drop / early-return paths.
    in_flight_publisher: Arc<AtomicU32>,
    /// Where routing decision records go (P1 of
    /// `docs/specs/SCHEDULER_QUALITY.md`).
    ///
    /// Injected rather than reached for as a process-global so tests
    /// can assert on the exact records a scenario produced without
    /// racing each other, and so a caller that wants no
    /// instrumentation pays nothing. Production wiring installs
    /// [`decision_log::TracingDecisionSink::from_env`].
    ///
    /// Emitting through this sink changes no routing decision. It is
    /// the observer that makes the decision legible in hindsight and
    /// replayable in the Tier-1 simulator.
    decision_sink: Arc<dyn DecisionSink>,
    /// Unix seconds of the last fleet snapshot emitted into the
    /// decision stream (P3). `0` = none yet. Atomic so the rate limit
    /// costs nothing on the hot path.
    last_snapshot_unix: Arc<std::sync::atomic::AtomicU64>,
}

/// How often a fleet observation snapshot is folded into the decision
/// record stream. Coarse on purpose — this samples fleet
/// *composition*, which changes on the scale of nodes joining and
/// EWMAs warming up, not the per-request state the decision records
/// already carry.
const SNAPSHOT_INTERVAL_SECS: u64 = 60;

impl MeshInferenceProvider {
    /// Standard constructor — takes the live `EmbeddedDaemon` so
    /// production wiring is unchanged. Internally upcasts to
    /// `Arc<dyn PeerEndpointSource>` via the blanket impl above;
    /// callers don't have to think about the trait.
    ///
    /// Creates a private in-flight publisher — fine for tests and
    /// for the rare daemon path that doesn't share counter state
    /// with an outer `AppState`. Production code should prefer
    /// [`MeshInferenceProvider::with_in_flight_publisher`] so the
    /// gossip emitter reads the same atomic the MIP guards write
    /// to.
    pub fn new(local: Arc<dyn InferenceProvider>, mesh: Arc<EmbeddedDaemon>) -> Self {
        Self::with_peer_source(local, mesh as Arc<dyn PeerEndpointSource>)
    }

    /// Rebuild `self_manifest` against the current state of the local
    /// provider. Called after a runtime slot mutation — `load_extra_slot`
    /// / `unload_extra_slot` — so `locate_named_model` and the local
    /// scorer immediately see the new lineup. Cheap: the manifest is a
    /// flat list pulled from `EmbeddedLlamaCpp::loaded_models`; no I/O.
    pub fn refresh_self_manifest(&self) {
        self.refresh_self_manifest_because("slot mutation");
    }

    /// As [`Self::refresh_self_manifest`], carrying WHY.
    ///
    /// Glassbox: an operator reading the log must be able to attribute a change
    /// in what this node advertises to the event that caused it — not merely
    /// observe that one happened. The causes are few and all interesting: a slot
    /// hot-loaded, a compute child reaching Serving, a child retired.
    pub fn refresh_self_manifest_because(&self, cause: &str) {
        let new_manifest = build_self_manifest(self.local.as_ref());
        tracing::info!(
            target: "compute_child",
            models = new_manifest.models.len(),
            %cause,
            "mesh-inference: self_manifest refreshed"
        );
        self.self_manifest.store(Arc::new(new_manifest));
    }

    /// Recompute the manifest and republish ONLY if the advertised id set has
    /// drifted from what is currently published. Returns whether it republished.
    ///
    /// This is a detector for our own bug, not a delivery mechanism. Every path
    /// that changes what the local provider can serve is supposed to call
    /// [`Self::refresh_self_manifest_because`]; a `true` here means one of them
    /// did not, so it logs at WARN with both id sets. That invalidation set has
    /// now been incomplete twice — 2026-05-20 (hot-loaded extras) and 2026-07-28
    /// (a compute child reaching Serving after boot) — each time discovered only
    /// when a user's request 503'd against a healthy node.
    pub fn reconcile_self_manifest(&self) -> bool {
        let published: Vec<String> = {
            let cur = self.self_manifest.load();
            let mut ids: Vec<String> = cur.models.iter().map(|m| m.id.clone()).collect();
            ids.sort();
            ids
        };
        let fresh_manifest = build_self_manifest(self.local.as_ref());
        let fresh: Vec<String> = {
            let mut ids: Vec<String> = fresh_manifest.models.iter().map(|m| m.id.clone()).collect();
            ids.sort();
            ids
        };
        if fresh == published {
            return false;
        }
        tracing::warn!(
            target: "compute_child",
            ?published,
            ?fresh,
            "mesh-inference: self_manifest had DRIFTED — a state change did not publish. \
             Republishing; the missing refresh call is a bug worth finding"
        );
        self.self_manifest.store(Arc::new(fresh_manifest));
        true
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
            self_manifest: arc_swap::ArcSwap::from_pointee(self_manifest),
            shared_model_id: arc_swap::ArcSwapOption::empty(),
            peer_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
            http,
            peer_observations: Arc::new(RwLock::new(std::collections::HashMap::new())),
            local_observations: Arc::new(RwLock::new(local_obs)),
            extension_registry: Arc::new(RwLock::new(ExtensionRegistry::new())),
            peer_health: Arc::new(commonwealth_core::peer_health::PeerHealthTracker::new()),
            local_inflight_by_model: Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            slot_aliases: arc_swap::ArcSwap::from_pointee(std::collections::HashMap::new()),
            in_flight_publisher: Arc::new(AtomicU32::new(0)),
            decision_sink: Arc::new(decision_log::TracingDecisionSink::from_env()),
            last_snapshot_unix: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Replace the routing-decision sink (P1 of
    /// `docs/specs/SCHEDULER_QUALITY.md`).
    ///
    /// Tests install [`decision_log::CaptureDecisionSink`] to assert
    /// on exactly the records a scenario produced; a trace-capture
    /// session installs a [`decision_log::TracingDecisionSink`]
    /// pointed at an explicit JSONL path. The default —
    /// [`decision_log::TracingDecisionSink::from_env`] — emits to
    /// `tracing` and, when `SOVEREIGN_DECISION_LOG` is set, to that
    /// file.
    pub fn with_decision_sink(mut self, sink: Arc<dyn DecisionSink>) -> Self {
        self.decision_sink = sink;
        self
    }

    /// The installed decision sink. Cloned into the `OutcomeContext`
    /// that rides along on a dispatched stream so the completion half
    /// of the join is emitted wherever the request finishes.
    pub fn decision_sink(&self) -> Arc<dyn DecisionSink> {
        Arc::clone(&self.decision_sink)
    }

    /// Build the completion-half context for a cascade step that is
    /// about to serve. `attempt_index` is the step's position in the
    /// cascade — `0` is the decision's first choice, anything higher
    /// means the steps in `failovers` were tried and did not serve.
    fn outcome_ctx(
        &self,
        decision_id: &str,
        oicp_request_id: &str,
        served_by: ServedBy,
        attempt_index: u32,
        failovers: &[decision_log::FailoverAttempt],
    ) -> OutcomeContext {
        OutcomeContext {
            sink: Arc::clone(&self.decision_sink),
            decision_id: decision_id.to_string(),
            oicp_request_id: oicp_request_id.to_string(),
            served_by,
            attempt_index,
            failovers: failovers.to_vec(),
        }
    }

    /// Production constructor variant that accepts an externally-owned
    /// in-flight publisher Arc. Daemon bootstrap passes the same Arc
    /// it holds on `AppState`, so the gossip emitter (reading via
    /// AppState) sees the same atomic this MIP's guards write to.
    ///
    /// Functionally identical to [`new`] except for the publisher
    /// source. Survives hot-reload: the new MIP receives the same
    /// Arc, so live guards from the previous MIP that haven't
    /// dropped yet continue to update the shared counter exactly as
    /// the new MIP's guards do.
    pub fn with_in_flight_publisher(
        local: Arc<dyn InferenceProvider>,
        mesh: Arc<EmbeddedDaemon>,
        publisher: Arc<AtomicU32>,
    ) -> Self {
        let mut me = Self::with_peer_source(local, mesh as Arc<dyn PeerEndpointSource>);
        me.in_flight_publisher = publisher;
        me
    }

    /// Variant that accepts an arbitrary `PeerEndpointSource` AND an
    /// externally-owned in-flight publisher. The composite-source
    /// case — gossiped peers + pinned worker pods — uses this so the
    /// daemon's `AppState` shares an Arc with the MIP's guards while
    /// still routing through a non-`EmbeddedDaemon` source.
    /// Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md.
    pub fn with_peer_source_and_publisher(
        local: Arc<dyn InferenceProvider>,
        mesh: Arc<dyn PeerEndpointSource>,
        publisher: Arc<AtomicU32>,
    ) -> Self {
        let mut me = Self::with_peer_source(local, mesh);
        me.in_flight_publisher = publisher;
        me
    }

    /// Hand out a shared reference to the gossiped in-flight counter.
    /// Mostly useful for tests asserting on the published value
    /// directly. In production the daemon prefers
    /// [`with_in_flight_publisher`] so the Arc identity is fixed
    /// across hot reloads.
    pub fn in_flight_publisher(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.in_flight_publisher)
    }

    /// Install the slot-alias map. Called by the daemon after model
    /// slots are registered so the mesh-aware path can rewrite
    /// `commonwealth/primary` → the local GGUF stem before serving
    /// locally. Safe to call multiple times — each call atomically
    /// swaps in the new map, so a runtime model swap can publish a
    /// new mapping without restarting the daemon.
    pub fn set_slot_aliases(&self, aliases: std::collections::HashMap<String, String>) {
        self.slot_aliases.store(Arc::new(aliases));
    }

    /// Install (or clear) the mesh-hosted shared model this node routes its
    /// primary turns into — `[shared_model] model_id`. `None` reverts to ordinary
    /// local-first routing. Atomic swap, safe to call at runtime / on reload.
    pub fn set_shared_model_id(&self, model_id: Option<String>) {
        self.shared_model_id.store(model_id.map(Arc::new));
    }

    /// Snapshot of per-peer health for diagnostics surfaces.
    pub fn peer_health_snapshot(&self) -> Vec<(String, bool, u32, u64)> {
        self.peer_health.snapshot()
    }

    /// Emit a fleet snapshot into the decision stream if one is due.
    ///
    /// A capture has to be **self-contained** to be replayable: the
    /// episodes and the fleet they ran against must come from the
    /// same file and the same moments. Interleaving the snapshot into
    /// the record stream on a timer achieves that with one env var
    /// and no second collection step — and because a long capture
    /// spans a changing fleet, one snapshot at the start would model
    /// a mesh that stopped existing halfway through.
    ///
    /// The interval is deliberately coarse relative to gossip (10s)
    /// and manifest TTL (60s): this samples fleet *composition*, not
    /// per-request state, which the decision records already carry.
    async fn maybe_emit_snapshot(&self, now_unix: u64) {
        let last = self.last_snapshot_unix.load(Ordering::Relaxed);
        if last != 0 && now_unix.saturating_sub(last) < SNAPSHOT_INTERVAL_SECS {
            return;
        }
        // Claim the slot before doing the work so concurrent requests
        // don't all decide they are the one to snapshot.
        if self
            .last_snapshot_unix
            .compare_exchange(last, now_unix, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let snapshot = self.observation_snapshot().await;
        self.decision_sink
            .record(decision_log::DecisionEvent::Snapshot(Box::new(snapshot)));
    }

    /// P3 of `docs/specs/SCHEDULER_QUALITY.md` — export the whole
    /// observation state the scheduler decides from.
    ///
    /// Joins the three views that today live in three unrelated
    /// places: this node's per-peer `NodeObservations` (latency and
    /// throughput EWMAs), the peers' gossiped `BenchmarkResult` and
    /// load signals, and `PeerHealthTracker`'s quarantine state.
    /// Separately, none of them answers "what fleet is this?";
    /// together they are exactly the input a simulator's service-time
    /// model needs to be **fit from the real mesh** instead of
    /// hand-tuned — which is the difference between §3's "p95
    /// improves 2.5×" being evidence and being an artifact of chosen
    /// constants.
    ///
    /// Read-only and allocation-cheap; safe to call from a diagnostic
    /// route on a live daemon.
    pub async fn observation_snapshot(&self) -> decision_log::FleetSnapshot {
        use crate::decision_log::{FleetSnapshot, LocalObservationRecord, PeerObservationRecord};
        use crate::decision_trace::TRACE_SCHEMA;
        let now = Self::now_unix_secs();
        let peer_obs = self.peer_observations.read().await.clone();
        let health: std::collections::HashMap<String, (bool, u32, u64)> = self
            .peer_health
            .snapshot()
            .into_iter()
            .map(|(name, quarantined, fails, cooldown)| (name, (quarantined, fails, cooldown)))
            .collect();

        // Peers are enumerated from the endpoint source rather than
        // from the observation map: a peer this node has never
        // dispatched to has no observations but is still part of the
        // fleet, and a fleet description that omits the idle nodes
        // would misstate the composition the sim is meant to
        // reproduce.
        let endpoints = self.mesh.peer_inference_endpoints().await;
        let mut peers: Vec<PeerObservationRecord> = endpoints
            .into_iter()
            .map(|p| {
                let (quarantined, consecutive_failures, cooldown_remaining_secs) =
                    health.get(&p.name).copied().unwrap_or((false, 0, 0));
                PeerObservationRecord {
                    observations: peer_obs.get(&p.name).cloned().unwrap_or_default(),
                    node_id: Some(p.node_id.to_hex()),
                    benchmark: p.benchmark.clone(),
                    gossiped_in_flight: p.current_in_flight,
                    inference_availability: p.inference_availability,
                    gossip_age_secs: (p.gossip_last_seen_unix > 0)
                        .then(|| now.saturating_sub(p.gossip_last_seen_unix)),
                    quarantined,
                    consecutive_failures,
                    cooldown_remaining_secs,
                    name: p.name,
                }
            })
            .collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name));

        FleetSnapshot {
            schema: TRACE_SCHEMA.to_string(),
            captured_at_unix: now,
            local: LocalObservationRecord {
                observations: self.local_observations.read().await.clone(),
                // Structurally `None`: no producer exists, by decision.
                // See the note on `LocalCandidateView.benchmark` below.
                benchmark: None,
                advertised_models: self
                    .self_manifest
                    .load()
                    .models
                    .iter()
                    .map(|m| m.id.clone())
                    .collect(),
                in_flight_published: self.in_flight_publisher.load(Ordering::Relaxed),
            },
            peers,
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

    /// This node's id as lowercase hex, for the `X-Node-Id` header —
    /// the identity that tells a peer daemon this is peer traffic and
    /// not its own user.
    ///
    /// `None` when we don't know it: a test stub that synthesizes
    /// peers from thin air, or a daemon that has not joined a mesh.
    /// Unknown means UNSTAMPED, and unstamped means the far side
    /// treats the request as local — so this returning `None` in
    /// production would silently disarm M5's admission gates rather
    /// than fail loudly. It is `Option` because the trait's default
    /// genuinely cannot know the id, not because absence is fine.
    ///
    /// One encoding for one key (ARCH §10.6): `NodeId::to_hex` is
    /// `hex::encode`, which is what `commonwealth-api`'s
    /// `parse_x_node_id` decodes. This used to be an open-coded
    /// `{b:02x}` fold at the manifest-fetch call site.
    async fn local_node_id_hex(&self) -> Option<String> {
        self.mesh.local_node_id().await.map(|id| id.to_hex())
    }

    /// Current wall-clock in unix seconds. Extracted so tests can
    /// mock and so the handful of registry-record sites all use
    /// the same clock source.
    fn now_unix_secs() -> u64 {
        sovereign_core::time::unix_now_u64()
    }

    /// Record that a request was dispatched to this peer (or local,
    /// if `peer_name` is `None`). Increments in-flight + samples.
    pub async fn record_dispatch(&self, peer_name: Option<&str>) {
        match peer_name {
            None => scheduler_core::observe_dispatch(&mut *self.local_observations.write().await),
            Some(name) => {
                let mut obs = self.peer_observations.write().await;
                scheduler_core::observe_dispatch(obs.entry(name.to_string()).or_default());
            }
        }
    }

    /// Record that a dispatched request completed successfully.
    /// Decrements in-flight; leaves failure counters untouched so
    /// the rolling rate drifts toward zero.
    pub async fn record_success(&self, peer_name: Option<&str>) {
        match peer_name {
            None => scheduler_core::observe_success(&mut *self.local_observations.write().await),
            Some(name) => {
                let mut map = self.peer_observations.write().await;
                scheduler_core::observe_success(map.entry(name.to_string()).or_default());
            }
        }
    }

    /// Record that a dispatched request failed. Decrements in-flight
    /// and bumps the rolling failure rate toward 1.0.
    pub async fn record_failure(&self, peer_name: Option<&str>) {
        match peer_name {
            None => scheduler_core::observe_failure(&mut *self.local_observations.write().await),
            Some(name) => {
                let mut map = self.peer_observations.write().await;
                let entry = map.entry(name.to_string()).or_default();
                scheduler_core::observe_failure(entry);
                tracing::warn!(
                    target: "mesh.health",
                    peer = name,
                    failure_rate = entry.recent_failure_rate,
                    in_flight = entry.in_flight,
                    "peer dispatch failed — failure-rate EMA climbing; the scorer will deprioritize this peer"
                );
            }
        }
    }

    /// Book a failed peer attempt against that peer's HEALTH — unless
    /// the peer shed, in which case it is not booked at all.
    ///
    /// A shed is a `503` + `Retry-After` produced in ~10 ms by a
    /// healthy daemon that has decided not to serve right now: its
    /// operator paused contribution, its local user is at the
    /// keyboard, or it is already at `max_peer_inflight`. Treating
    /// that as a fault is not a cosmetic mistake. `PeerHealthTracker`
    /// quarantines on `FAILURE_THRESHOLD` consecutive failures, and a
    /// quarantined peer is dropped from the candidate set *before*
    /// its manifest is even consulted — so with the ceiling at its
    /// default of 1, a mere three concurrent turns would bench a
    /// perfectly healthy neighbour for a cooldown.
    ///
    /// That failure mode did not exist until this commit stamped
    /// `X-Node-Id`: before it, peer inference was never gated, so no
    /// shed could ever be booked. The stamp and this exemption are
    /// one change, and separating them would ship the regression.
    ///
    /// What is NOT skipped is the caller's `record_failure` on
    /// `peer_observations`: that decrements the in-flight count this
    /// attempt incremented (skipping it would leak the counter and
    /// permanently mis-rank the peer), and nudging the load-balance
    /// EMA away from a peer that just said "I'm full" is the correct
    /// response. Backing off is right; declaring it broken is not.
    fn book_peer_failure(&self, peer_name: &str, err_text: &str, shed: bool) {
        if shed {
            tracing::info!(
                target: "mesh.health",
                peer = peer_name,
                error = err_text,
                "peer SHED this turn (503/429) — not booked against peer health; \
                 a refusal to serve is not a fault and must not accumulate toward quarantine"
            );
            return;
        }
        self.peer_health.record_failure(peer_name);
    }

    /// Serve an explicitly-named model from this node's own provider,
    /// on the NON-STREAMING path.
    ///
    /// One body, two callers: the ordinary `NamedModelLocation::Local`
    /// route, and the fall-back a `LocalAlternative::LocalHasIt` peer
    /// route takes when every peer address fails. It is extracted
    /// rather than copied because the second caller arrived as a bug
    /// fix, and a hand-copied second body is how this file already
    /// grew three features that existed on one routing surface and
    /// not the other.
    ///
    /// `attempt_index` and `failovers` are what distinguish the two:
    /// served-first-try records `0, &[]`, while the fall-back records
    /// the peer attempts it is recovering from, so the decision log
    /// shows a peer was tried and declined rather than implying we
    /// went local by choice.
    ///
    /// The in-flight `guard` is passed IN rather than taken here: the
    /// route plan already entered it when it chose this step, and the
    /// counter must be raised at decision time (that is what stops
    /// concurrent callers from all reading zero and piling onto the
    /// same node), not at serve time.
    async fn complete_named_locally(
        &self,
        request: &CompletionRequest,
        model_id: &str,
        guard: LocalInflightGuard,
        decision_id: &str,
        oicp_request_id: &str,
        attempt_index: u32,
        failovers: &[decision_log::FailoverAttempt],
    ) -> Result<CompletionResponse> {
        let _guard = guard;
        // Resolve slot aliases for the local-serving path only — the
        // routing decision already saw the alias and chose this node,
        // so peers that also advertise the alias got their fair chance
        // to win. The underlying provider works in terms of GGUF
        // stems, so we rewrite here and hand it the resolved id. No-op
        // when the requested id isn't an alias.
        let aliases = self.slot_aliases.load();
        let resolved = aliases.get(model_id).cloned();
        let log_model = model_id.to_string();
        match &resolved {
            Some(target) => tracing::info!(
                alias = %log_model,
                target = %target,
                "mesh-inference: serving complete() locally — resolved slot alias"
            ),
            None => tracing::info!(
                model = %log_model,
                "mesh-inference: serving complete() locally by explicit model name"
            ),
        }
        // PIN THE RESOLVED NAME, always — the alias's target when the id
        // is an alias, the id itself otherwise.
        //
        // The `None` arm used to pass the caller's request straight
        // through, which is only harmless when the caller already named
        // the model. A SHARED PRIMARY does not: it is resolved by this
        // node, not named by the client, so an unpinned request reaches
        // the provider with `model_id: None` and its slot picker falls
        // back to choosing by SPEED — the caller asked for the shared
        // model and silently gets whatever this node felt like serving.
        // The pre-unification body pinned it by rewriting the request up
        // front; that guarantee has to live here now. Caught by
        // `a_shared_primary_resolving_locally_still_names_the_model_it_resolved`
        // during a deliberate re-check of the unification, NOT by the
        // suite, which was green.
        let effective_id = resolved.unwrap_or_else(|| model_id.to_string());
        let serve_request = pinned_request(request, Some(&effective_id));
        let serve_request = serve_request.as_ref();
        let started = Instant::now();
        let result = self.local.complete(serve_request).await;
        // Close the decision->outcome join. `ttft_ms` is None because
        // there is no stream to time a first token against; reading
        // one off a non-streaming call would be a fabrication (§18.3).
        let ctx = self.outcome_ctx(
            decision_id,
            oicp_request_id,
            ServedBy::Local {
                model_id: log_model.clone(),
            },
            attempt_index,
            failovers,
        );
        match &result {
            Ok(_) => ctx.complete(None, Some(started.elapsed().as_secs_f64() * 1000.0), None),
            Err(e) => ctx.failed(e.to_string(), false),
        }
        result
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
    /// [`crate::oicp_select::adjust_for_observations`] so LAN peers pick up their
    /// locality bonus in real deployments instead of defaulting
    /// to `Far`.
    async fn get_peer_manifest(&self, peer: &PeerInferenceEndpoint) -> Option<ManifestRead> {
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
    async fn get_peer_manifest_fresh(&self, peer: &PeerInferenceEndpoint) -> Option<ManifestRead> {
        self.get_peer_manifest_inner(peer, true).await
    }

    async fn get_peer_manifest_inner(
        &self,
        peer: &PeerInferenceEndpoint,
        bypass_cache: bool,
    ) -> Option<ManifestRead> {
        // `to_hex()`, not `to_string()`: `NodeId`'s `Display` is the
        // TRUNCATED human form (`node-` + the first 8 of 16 bytes) and
        // its own doc says so. Two peers sharing a first-8-byte prefix
        // would have shared one cache entry and been served each
        // other's manifests — vanishingly unlikely for random ids, but
        // a human-facing rendering has no business being a cache key.
        // Surfaced by the decision records: two test peers scored
        // against one manifest because their ids rendered identically.
        let key = peer.node_id.to_hex();
        // Cache hit (unless caller demanded a fresh probe).
        if !bypass_cache {
            let cache = self.peer_cache.read().await;
            if let Some(entry) = cache.get(&key) {
                let age = entry.fetched_at.elapsed();
                if age < MANIFEST_TTL {
                    return Some(ManifestRead {
                        manifest: entry.manifest.clone(),
                        rtt_ms: entry.rtt_ms,
                        age_secs: age.as_secs(),
                        from_cache: true,
                    });
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
        let local_node_id_hex = self.local_node_id_hex().await;

        for base in &peer.base_urls {
            let root = base
                .trim_end_matches('/')
                .trim_end_matches("/v1")
                .trim_end_matches('/');
            let url = format!("{root}/oicp/v1/capabilities");
            let started = Instant::now();
            // Pinned worker pods serve their TLS-pinned manifest on
            // the same `:9742` listener as inference — use the pod's
            // pinned client + worker bearer to reach it. The default
            // mesh `self.http` would fail TLS verification.
            let (client, bearer) = match &peer.transport {
                Some(t) => (t.client.clone(), Some(t.bearer.clone())),
                None => (self.http.clone(), None),
            };
            let mut req = client.get(&url);
            if let Some(ref id_hex) = local_node_id_hex {
                req = req.header("X-Node-Id", id_hex);
            }
            if let Some(b) = bearer {
                req = req.bearer_auth(b);
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    // Lock the RTT in before the JSON parse — we
                    // want the network round-trip, not the parse
                    // time, to classify the peer's locality.
                    let rtt_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
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
                                let mut registry = self.extension_registry.write().await;
                                for model in &m.models {
                                    for claim in &model.claims {
                                        registry.observe_advertisement(&claim.hint, now);
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
                            return Some(ManifestRead {
                                manifest: m,
                                rtt_ms,
                                age_secs: 0,
                                from_cache: false,
                            });
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
    /// OICP peer selection: the single best peer that strictly beats local, or
    /// `None`. Thin wrapper over [`Self::select_peers_ranked`] for callers (the
    /// non-streaming `complete`, tests) that only want the top pick.
    /// OICP peer selection, ranked best-first. Every peer that strictly beats
    /// local is a candidate; the routing cascade tries them in order before
    /// falling back to local, so a 503 from the best peer fails over to the
    /// next-best peer instead of collapsing straight to local.
    ///
    /// `path` names WHY this ranking ran: [`DecisionPath::RankedOicp`]
    /// for a request that carried no named target, or
    /// [`DecisionPath::NamedFallthrough`] for one whose soft named
    /// target resolved to nobody. The scoring is identical either way
    /// — only the record's label differs, so an operator can count
    /// shared-cluster outages without them vanishing into the ordinary
    /// ranked population.
    async fn select_peers_ranked(
        &self,
        request: &CompletionRequest,
        path: DecisionPath,
    ) -> RankedSelection {
        // Glassbox: every stay-local decision below names its gate so
        // the routing outcome is reconstructable from a debug log.
        // `oicp_request_id` is the caller-declared tag (e.g. the
        // workload resolver's `wl-<class>-<id>`), joinable against
        // the adapter's `slot_selected` event on the serving node.
        let oicp_request_id = request
            .oicp
            .as_ref()
            .and_then(|o| o.request_id.as_deref())
            .unwrap_or("");
        // P1: one decision record per decision point. The builder is
        // filled as we score and emitted exactly once on every exit
        // path below — including the gates, whose records carry no
        // candidates but do name the gate. "The hub lost" and "the
        // hub was never considered" are different failures and the
        // record has to be able to say which happened.
        let rec = DecisionBuilder::new(oicp_request_id, path, Self::request_facts(request));
        if !Self::has_routing_signal(request) {
            tracing::debug!(
                oicp_request_id = %oicp_request_id,
                gate = "no_routing_signal",
                "mesh-inference: staying local"
            );
            return self.gated(rec, "no_routing_signal");
        }
        // Operator override: short-circuit all outbound peer
        // routing. Set `SOVEREIGN_DISABLE_PEER_INFERENCE=1` in the
        // daemon's environment to keep every inference call local
        // regardless of load-balance scoring. Use when reserving
        // a peer's compute (long ingests that shouldn't clobber a
        // colleague's machine) — the request stays local even if
        // a peer would strictly outscore the local manifest.
        if peer_inference_disabled() {
            tracing::debug!(
                oicp_request_id = %oicp_request_id,
                gate = "operator_disabled",
                "mesh-inference: peer routing disabled via \
                 SOVEREIGN_DISABLE_PEER_INFERENCE — staying local"
            );
            return self.gated(rec, "operator_disabled");
        }
        // SLOT_POLICY §5 — the offload gate. A request crosses the
        // network to a peer only when its envelope both permits
        // sharding (`MeshAllowed`) and tolerates a hop (latency
        // class != `Fast`). This single predicate replaces the two
        // gates that used to live here — a privacy check and a
        // `preferred_speed != Slow` check — which had drifted into
        // an incoherent pair: the speed literal (a derived shadow of
        // the latency class) was the real routing lever, while the
        // OICP envelope the whole protocol exists to honour was only
        // consulted for privacy. Now the envelope decides both.
        // `has_routing_signal` above already proved the envelope is
        // present; bind it defensively and bail if somehow absent.
        let Some(req_oicp) = request.oicp.as_ref() else {
            return self.gated(rec, "envelope_absent");
        };
        let verdict = crate::oicp_select::offload_verdict(req_oicp);
        if verdict != crate::oicp_select::OffloadVerdict::Eligible {
            // The budget case is reported apart from the other two on
            // purpose: it does not mean "this work stays home by policy",
            // it means SOME OTHER NODE already forwarded this request and
            // we are the last hop. An operator chasing a slow answer needs
            // to be able to tell those apart (§18.3).
            tracing::debug!(
                oicp_request_id = %oicp_request_id,
                gate = verdict.gate(),
                sharding = ?req_oicp.sharding(),
                latency = ?req_oicp.effective_latency_class(),
                forward_budget = req_oicp.effective_forward_budget(),
                "mesh-inference: staying local (SLOT_POLICY §5: offload iff \
                 MeshAllowed AND latency != Fast AND forward budget remains)"
            );
            return self.gated(rec, verdict.gate());
        }

        // Local is always a candidate. `None` means no loaded
        // model's claims can serve the request — any peer that CAN
        // then wins automatically. After claim-scoring, fold in
        // v0.3 §7 operational adjustments so a hot local slot can
        // lose to an idle peer on load, and a reliable peer can
        // beat a failure-prone local.
        //
        // That first clause was false from June 10th until 2026-07-27
        // — see F9 — and the fix is at the `local_obs` binding below,
        // not here.
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
        // F9 (`SCHEDULER_QUALITY.md`) — the local load signal, wired.
        //
        // `local_observations.in_flight` has no writer on the dispatch
        // path: its only mutator is `record_dispatch(None)`, which has
        // zero callers in the repository. The scorer therefore read a
        // permanent 0, `load_penalty` was a permanent 1.0, and the
        // local candidate was scored idle no matter how backed up this
        // node actually was.
        //
        // The true count already exists and needs no new bookkeeping:
        // `in_flight_publisher` is the RAII-maintained total this node
        // gossips (`enter_local_total`, saturating on both edges). Two
        // reasons to prefer it over teaching the dispatch path to call
        // `record_dispatch(None)`: it cannot drift out of pairing with
        // the guards that already maintain it, and it makes both sides
        // of the comparison the SAME quantity — a peer's in-flight
        // number is *its* published total (`scheduler_core.rs:512`
        // prefers the gossiped count), so scoring local on a private
        // counter was comparing two numbers that only shared a name.
        //
        // Deliberately NOT fixed alongside it: the peer half of F9
        // (peer `samples` never leaves 0 on the ranked path, pinning
        // `cold_start_weight` at 0.7). Tier 1 prices that blindness as
        // *protective* on four of five fleets — up to -33% mean latency
        // — because a permanently cold peer is a brake on over-offload.
        // Completing that wiring is a regression, not a fix; see the
        // `what_the_scorer_loses_by_never_seeing_its_own_load` arm table
        // and F7, which is the same trap.
        let local_obs = {
            let mut obs = self.local_observations.read().await.clone();
            obs.in_flight = self.in_flight_publisher.load(Ordering::Relaxed);
            obs
        };
        let self_manifest = self.self_manifest.load();
        let now_unix = Self::now_unix_secs();

        // ── Gather ──────────────────────────────────────────────
        // Everything from here to `scheduler_core::rank` is I/O and
        // environment: fetch what this node currently believes about
        // its peers. Nothing below decides anything. The split is
        // load-bearing — the Tier-1 simulator
        // (`SCHEDULER_QUALITY.md` §5) replaces exactly this half and
        // shares the other, so arm 0 of the sim runs the production
        // decision rather than a transcription of it.
        let peers = self.mesh.peer_inference_endpoints().await;
        let peer_obs_snapshot = self.peer_observations.read().await.clone();
        // Forced-choice sentinel (SLOT_POLICY §6): a request eliciting a
        // calibrated one-pass distribution can only be honoured by a peer
        // whose manifest advertises `x:forced_choice`. Compute the need
        // once; the core excludes non-advertising peers so the sentinel
        // never crosses to a peer that would silently fall back to
        // K-sampling. (Explicit `model_id` dispatch is honoured by name
        // and never reaches this scorer, so it is not filtered here.)
        let needs_forced_choice = request.forced_choice_candidates().is_some();
        let mut views: Vec<PeerCandidateView> = Vec::with_capacity(peers.len());
        for peer in &peers {
            // A quarantined peer is skipped *before* the manifest
            // fetch — the exclusion costs no network. The core
            // records the reason.
            let quarantined = self.peer_health.is_quarantined(&peer.name);
            let manifest = if quarantined {
                None
            } else {
                self.get_peer_manifest(peer).await.map(|m| PeerManifestView {
                    manifest: m.manifest,
                    rtt_ms: m.rtt_ms,
                    age_secs: m.age_secs,
                    from_cache: m.from_cache,
                })
            };
            views.push(PeerCandidateView {
                name: peer.name.clone(),
                node_id_hex: peer.node_id.to_hex(),
                quarantined,
                pinned_transport: peer.transport.is_some(),
                gossiped_in_flight: peer.current_in_flight,
                availability: peer.inference_availability,
                gossip_last_seen_unix: peer.gossip_last_seen_unix,
                benchmark: peer.benchmark.clone(),
                observations: peer_obs_snapshot
                    .get(&peer.name)
                    .cloned()
                    .unwrap_or_default(),
                manifest,
            });
        }

        // ── Decide ──────────────────────────────────────────────
        // `tie_band` is `None` here by construction — production ranks
        // on the product objective, which has no scale on which two
        // candidates are "close". It becomes readable the moment
        // production adopts §4.1, and not before.
        let RankResult {
            ranked, decision, ..
        } = scheduler_core::rank(
            rec,
            RankInputs {
                now_unix,
                oicp_request_id,
                req: req_oicp,
                needs_forced_choice,
                // Production still ranks on the product. §4.1's
                // predicted-time objective is measured as a Tier-1 arm
                // first (`SCHEDULER_QUALITY.md` §6: behavioural work
                // goes INTO the sim as arms, not into production).
                objective: RankObjective::Product,
                // §4.1's tier floor is likewise a Tier-1 arm
                // first. `None` here is what keeps every arm
                // recorded before it comparable — production
                // behaviour is provably unchanged by this file.
                tier_floor: TierFloor::None,
                local: LocalCandidateView {
                    manifest: &self_manifest,
                    observations: &local_obs,
                    // Always `None`, and that is a decision rather than
                    // an omission. The scorer still accepts a benchmark
                    // — `scheduler_core`'s tests exercise both arms —
                    // but nothing on this node produces one, because
                    // the probe that used to (`run_baseline_benchmark`,
                    // deleted 2026-07-28) measured the small always-hot
                    // slot and `throughput_factor` then extrapolated
                    // linearly on the size ratio to whatever model was
                    // being scored. That law is false; decode is
                    // bandwidth-bound and scales sub-linearly.
                    // `SCHEDULER_QUALITY.md` §4.5 prices the error at
                    // −56% mean latency on large models.
                    //
                    // The honest producer is `svrn mesh bench`, which
                    // measures the model actually being served. It
                    // deliberately does NOT write here: its consumer is
                    // a human deciding whether to add a machine, not
                    // the ranked dispatch, and pointing it at this
                    // field would ship §4.5's regression with no other
                    // code change. Wiring it up is a regression, not a
                    // fix — measure it as a Tier-1 arm first.
                    benchmark: None,
                },
                peers: &views,
            },
        );

        // P3: fold a fleet snapshot into the same record stream on a
        // slow cadence. Cheap (a clone of two small maps) and rate-
        // limited to at most one per SNAPSHOT_INTERVAL_SECS, so a busy
        // node pays it once a minute, not once a request.
        self.maybe_emit_snapshot(now_unix).await;

        // Re-pair the ranked indices with the endpoints we own. The
        // core deals in indices so it never has to name
        // `PeerInferenceEndpoint`, which lives on the daemon side of
        // the crate and drags transport with it.
        let mut owned: Vec<Option<PeerInferenceEndpoint>> = peers.into_iter().map(Some).collect();
        let winners: Vec<(PeerInferenceEndpoint, ModelCandidate)> = ranked
            .into_iter()
            .filter_map(|w| {
                owned
                    .get_mut(w.view_idx)
                    .and_then(Option::take)
                    .map(|p| (p, w.candidate))
            })
            .collect();

        let decision_id = decision.decision_id.clone();
        decision_log::emit_decision(&self.decision_sink, decision);
        RankedSelection {
            peers: winners,
            decision_id,
            oicp_request_id: oicp_request_id.to_string(),
        }
    }

    /// Emit a decision that never reached scoring, and return an
    /// empty selection. Every gate in [`Self::select_peers_ranked`]
    /// exits through here so a gated request is as visible in the
    /// record stream as a scored one — "stayed local because
    /// `latency == Fast`" is an answer; a missing record is not.
    fn gated(&self, rec: DecisionBuilder, gate: &str) -> RankedSelection {
        let decision = rec.finish(
            Verdict::Gated {
                gate: gate.to_string(),
            },
            &[],
        );
        let decision_id = decision.decision_id.clone();
        let oicp_request_id = decision.oicp_request_id.clone();
        decision_log::emit_decision(&self.decision_sink, decision);
        RankedSelection {
            peers: Vec::new(),
            decision_id,
            oicp_request_id,
        }
    }

    /// The request-side facts a decision record carries. Read from
    /// the OICP envelope where present, defaulted the same way the
    /// scheduler itself defaults them (§8: absent hint → `general`,
    /// absent latency → `normal`) so a replayed record reconstructs
    /// the same request the scorer saw.
    fn request_facts(request: &CompletionRequest) -> RequestFacts {
        match request.oicp.as_ref() {
            Some(o) => RequestFacts {
                capability_hint: o.effective_hint().to_string(),
                latency_class: format!("{:?}", o.effective_latency_class()),
                sharding: format!("{:?}", o.sharding()),
                context_tokens: o.context_tokens,
                max_output_tokens: o.max_output_tokens,
                preferred_speed: format!("{:?}", request.preferred_speed),
                explicit_model_id: request.model_id.clone(),
            },
            None => RequestFacts {
                capability_hint: "<no-envelope>".into(),
                latency_class: "<no-envelope>".into(),
                sharding: "<no-envelope>".into(),
                context_tokens: None,
                max_output_tokens: None,
                preferred_speed: format!("{:?}", request.preferred_speed),
                explicit_model_id: request.model_id.clone(),
            },
        }
    }

    /// Stamp the response's `model_id` with a peer-attribution
    /// suffix so `ResponseProvenance.inference_backend` reads
    /// e.g. `Qwen3.5-9B.Q8_0 @ peer mac-peer`.
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
    /// - `Peer(peer, candidate, local_alternative)`: route there over
    ///   HTTP. The third field records WHY the peer was chosen —
    ///   because it is the only holder, or because step 3's tie-break
    ///   merely preferred it — and that is what decides whether a
    ///   peer failure may be served here instead. See
    ///   [`LocalAlternative`]; it is the load-balancing-vs-name-
    ///   resolution distinction in the paragraph above, made
    ///   structural rather than left implicit.
    /// - `Unknown`: no node in the mesh advertises the id. Caller
    ///   surfaces this as a clear error rather than falling back to
    ///   a different model.
    async fn locate_named_model(&self, model_id: &str) -> NamedModelLocation {
        let local_has = self
            .self_manifest
            .load()
            .models
            .iter()
            .any(|m| m.id == model_id);

        // Operator override: short-circuit all outbound peer
        // routing on this code path too. The OICP `select_peer`
        // path and this explicit-model-name path are independent
        // — the env var has to gate both, or a recipe that names
        // a specific GGUF (the common case for `enrich build`)
        // sneaks past. Returns Local when we have the model, else
        // Unknown (the caller surfaces a clear error rather than
        // silently falling through to a peer).
        if peer_inference_disabled() {
            tracing::debug!(
                model = %model_id,
                local_has,
                "mesh-inference: peer routing disabled via \
                 SOVEREIGN_DISABLE_PEER_INFERENCE — local or unknown"
            );
            return if local_has {
                NamedModelLocation::Local
            } else {
                NamedModelLocation::Unknown(NamedUnknownReason::PeerInferenceDisabled)
            };
        }

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
            return NamedModelLocation::Unknown(NamedUnknownReason::NotAdvertised);
        }

        // Pick the minimum in-flight peer (if any). Cheap O(n) since
        // peer fanout is small (≤ tens of nodes in practice).
        let best_peer = peer_candidates
            .into_iter()
            .min_by_key(|(_, _, inflight)| *inflight);

        match (local_has, best_peer) {
            (false, Some((peer, cand, _))) => {
                NamedModelLocation::Peer(peer, cand, LocalAlternative::SoleHolder)
            }
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
                    // We hold it too — the peer merely looked less
                    // busy. A peer failure here must not become a
                    // client-visible error (`LocalAlternative`).
                    NamedModelLocation::Peer(peer, cand, LocalAlternative::LocalHasIt)
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
            let manifest = match fetch {
                Some(m) => m.manifest,
                None => continue,
            };
            if let Some(model) = manifest.models.iter().find(|m| m.id == model_id) {
                // Pinned worker pods are scored with neutral affinity
                // (1.0) — see the carve-out in `select_peer`. Without
                // this the gather path would yield 0.0 for a pinned
                // pod's claim affinity (if the child's manifest didn't
                // populate one) and the candidate would silently drop
                // out of the load-balance comparison.
                let claim_affinity = if peer.transport.is_some() {
                    1.0
                } else {
                    model
                        .claims
                        .first()
                        .map(|c| c.effective_affinity())
                        .unwrap_or(0.0)
                };
                // Same gossip-override policy as `select_peer`: when
                // the peer publishes its self-reported in-flight,
                // trust it over our local view, which sees only
                // founder-originated dispatches. See
                // `sovereign/docs/MESH_LOAD_AWARENESS.md`.
                let self_observed = self
                    .peer_observations
                    .read()
                    .await
                    .get(&peer.name)
                    .map(|o| o.in_flight)
                    .unwrap_or(0);
                let peer_inflight = effective_peer_in_flight(self_observed, peer.current_in_flight);
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
    /// Decide the cascade of routes to attempt for a streaming
    /// request. Both `complete_stream_with_id` (legacy text-only) and
    /// `complete_stream_with_id_and_finish` (typed Finish) consume
    /// the same cascade; they differ only in WHICH inner method they
    /// invoke at each terminus. Mirrors the routing priority of the
    /// non-streaming `complete()` surface.
    ///
    /// Priority order:
    /// 1. **Explicit `model_id`** — if the request names a model and
    ///    it's local, return a single-step `[LocalNamed]` cascade.
    ///    If it's a peer's, return `[Peer{Hard}]` — failure is an
    ///    error, no fall-through. If nobody advertises it, return an
    ///    `Err` immediately.
    /// 2. **OICP-selected peer** — if `select_peer` picks a peer,
    ///    return `[Peer{Soft}, LocalFallback]` — peer failure
    ///    transparently falls through to local.
    /// 3. **Local fallback** — `[LocalFallback]`.
    ///
    /// The cascade-returning shape (rather than picking once) is the
    /// reason this method is non-trivial: peer failure is recoverable
    /// in the OICP case but not the explicit case, and the typed
    /// follow-up of "if peer failed, try local" has to be expressible
    /// in one return value so the caller can iterate without
    /// re-running `select_peer` (which is non-idempotent).
    /// The shared-model id this request should route into, if any.
    /// SLOT_POLICY §5: only an offload-eligible turn — `MeshAllowed`
    /// privacy AND a latency class that tolerates a hop (not `Fast`)
    /// — with no explicit `model_id` targets the configured shared
    /// model. An envelope-less, `LocalOnly`, or latency-`Fast`
    /// request never does. This shares the exact predicate the
    /// `select_peers_ranked` offload gate uses, so the shared-model
    /// path and the general peer-scoring path can't disagree about
    /// what "offloadable" means (the old code gated on the derived
    /// `Speed::Slow` shadow instead, which was the same drift the
    /// scoring gate carried).
    fn shared_primary_id(&self, request: &CompletionRequest) -> Option<String> {
        let offloadable = request
            .oicp
            .as_ref()
            .is_some_and(crate::oicp_select::offload_eligible);
        if !offloadable {
            return None;
        }
        self.shared_model_id.load_full().map(|s| (*s).clone())
    }

    /// THE decider for "where does a named request go": name resolution, the
    /// hop bound, and the decision record, in one place.
    ///
    /// Extracted 2026-08-06 because there were **two** implementations and only
    /// one was gated. `select_route` (streaming) called `locate_named_model`
    /// and applied the forward budget; `complete()` (non-streaming) called
    /// `locate_named_model` directly at its own site and applied nothing —
    /// no budget check, no decision record. Measured: an identical
    /// peer-routed request emitted 2 decision-log records with `"stream": true`
    /// and 0 with `"stream": false`.
    ///
    /// That is exactly the mistake M1's own retrospective recorded — "the first
    /// implementation was placed where the *architecture diagram* said requests
    /// are routed, not where they are actually routed" — repeated, because the
    /// bound was written into one call site instead of into the decider.
    /// ARCH_PRINCIPLES §10.6: a duplicated *decider* diverges into a plausible
    /// result with nothing red anywhere. Both callers now share this body, so
    /// the two cannot drift again.
    async fn resolve_named_dispatch(
        &self,
        request: &CompletionRequest,
        model_id: &str,
    ) -> NamedDispatch {
        // P1 on the named path. This record carries a verdict but
        // no scored candidates, and that is deliberate: named
        // dispatch is name resolution plus a min-in-flight
        // tiebreak (`locate_named_model`), not the OICP scorer,
        // so there is no `ScoreBreakdown` to report and inventing
        // a neutral one would pollute the §5 scoreboard with
        // decisions the scorer never made. What the record does
        // guarantee is that **every** outcome has a decision to
        // join back to, on both routing surfaces.
        let rec = DecisionBuilder::new(
            request
                .oicp
                .as_ref()
                .and_then(|o| o.request_id.as_deref())
                .unwrap_or(""),
            DecisionPath::NamedModel,
            Self::request_facts(request),
        );
        let located = self.locate_named_model(model_id).await;

        // THE NAMED PATH'S ONLY HOP BOUND. Named dispatch never reaches
        // `offload_verdict` — it is name resolution, not the OICP scorer —
        // so without this the forward budget bounds the scored path and
        // leaves this one open. That matters because this IS the
        // thin-client path: an IDE or any OpenAI client pins `model` and
        // carries no envelope, and `build_request` forwards the name
        // verbatim.
        //
        // The failure it closes is not hypothetical: `locate_named_model`
        // resolves against a 60s-cached manifest, so two nodes whose caches
        // each say "the other one has it" bounce a named request between
        // them until a client timeout.
        //
        // Downgrade rather than error: serving the model we were asked for
        // is strictly better than refusing, and `Unknown` is already the
        // path's honest "nobody has this" outcome (§18.3 — the substitution
        // is named in the trace, never silent).
        let may_forward = request.oicp.as_ref().is_none_or(|o| o.may_forward());

        // THE NAMED PATH'S PRIVACY BOUND (B2, measured 2026-08-06).
        //
        // Named dispatch never reaches `offload_verdict`, and `offload_verdict`
        // is where the privacy gate lived — so this path forwarded a
        // `local_only` envelope to a peer, contradicting BOTH this module's
        // rule 1 (see the header) and the forwarding-boundary gate in
        // `routes_inference.rs` (which cannot fire here: it sits at Priority 1,
        // *after* the Priority-0 local_inference provider that does the
        // forwarding). Measured: an envelope stating LocalOnly was served by a
        // peer, 200.
        //
        // That is the same mistake as the missing hop bound directly below —
        // a gate written into one call site instead of into the decider
        // (§10.6) — so it gets the same fix: it lives HERE, next to the budget,
        // in the one place both routing surfaces call.
        //
        // **Absent privacy counts as LocalOnly, and an absent ENVELOPE does
        // not.** `sharding()` defaults to LocalOnly because OICP §3.1 is
        // explicit that "privacy is the default, not something the client has
        // to remember to request" — so an envelope that does not say
        // `mesh_allowed` has not opted in. But a request with NO envelope at
        // all has stated nothing, and forcing it local would 503 every
        // thin-client request for a peer-only model — the exact shape M6-A
        // proved works and the reason this mesh is useful from a laptop.
        // Reading this module's rule 1 literally ("no OICP ... -> local") would
        // break that; the rule is stale for the named path.
        let privacy_permits_peer = request
            .oicp
            .as_ref()
            .is_none_or(|o| o.sharding() == sovereign_contracts::oicp::ShardingPrivacy::MeshAllowed);

        let located = match located {
            // ORDER MATTERS, and it is budget-then-privacy. A FORWARDED
            // request carries the budget-only envelope `oicp-client` stamps
            // (`lib.rs`, the named branch), whose privacy field is ABSENT and
            // therefore reads as LocalOnly — so a privacy-first ordering
            // reports "you asked for local_only" at a request whose real story
            // is "some node already forwarded this once". That is exactly the
            // B1 misattribution class, re-introduced one gate over; caught by
            // `non_streaming_named_dispatch_refuses_to_forward_an_exhausted_request`
            // when this was written the other way round.
            //
            // A spent budget is a definite fact about this request's history.
            // Privacy-by-default is an absence. Report the fact first.
            NamedModelLocation::Peer(..) if !may_forward => {
                let local_has = self
                    .self_manifest
                    .load()
                    .models
                    .iter()
                    .any(|m| m.id == model_id);
                tracing::debug!(
                    model = %model_id,
                    gate = "forward_budget_exhausted",
                    local_has,
                    "mesh-inference: already forwarded once — will not forward \
                     a named request again"
                );
                if local_has {
                    NamedModelLocation::Local
                } else {
                    // NOT `NotAdvertised`: a peer demonstrably advertises this
                    // id — that is why `located` was `Peer` a line ago. Saying
                    // otherwise is B1, the whole reason this reason exists.
                    NamedModelLocation::Unknown(NamedUnknownReason::ForwardBudgetExhausted)
                }
            }
            // The privacy arm is SECOND on purpose (see above): a spent budget
            // outranks an unstated privacy field as an explanation.
            NamedModelLocation::Peer(..) if !privacy_permits_peer => {
                let local_has = self
                    .self_manifest
                    .load()
                    .models
                    .iter()
                    .any(|m| m.id == model_id);
                tracing::debug!(
                    model = %model_id,
                    gate = "privacy_local_only",
                    local_has,
                    "mesh-inference: envelope says local_only — will not carry \
                     a named request across the trust boundary"
                );
                if local_has {
                    // Serving it here honours LocalOnly exactly. Not a
                    // substitution: same model, no boundary crossed.
                    NamedModelLocation::Local
                } else {
                    NamedModelLocation::Unknown(NamedUnknownReason::PrivacyLocalOnly)
                }
            }
            other => other,
        };

        let verdict = match &located {
            NamedModelLocation::Local => Verdict::NamedLocal {
                model_id: model_id.to_string(),
            },
            NamedModelLocation::Peer(peer, cand, _) => Verdict::NamedPeer {
                peer: peer.name.clone(),
                model_id: cand.model_id.clone(),
            },
            NamedModelLocation::Unknown(_) => Verdict::NamedUnknown {
                model_id: model_id.to_string(),
            },
        };
        let decision = rec.finish(verdict, &[]);
        let decision_id = decision.decision_id.clone();
        let oicp_request_id = decision.oicp_request_id.clone();
        decision_log::emit_decision(&self.decision_sink, decision);
        NamedDispatch {
            located,
            decision_id,
            oicp_request_id,
        }
    }

    async fn select_route(&self, request: &CompletionRequest) -> Result<RoutePlan> {
        // Effective named target: an explicit `model_id` (Hard — fail loud if no
        // node advertises it) takes priority; otherwise a configured shared-model
        // primary (Soft — degrade to the local model when the cluster is forming
        // or the host is unreachable).
        let (named, soft) = match explicit_model_id(request) {
            Some(id) => (Some(id.to_string()), false),
            None => (self.shared_primary_id(request), true),
        };
        if let Some(model_id) = named {
            let NamedDispatch {
                located,
                decision_id,
                oicp_request_id,
            } = self.resolve_named_dispatch(request, &model_id).await;
            let plan = |steps| RoutePlan {
                steps,
                decision_id: decision_id.clone(),
                oicp_request_id: oicp_request_id.clone(),
            };
            match located {
                NamedModelLocation::Local => {
                    tracing::info!(model = %model_id, soft, "mesh-inference: routing locally by model name");
                    let guard = self.enter_local_inflight(&model_id);
                    Ok(plan(vec![RouteDecision::LocalNamed {
                        attribution: model_id,
                        guard,
                    }]))
                }
                NamedModelLocation::Peer(peer, peer_cand, local_alt) => {
                    tracing::info!(
                        peer = %peer.name,
                        addrs = peer.base_urls.len(),
                        model = %peer_cand.model_id,
                        soft,
                        local_alternative = ?local_alt,
                        "mesh-inference: routing to peer by model name"
                    );
                    let ledger = self
                        .mesh
                        .ledger_emission_for(&peer.node_id, &peer_cand.model_id, &peer.name)
                        .await;
                    if soft {
                        // Shared-model primary: prefer the host, but fall back to
                        // the local model if every address fails — the cascade's
                        // existing LocalFallback step (degraded, not an error).
                        let total = self.enter_local_total();
                        Ok(plan(vec![
                            RouteDecision::Peer {
                                peer,
                                peer_cand,
                                ledger,
                                disposition: PeerFailureDisposition::Soft,
                                pinned_model_id: Some(model_id.clone()),
                            },
                            RouteDecision::LocalFallback { total },
                        ]))
                    } else if local_alt == LocalAlternative::LocalHasIt {
                        // We advertise this id too; the balancer only
                        // preferred the peer because our in-flight
                        // count was higher. So a peer failure is Soft
                        // and the cascade continues into OUR copy of
                        // the SAME model — not a substitution, and so
                        // not the case `Hard` exists to protect
                        // (§18.3). `LocalNamed` keeps the attribution
                        // as the caller wrote it.
                        let guard = self.enter_local_inflight(&model_id);
                        Ok(plan(vec![
                            RouteDecision::Peer {
                                peer,
                                peer_cand,
                                ledger,
                                disposition: PeerFailureDisposition::Soft,
                                pinned_model_id: Some(model_id.clone()),
                            },
                            RouteDecision::LocalNamed {
                                attribution: model_id,
                                guard,
                            },
                        ]))
                    } else {
                        Ok(plan(vec![RouteDecision::Peer {
                            peer,
                            peer_cand,
                            ledger,
                            pinned_model_id: Some(model_id.clone()),
                            disposition: PeerFailureDisposition::Hard { model_id },
                        }]))
                    }
                }
                NamedModelLocation::Unknown(reason) => {
                    if soft {
                        // Fall THROUGH to ranked mesh selection, not
                        // straight to this node's own model. A soft
                        // named target is a preference, not a
                        // constraint — and the household that stood up
                        // a shared 122B is exactly the household that
                        // also has a 35B hub on the LAN. Dropping a 4B
                        // laptop to its own 4B while that hub sits
                        // free is a pure loss: no latency is bought,
                        // no privacy is honoured, and the user sees a
                        // markedly worse answer for no reason.
                        //
                        // The `NamedModel` record above already
                        // recorded that `model_id` resolved to nobody;
                        // the plan below joins its outcome to the
                        // FALLTHROUGH record, because the ranked
                        // scorer is what actually picks the server.
                        // The two share an `oicp_request_id`, so the
                        // pair reads as one story.
                        // `reason` is on this line because the fallthrough is
                        // NOT always "forming/unavailable": a hop-exhausted
                        // soft target lands here too, and the ranked scorer it
                        // falls through to has its OWN forward-budget gate
                        // (`oicp_select::offload_verdict`), so the request is
                        // still bounded — but an operator reading only this
                        // line would not know which of the two happened.
                        tracing::info!(
                            shared = %model_id,
                            reason = ?reason,
                            "mesh-inference: shared model unavailable on the \
                             named path — falling through to ranked mesh selection"
                        );
                        Ok(self
                            .ranked_route_plan(request, DecisionPath::NamedFallthrough)
                            .await)
                    } else {
                        // C2 (measured 2026-08-06): this arm used to return
                        // `Err` bare, so a STREAMING refusal emitted a
                        // `NamedUnknown` decision with no outcome to join to —
                        // three consecutive refusals in the M6-C run produced
                        // three orphan decisions. The non-streaming sibling
                        // already did this ("a refusal is a verdict, not a gap
                        // in the record"); `NamedDispatch`'s own doc says every
                        // named resolution INCLUDING Unknown needs an outcome.
                        // The streaming surface simply never honoured it.
                        //
                        // A refusal is exactly the record an operator greps for,
                        // so an un-joined decision is worst precisely when it
                        // matters most.
                        let msg = reason.refusal(&model_id);
                        self.outcome_ctx(
                            &decision_id,
                            &oicp_request_id,
                            ServedBy::Failed,
                            0,
                            &[],
                        )
                        .failed(msg.clone(), false);
                        Err(sovereign_core::error::Error::ModelNotLoaded(msg))
                    }
                }
            }
        } else {
            Ok(self
                .ranked_route_plan(request, DecisionPath::RankedOicp)
                .await)
        }
    }

    /// Ranked OICP failover as a route plan: one Soft `Peer` step per
    /// peer that strictly beats local, best-first, then
    /// `LocalFallback`. The cascade loop tries each in order — a 503 /
    /// transport failure on the best peer fails over to the NEXT peer
    /// (Soft `continue`) instead of collapsing straight to local.
    /// `enter_local_total` stays eager so the gossip publisher sees the
    /// (possible) local load on the same timing it always did, before
    /// any peer round-trip decides.
    ///
    /// Two callers, distinguished by `path` alone: the ordinary ranked
    /// route, and the soft-named fallthrough (a configured shared
    /// model that nobody in the mesh is currently serving). Sharing
    /// the assembly is the point — a fallthrough that built its own
    /// cascade would be free to drift from the one production ranks.
    async fn ranked_route_plan(
        &self,
        request: &CompletionRequest,
        path: DecisionPath,
    ) -> RoutePlan {
        let RankedSelection {
            peers: ranked,
            decision_id,
            oicp_request_id,
        } = self.select_peers_ranked(request, path).await;
        let total = self.enter_local_total();
        if ranked.is_empty() {
            tracing::debug!(
                ?path,
                "mesh-inference: no peer beat local — serving locally"
            );
        } else {
            tracing::info!(
                ?path,
                peers = ranked.len(),
                "mesh-inference: routing to peer(s) by OICP selection (ranked failover)"
            );
        }
        let mut steps = Vec::with_capacity(ranked.len() + 1);
        for (peer, peer_cand) in ranked {
            let ledger = self
                .mesh
                .ledger_emission_for(&peer.node_id, &peer_cand.model_id, &peer.name)
                .await;
            steps.push(RouteDecision::Peer {
                peer,
                peer_cand,
                ledger,
                disposition: PeerFailureDisposition::Soft,
                // Ranked/OICP: the peer picks from the envelope.
                pinned_model_id: None,
            });
        }
        steps.push(RouteDecision::LocalFallback { total });
        RoutePlan {
            steps,
            decision_id,
            oicp_request_id,
        }
    }

    /// `complete_stream()`).
    ///
    /// Also bumps the gossiped total counter (`in_flight_publisher`)
    /// so peers see the load. The two counters are kept in lock-step
    /// by composition: the returned guard *contains* a
    /// `LocalTotalGuard` whose Drop runs alongside the HashMap
    /// decrement.
    fn enter_local_inflight(&self, model_id: &str) -> LocalInflightGuard {
        let total = self.enter_local_total();
        let mut map = self
            .local_inflight_by_model
            .lock()
            .expect("local_inflight_by_model poisoned");
        *map.entry(model_id.to_string()).or_insert(0) += 1;
        LocalInflightGuard {
            counter: Arc::clone(&self.local_inflight_by_model),
            model_id: model_id.to_string(),
            _total: total,
        }
    }

    /// Increment the gossiped total in-flight counter and return a
    /// drop-guard that decrements on Drop. Used by every local-serve
    /// dispatch path that *isn't* already going through
    /// [`enter_local_inflight`] (which composes this guard inside).
    /// Currently that's the OICP-routed fallback-to-local arm: when
    /// the selector chose a peer but every address failed and we
    /// served locally, the peer-side counters got decremented but
    /// nothing bumped the local view. This guard plugs that path.
    ///
    /// Saturating subtract in Drop — the counter never underflows
    /// even if a refactor lands an unbalanced inc/dec pair.
    fn enter_local_total(&self) -> LocalTotalGuard {
        self.in_flight_publisher.fetch_add(1, Ordering::Relaxed);
        LocalTotalGuard {
            publisher: Arc::clone(&self.in_flight_publisher),
        }
    }
}

/// RAII guard for the per-model local in-flight counter. Decrements
/// the counter in `Drop`; safe to drop after the entry has been
/// pruned to zero (saturating subtract + no-op when absent).
///
/// Composes a [`LocalTotalGuard`] in `_total` so the gossiped
/// publisher decrements in lock-step. Rust's struct-field drop order
/// (declaration order) means the HashMap-entry decrement runs
/// before `_total`'s Drop fires — readers that race the decrement
/// see "either both committed or neither has", never "publisher
/// decremented while HashMap still high."
struct LocalInflightGuard {
    counter: Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
    model_id: String,
    _total: LocalTotalGuard,
}

impl Drop for LocalInflightGuard {
    fn drop(&mut self) {
        let Ok(mut map) = self.counter.lock() else {
            return;
        };
        if let Some(v) = map.get_mut(&self.model_id) {
            *v = v.saturating_sub(1);
            if *v == 0 {
                map.remove(&self.model_id);
            }
        }
    }
}

/// RAII guard for the gossiped total in-flight counter. Saturating
/// subtract in Drop — the counter is correctness-best-effort for
/// scoring purposes, not load-bearing for correctness, so we never
/// want a bug to underflow it to `u32::MAX`.
struct LocalTotalGuard {
    publisher: Arc<AtomicU32>,
}

impl Drop for LocalTotalGuard {
    fn drop(&mut self) {
        // Compare-exchange loop because `fetch_sub` would underflow
        // on a hypothetical unbalanced drop. The counter starts at 0
        // and every `enter_local_total` bumps it by 1 before yielding
        // the guard, so the only way to reach 0 with a live guard is
        // a logic bug — saturate rather than wrap.
        let mut cur = self.publisher.load(Ordering::Relaxed);
        loop {
            let new = cur.saturating_sub(1);
            match self.publisher.compare_exchange_weak(
                cur,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }
}

/// Where an explicitly-named `request.model_id` lives in the mesh.
/// Disposition for a `Peer` route on transport failure. Named-peer
/// routes (explicit `model_id` matched a peer) are *hard* — failing
/// to reach the named peer is a routing error the caller must see.
/// OICP-selected peer routes are *soft* — falling through to the
/// local model is the correct behaviour. Each disposition shapes the
/// error path inside the route-cascade loop in
/// `complete_stream_with_id{,_and_finish}`.
#[derive(Clone)]
enum PeerFailureDisposition {
    /// Explicit named-peer route — when every base_url fails, return
    /// a `Routing` error naming the model and peer.
    Hard { model_id: String },
    /// OICP-selected route — when every base_url fails, fall through
    /// to the next [`RouteDecision`] in the cascade (typically
    /// `LocalFallback`). This is the mac-peer → Taiwan-pod recovery
    /// path the mesh-routing design was built around.
    Soft,
}

/// One step in a routing cascade. `select_route` returns a `Vec` of
/// these, ordered first-try → last-fallback. Both
/// `complete_stream_with_id` (legacy text stream) and
/// `complete_stream_with_id_and_finish` (typed Finish stream) iterate
/// the same cascade, terminating on the first success. Routing logic
/// lives in `select_route` only; per-method code is responsible for
/// constructing the appropriate stream type per terminus (legacy:
/// `complete_stream`, typed: `complete_stream_with_finish`).
///
/// The guards (`LocalInflightGuard` / `LocalTotalGuard`) move into
/// the stream wrapper at construction so the Drop side decrements
/// counters when the stream lifetime ends — same lifetime discipline
/// as before the extraction, just sourced from one place.
enum RouteDecision {
    /// Serve locally with an inflight guard tied to the named model.
    /// `attribution` is the model name as the caller asked for it —
    /// echoed back so `ResponseProvenance.inference_backend` reads
    /// "qwopus-3.5-9B" rather than the slot-derived label.
    LocalNamed {
        attribution: String,
        guard: LocalInflightGuard,
    },
    /// Serve via peer; iterate `peer.base_urls` and apply
    /// `disposition` if every URL fails.
    Peer {
        peer: PeerInferenceEndpoint,
        peer_cand: ModelCandidate,
        ledger: Option<LedgerEmission>,
        disposition: PeerFailureDisposition,
        /// Model id to PIN on the outgoing request, when this route
        /// came from resolving a NAME (an explicit `model_id`, or a
        /// configured shared primary). `None` on ranked/OICP routes,
        /// where the whole point is that the peer selects its own
        /// best model from the envelope.
        ///
        /// Not cosmetic: a peer that resolves models strictly REFUSES
        /// a request naming nothing, so an unpinned named route is
        /// answered with a refusal and the cascade then collapses to
        /// local — the caller silently gets this node's model instead
        /// of the one they named. The non-streaming body used to pin
        /// this by rewriting the request up front; carrying it on the
        /// step is what lets both surfaces do it from one decision.
        pinned_model_id: Option<String>,
    },
    /// Local fallback — total-counter guard (no per-model accounting
    /// because the request didn't name a model).
    LocalFallback { total: LocalTotalGuard },
}

/// Returned by [`MeshInferenceProvider::locate_named_model`]; see
/// that method for the contract this enum encodes.
#[derive(Debug)]
enum NamedModelLocation {
    /// Our own `self_manifest` advertises this model id. The local
    /// provider's slot picker will route the request into the
    /// matching slot — no further metadata needed at this layer.
    Local,
    /// A peer's manifest advertises this model id. The third field
    /// says whether OURS does too — see [`LocalAlternative`], which
    /// decides what a peer failure is allowed to mean.
    Peer(PeerInferenceEndpoint, ModelCandidate, LocalAlternative),
    /// This node will not dispatch the id — for one of
    /// [`NamedUnknownReason`]'s reasons, only ONE of which is
    /// "the mesh does not have it".
    Unknown(NamedUnknownReason),
}

/// When a peer route was chosen for an explicitly-named model: does
/// THIS node advertise the same id?
///
/// It exists because `NamedModelLocation::Peer` was answering two
/// materially different questions with one shape, and the difference
/// decides whether a peer failure may fall back:
///
/// - `SoleHolder` — only the peer has it. Falling back would serve a
///   DIFFERENT model than the caller named, which is precisely the
///   silent substitution §18.3 forbids. Fail loud.
/// - `LocalHasIt` — both hold it, and `locate_named_model`'s
///   load-balance rule merely preferred the peer because our own
///   in-flight count was higher. Falling back serves exactly what was
///   asked for, on the node that was always able to serve it.
///
/// Measured 2026-08-06, and this is why the distinction is not
/// cosmetic: once M5 piece 3 stamped `X-Node-Id`, peers began
/// shedding, and four of five load-balanced turns for `primary`
/// returned a hard 503 to the client while `primary` was loaded here
/// and answering in 1.57 s. Nothing had substituted anything — the
/// code simply could not tell the two cases apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalAlternative {
    /// This node advertises the same id; a peer failure may fall back.
    LocalHasIt,
    /// The peer is the only holder; a peer failure is terminal.
    SoleHolder,
}

/// Why a named model could not be dispatched.
///
/// A closed set, so an enum rather than a string (ARCH_PRINCIPLES §2).
/// It exists because three distinct causes used to collapse into one
/// refusal that named only the first — measured 2026-08-06 as M6-B
/// finding B1: a hop-exhausted request was told "no node in this mesh
/// advertises model X — check `/v1/models`", while a peer both
/// advertised it and had served it 20 ms earlier. An operator who
/// followed that instruction found the model listed and had nowhere
/// left to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedUnknownReason {
    /// No node's manifest carries the id — the honest original meaning.
    NotAdvertised,
    /// A peer DOES advertise it, but this request arrived already
    /// forwarded and its hop budget is spent, so forwarding again
    /// would risk the A→B→A bounce the budget exists to bound.
    /// Set only by the downgrade in [`Self::refusal`]'s caller
    /// `resolve_named_dispatch`.
    ForwardBudgetExhausted,
    /// A peer may advertise it, but `SOVEREIGN_DISABLE_PEER_INFERENCE`
    /// forbids this node from looking outward at all.
    PeerInferenceDisabled,
    /// A peer advertises it, but the request's envelope says
    /// `sharding = local_only`, so serving it would carry the prompt
    /// across the trust boundary. Refusing is the contract (§18.3:
    /// refuse, never substitute) — see the B2 gate in
    /// `resolve_named_dispatch`.
    PrivacyLocalOnly,
}

impl NamedUnknownReason {
    /// THE operator-facing refusal text. One renderer, called from both
    /// refusal sites — the streaming `select_route` and the
    /// non-streaming `complete()` — because two copies of this message
    /// already existed and drifting them is how B1 stayed invisible
    /// (§10.6: one decider, one name).
    ///
    /// Each arm names what the operator should do NEXT, and no arm
    /// sends them somewhere the evidence contradicts.
    fn refusal(self, model_id: &str) -> String {
        match self {
            Self::NotAdvertised => format!(
                "no node in this mesh advertises model '{model_id}' — \
                 check `/v1/models` for available names"
            ),
            Self::ForwardBudgetExhausted => format!(
                "model '{model_id}' is advertised by a peer, but this request \
                 has already been forwarded once and its mesh hop budget is \
                 spent — a further forward could bounce between nodes with \
                 stale manifests. Raise `forward_budget` in the OICP envelope \
                 to allow another hop, or send the request to a node that \
                 holds the model"
            ),
            Self::PeerInferenceDisabled => format!(
                "model '{model_id}' is not loaded on this node and peer \
                 routing is disabled by SOVEREIGN_DISABLE_PEER_INFERENCE — \
                 a peer may well advertise it. Unset that variable to route \
                 across the mesh, or load the model here"
            ),
            Self::PrivacyLocalOnly => format!(
                "model '{model_id}' is advertised by a peer, but this request's \
                 OICP envelope says privacy 'local_only', so serving it would \
                 send the prompt off this machine. Set \
                 `privacy.sharding = \"mesh_allowed\"` to permit that, or load \
                 the model on this node"
            ),
        }
    }
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

/// The request to actually send to a peer for one route step.
///
/// Returns the caller's request untouched unless the step carries a
/// `pinned_model_id` that differs from what the request already
/// names, in which case an owned copy carries the resolved id. One
/// body, three callers (`complete` + the two streaming entry points),
/// because "which model goes on the wire" is a routing decision and a
/// third hand-written copy of it is exactly how the surfaces drifted
/// apart in the first place (§10.6).
fn pinned_request<'a>(
    request: &'a CompletionRequest,
    pinned_model_id: Option<&str>,
) -> std::borrow::Cow<'a, CompletionRequest> {
    match pinned_model_id {
        Some(id) if request.model_id.as_deref() != Some(id) => {
            std::borrow::Cow::Owned(CompletionRequest {
                model_id: Some(id.to_string()),
                ..request.clone()
            })
        }
        _ => std::borrow::Cow::Borrowed(request),
    }
}

/// Build the per-peer `RemoteApiProvider` for one routing attempt.
///
/// Branches on `peer.transport`:
/// - `None` (default mesh peer): plain-HTTP client, no bearer.
/// - `Some(t)` (pinned worker pod): TLS-pinned client + owner-signed
///   `WorkerToken` bearer. Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md.
///
/// One call site per branch — every place in this file that hits a
/// peer over HTTP goes through here, so the pinned-pod carve-out
/// can't accidentally regress when a new routing path is added.
///
/// `local_node_id_hex` is M5 piece 3 and it is the whole of it. Every
/// provider built here is BY CONSTRUCTION talking to a mesh peer, so
/// this is the one place in the workspace that can honestly say so —
/// which is why the stamp belongs here and not at the four routing
/// call sites, where a fifth would be added without it.
///
/// Passing `None` is not neutral. It makes this node's forwarded
/// turns indistinguishable from the peer's own user typing, so the
/// peer's pause, foreground yield and `max_peer_inflight` ceiling all
/// stay dark — the exact state M5's experiment measured on
/// 2026-08-06, where four concurrent peer requests serialized to
/// 6.41 s with `peer_inflight_current` never leaving 0. `None`
/// therefore means "we do not know who we are", never "don't bother".
fn provider_for_peer(
    peer: &PeerInferenceEndpoint,
    url: &str,
    local_node_id_hex: Option<&str>,
) -> RemoteApiProvider {
    const PEER_CONTEXT: u32 = 32_768;
    // `"mesh-peer"` is an attribution label, not a servable model name —
    // it exists so logs and `CompletionResponse::model_id` can say a turn
    // left this node. `with_placeholder_model_id` is what keeps it off
    // the wire: sent as `model`, it puts the receiving node on its
    // explicit-name path, resolves to nobody, and 503s.
    let provider = match &peer.transport {
        Some(t) => RemoteApiProvider::with_client_and_bearer(
            url,
            t.client.clone(),
            t.bearer.clone(),
            "mesh-peer",
            PEER_CONTEXT,
        )
        .with_placeholder_model_id(),
        None => RemoteApiProvider::new(url, None, "mesh-peer", PEER_CONTEXT)
            .with_placeholder_model_id(),
    };
    match local_node_id_hex {
        Some(hex) => provider.with_node_id(hex),
        None => {
            tracing::debug!(
                peer = %peer.name,
                "mesh-inference: forwarding UNSTAMPED — this node's id is unknown, \
                 so the peer will admit this turn as its own local traffic"
            );
            provider
        }
    }
}

#[async_trait]
impl InferenceProvider for MeshInferenceProvider {
    /// Non-streaming completion, driven by the SAME `select_route`
    /// cascade the two streaming entry points use.
    ///
    /// Until 2026-08-07 this method resolved its own route inline: its
    /// own shared-primary rewrite, its own named dispatch, its own
    /// single-peer ranked pick. That is why FOUR separate features —
    /// the forward budget, the privacy gate, the outcome join, and the
    /// `LocalAlternative` fallback — each had to be written twice to
    /// reach both surfaces, the last of them as a same-day regression
    /// fix. The routing DECISION now has exactly one implementation.
    /// What remains per-method is only how a step's terminus is built,
    /// which is genuinely different: a response here, a stream there.
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let RoutePlan {
            steps,
            decision_id,
            oicp_request_id,
        } = self.select_route(request).await?;
        let mut failovers: Vec<decision_log::FailoverAttempt> = Vec::new();
        let mut last_err: Option<sovereign_core::error::Error> = None;

        for (attempt_index, step) in steps.into_iter().enumerate() {
            let attempt_index = attempt_index as u32;
            match step {
                RouteDecision::LocalNamed { attribution, guard } => {
                    return self
                        .complete_named_locally(
                            request,
                            &attribution,
                            guard,
                            &decision_id,
                            &oicp_request_id,
                            attempt_index,
                            &failovers,
                        )
                        .await;
                }
                RouteDecision::Peer {
                    peer,
                    peer_cand,
                    ledger,
                    disposition,
                    pinned_model_id,
                } => {
                    let serve_request = pinned_request(request, pinned_model_id.as_deref());
                    let serve_request = serve_request.as_ref();
                    // The contribution ledger is emitted from the STREAM
                    // wrapper's lifecycle and has never had a non-streaming
                    // equivalent. Left off deliberately rather than quietly
                    // switched on: emitting here would start booking
                    // contribution for traffic that has never been booked,
                    // which is a ledger-visible change and belongs in its
                    // own commit with its own note.
                    let _ = ledger;

                    // Raise the peer's observed in-flight count BEFORE
                    // handing off. `locate_named_model`'s load-balance rule
                    // and the ranked scorer both read it, so without this
                    // every concurrent caller sees `peer_inflight = 0` and
                    // floods one peer. The named branch did this; the ranked
                    // branch did not. Uniform now — see the commit note.
                    self.record_dispatch(Some(&peer.name)).await;
                    let started = Instant::now();
                    let mut last_transport_err: Option<String> = None;
                    let node_id_hex = self.local_node_id_hex().await;
                    for url in &peer.base_urls {
                        let rp = provider_for_peer(&peer, url, node_id_hex.as_deref());
                        match rp.complete(serve_request).await {
                            Ok(mut resp) => {
                                // Prefer the peer's OICP-advertised model id
                                // over whatever label the wire response
                                // carried — the advertised id is what the
                                // selector actually scored, so attribution
                                // should match it. (Some backends echo a
                                // request hint instead of the served model.)
                                resp.model_id = peer_cand.model_id.clone();
                                self.peer_health.record_success(&peer.name);
                                self.record_success(Some(&peer.name)).await;
                                self.outcome_ctx(
                                    &decision_id,
                                    &oicp_request_id,
                                    ServedBy::Peer {
                                        name: peer.name.clone(),
                                        node_id: Some(peer.node_id.to_hex()),
                                        model_id: peer_cand.model_id.clone(),
                                    },
                                    attempt_index,
                                    &failovers,
                                )
                                .complete(
                                    None,
                                    Some(started.elapsed().as_secs_f64() * 1000.0),
                                    None,
                                );
                                return Ok(Self::annotate(resp, &peer.name));
                            }
                            Err(e) => {
                                tracing::info!(
                                    peer = %peer.name,
                                    url = %url,
                                    error = %e,
                                    "mesh-inference: peer complete() transport error, \
                                     trying next address"
                                );
                                last_transport_err = Some(format!("{e}"));
                            }
                        }
                    }
                    // Every address for this peer failed. One failure per
                    // PEER, not per address — a peer is unreachable as a
                    // unit.
                    let err_text = last_transport_err.unwrap_or_else(|| "unreachable".into());
                    let shed = decision_log::looks_shed(&err_text);
                    self.book_peer_failure(&peer.name, &err_text, shed);
                    self.record_failure(Some(&peer.name)).await;
                    failovers.push(decision_log::FailoverAttempt {
                        peer: peer.name.clone(),
                        error: err_text.clone(),
                        shed,
                    });
                    match disposition {
                        PeerFailureDisposition::Hard { model_id } => {
                            // Terminal: the peer is the only holder, so no
                            // later step can serve the name that was asked
                            // for. This is where the join closes.
                            self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::Failed,
                                attempt_index,
                                &failovers,
                            )
                            .failed(err_text.clone(), shed);
                            return Err(sovereign_core::error::Error::Routing(format!(
                                "model '{}' is advertised by peer '{}' but all peer \
                                 addresses failed: {}",
                                model_id, peer.name, err_text
                            )));
                        }
                        PeerFailureDisposition::Soft => {
                            tracing::info!(
                                peer = %peer.name,
                                shed,
                                "mesh-inference: peer step failed, continuing the cascade"
                            );
                            last_err =
                                Some(sovereign_core::error::Error::Routing(err_text.clone()));
                        }
                    }
                }
                RouteDecision::LocalFallback { total } => {
                    // `total` is the eagerly-entered gossip counter: this
                    // node is about to produce load and peers must see it.
                    let _total = total;
                    let started = Instant::now();
                    let result = self.local.complete(request).await;
                    // `failovers` is what distinguishes the two flows that
                    // arrive here: the selector chose nobody (a `stay_local`
                    // decision — still a decision, empty list), or peers were
                    // tried and failed. `ttft_ms` is None by construction —
                    // there is no stream to time a first token against, and
                    // reading one off a non-streaming call would be a
                    // fabrication.
                    let ctx = self.outcome_ctx(
                        &decision_id,
                        &oicp_request_id,
                        ServedBy::LocalFallback {
                            model_id: self.local.model_id_for(request.preferred_speed),
                        },
                        attempt_index,
                        &failovers,
                    );
                    match &result {
                        Ok(_) => {
                            ctx.complete(None, Some(started.elapsed().as_secs_f64() * 1000.0), None)
                        }
                        Err(e) => ctx.failed(e.to_string(), false),
                    }
                    return result;
                }
            }
        }

        // Only reachable if a plan ended without a serving step, which
        // `select_route` does not currently produce. Reported rather
        // than unwrapped so a future plan shape cannot panic here.
        Err(last_err.unwrap_or_else(|| {
            sovereign_core::error::Error::Routing(
                "the route plan ended with no step able to serve".into(),
            )
        }))
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

    /// Streaming + attribution in one call. Cascade-driven via
    /// `select_route` (shared with `complete_stream_with_id_and_finish`),
    /// keeping the routing logic in one place. Each `RouteDecision`
    /// step constructs the appropriate stream via
    /// `self.local.complete_stream` / `rp.complete_stream`; the typed
    /// sibling method swaps in `complete_stream_with_finish` at the
    /// same step boundaries.
    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        let RoutePlan {
            steps,
            decision_id,
            oicp_request_id,
        } = self.select_route(request).await?;
        let mut last_err: Option<sovereign_core::error::Error> = None;
        let mut failovers: Vec<decision_log::FailoverAttempt> = Vec::new();
        for (attempt_index, step) in steps.into_iter().enumerate() {
            let attempt_index = attempt_index as u32;
            match step {
                RouteDecision::LocalNamed { attribution, guard } => {
                    let stream = self.local.complete_stream(request).await?;
                    let observed: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
                        Box::pin(InflightGuardedStream::new(
                            ThroughputObservedStream::new(
                                stream,
                                ThroughputTarget::Local(Arc::clone(&self.local_observations)),
                            )
                            .with_outcome(self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::Local {
                                    model_id: attribution.clone(),
                                },
                                attempt_index,
                                &failovers,
                            )),
                            guard,
                        ));
                    return Ok((observed, attribution));
                }
                RouteDecision::Peer {
                    peer,
                    peer_cand,
                    ledger,
                    disposition,
                    pinned_model_id,
                } => {
                    let serve_request = pinned_request(request, pinned_model_id.as_deref());
                    let serve_request = serve_request.as_ref();
                    let mut last_transport_err: Option<String> = None;
                    let node_id_hex = self.local_node_id_hex().await;
                    for url in &peer.base_urls {
                        let rp = provider_for_peer(&peer, url, node_id_hex.as_deref());
                        match rp.complete_stream(serve_request).await {
                            Ok(stream) => {
                                let attribution =
                                    format!("{} @ peer {}", peer_cand.model_id, peer.name);
                                let mut wrapper = ThroughputObservedStream::new(
                                    stream,
                                    ThroughputTarget::Peer {
                                        name: peer.name.clone(),
                                        map: Arc::clone(&self.peer_observations),
                                    },
                                )
                                .with_outcome(self.outcome_ctx(
                                    &decision_id,
                                    &oicp_request_id,
                                    ServedBy::Peer {
                                        name: peer.name.clone(),
                                        node_id: Some(peer.node_id.to_hex()),
                                        model_id: peer_cand.model_id.clone(),
                                    },
                                    attempt_index,
                                    &failovers,
                                ));
                                if let Some(em) = ledger.clone() {
                                    wrapper = wrapper.with_ledger_emission(em);
                                }
                                let observed: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
                                    Box::pin(wrapper);
                                self.peer_health.record_success(&peer.name);
                                // INVARIANT (no double-emit): once we return the
                                // peer's LIVE stream here, the routing cascade is
                                // over. A failure that surfaces *mid-stream* (the
                                // peer dies after ≥1 token) must NOT re-enter the
                                // cascade or restart locally — the client would
                                // then see duplicated / garbled output. The
                                // ranked failover below only retries from the
                                // pre-`Ok` `Err` arm (connect / headers / 503),
                                // never from a stream already handed out. Pinned
                                // by `peer_dies_mid_stream_does_not_duplicate`.
                                return Ok((observed, attribution));
                            }
                            Err(e) => {
                                tracing::info!(
                                    peer = %peer.name,
                                    url = %url,
                                    error = %e,
                                    "mesh-inference: peer transport error, trying next address"
                                );
                                last_transport_err = Some(format!("{e}"));
                            }
                        }
                    }
                    let step_err = last_transport_err
                        .clone()
                        .unwrap_or_else(|| "unreachable".into());
                    let shed = decision_log::looks_shed(&step_err);
                    self.book_peer_failure(&peer.name, &step_err, shed);
                    failovers.push(decision_log::FailoverAttempt {
                        peer: peer.name.clone(),
                        error: step_err.clone(),
                        shed,
                    });
                    match disposition {
                        PeerFailureDisposition::Hard { model_id } => {
                            // Terminal: no further step will serve, so
                            // this is where the join closes.
                            self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::Failed,
                                attempt_index,
                                &failovers,
                            )
                            .failed(step_err, shed);
                            return Err(sovereign_core::error::Error::Routing(format!(
                                "model '{}' is advertised by peer '{}' but all peer \
                                 addresses failed: {}",
                                model_id,
                                peer.name,
                                last_transport_err.unwrap_or_else(|| "unreachable".into())
                            )));
                        }
                        PeerFailureDisposition::Soft => {
                            tracing::info!(
                                peer = %peer.name,
                                "mesh-inference: all peer addresses failed, falling through to next route"
                            );
                            // Record err for diagnostic if even the
                            // local fallback subsequently fails; not
                            // surfaced unless cascade exhausts.
                            last_err =
                                last_transport_err.map(sovereign_core::error::Error::Inference);
                            continue;
                        }
                    }
                }
                RouteDecision::LocalFallback { total } => {
                    let stream = self.local.complete_stream(request).await?;
                    let model_id = self.local.model_id_for(request.preferred_speed);
                    let observed: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
                        Box::pin(TotalGuardedStream::new(
                            ThroughputObservedStream::new(
                                stream,
                                ThroughputTarget::Local(Arc::clone(&self.local_observations)),
                            )
                            .with_outcome(self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::LocalFallback {
                                    model_id: model_id.clone(),
                                },
                                attempt_index,
                                &failovers,
                            )),
                            total,
                        ));
                    return Ok((observed, model_id));
                }
            }
        }
        tracing::error!(
            target: "mesh.health",
            last_err = ?last_err,
            "mesh-inference: route cascade exhausted — every candidate peer and the local fallback failed for this request"
        );
        // Nothing served. Close the join anyway: a decision with no
        // outcome is indistinguishable from a lost record, and the
        // calibration contract needs "the mesh could not serve this"
        // to be a *measurable* result rather than a gap.
        {
            let err_text = last_err
                .as_ref()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "cascade exhausted".to_string());
            let shed = decision_log::looks_shed(&err_text);
            self.outcome_ctx(
                &decision_id,
                &oicp_request_id,
                ServedBy::Failed,
                failovers.len() as u32,
                &failovers,
            )
            .failed(err_text, shed);
        }
        Err(last_err.unwrap_or_else(|| {
            sovereign_core::error::Error::Routing(
                "mesh-inference: route cascade exhausted with no success".into(),
            )
        }))
    }

    /// Typed-Finish sibling of `complete_stream_with_id`. Cascade
    /// shape comes from `select_route` (shared); the per-terminus
    /// stream construction uses `complete_stream_with_finish`
    /// instead of `complete_stream`, propagating typed
    /// `StreamFrame::Finish { reason, usage }` all the way to the
    /// runtime so cutoff truncation lights up the desktop chip with
    /// the real reason (not the prior chars-per-token heuristic).
    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(
        Pin<Box<dyn Stream<Item = sovereign_core::types::StreamFrame> + Send>>,
        String,
    )> {
        use sovereign_core::types::StreamFrame;
        let RoutePlan {
            steps,
            decision_id,
            oicp_request_id,
        } = self.select_route(request).await?;
        let mut last_err: Option<sovereign_core::error::Error> = None;
        let mut failovers: Vec<decision_log::FailoverAttempt> = Vec::new();
        for (attempt_index, step) in steps.into_iter().enumerate() {
            let attempt_index = attempt_index as u32;
            match step {
                RouteDecision::LocalNamed { attribution, guard } => {
                    let stream = self.local.complete_stream_with_finish(request).await?;
                    let observed: Pin<Box<dyn Stream<Item = StreamFrame> + Send>> =
                        Box::pin(InflightGuardedStream::new(
                            ThroughputObservedStream::new(
                                stream,
                                ThroughputTarget::Local(Arc::clone(&self.local_observations)),
                            )
                            .with_outcome(self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::Local {
                                    model_id: attribution.clone(),
                                },
                                attempt_index,
                                &failovers,
                            )),
                            guard,
                        ));
                    return Ok((observed, attribution));
                }
                RouteDecision::Peer {
                    peer,
                    peer_cand,
                    ledger,
                    disposition,
                    pinned_model_id,
                } => {
                    let serve_request = pinned_request(request, pinned_model_id.as_deref());
                    let serve_request = serve_request.as_ref();
                    let mut last_transport_err: Option<String> = None;
                    let node_id_hex = self.local_node_id_hex().await;
                    for url in &peer.base_urls {
                        let rp = provider_for_peer(&peer, url, node_id_hex.as_deref());
                        match rp.complete_stream_with_finish(serve_request).await {
                            Ok(stream) => {
                                let attribution =
                                    format!("{} @ peer {}", peer_cand.model_id, peer.name);
                                let mut wrapper = ThroughputObservedStream::new(
                                    stream,
                                    ThroughputTarget::Peer {
                                        name: peer.name.clone(),
                                        map: Arc::clone(&self.peer_observations),
                                    },
                                )
                                .with_outcome(self.outcome_ctx(
                                    &decision_id,
                                    &oicp_request_id,
                                    ServedBy::Peer {
                                        name: peer.name.clone(),
                                        node_id: Some(peer.node_id.to_hex()),
                                        model_id: peer_cand.model_id.clone(),
                                    },
                                    attempt_index,
                                    &failovers,
                                ));
                                if let Some(em) = ledger.clone() {
                                    wrapper = wrapper.with_ledger_emission(em);
                                }
                                let observed: Pin<Box<dyn Stream<Item = StreamFrame> + Send>> =
                                    Box::pin(wrapper);
                                self.peer_health.record_success(&peer.name);
                                // INVARIANT (no double-emit): once we return the
                                // peer's LIVE stream here, the routing cascade is
                                // over. A failure that surfaces *mid-stream* (the
                                // peer dies after ≥1 token) must NOT re-enter the
                                // cascade or restart locally — the client would
                                // then see duplicated / garbled output. The
                                // ranked failover below only retries from the
                                // pre-`Ok` `Err` arm (connect / headers / 503),
                                // never from a stream already handed out. Pinned
                                // by `peer_dies_mid_stream_does_not_duplicate`.
                                return Ok((observed, attribution));
                            }
                            Err(e) => {
                                tracing::info!(
                                    peer = %peer.name,
                                    url = %url,
                                    error = %e,
                                    "mesh-inference: typed peer transport error, trying next address"
                                );
                                last_transport_err = Some(format!("{e}"));
                            }
                        }
                    }
                    let step_err = last_transport_err
                        .clone()
                        .unwrap_or_else(|| "unreachable".into());
                    let shed = decision_log::looks_shed(&step_err);
                    self.book_peer_failure(&peer.name, &step_err, shed);
                    failovers.push(decision_log::FailoverAttempt {
                        peer: peer.name.clone(),
                        error: step_err.clone(),
                        shed,
                    });
                    match disposition {
                        PeerFailureDisposition::Hard { model_id } => {
                            // Terminal: no further step will serve, so
                            // this is where the join closes.
                            self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::Failed,
                                attempt_index,
                                &failovers,
                            )
                            .failed(step_err, shed);
                            return Err(sovereign_core::error::Error::Routing(format!(
                                "model '{}' is advertised by peer '{}' but all peer \
                                 addresses failed: {}",
                                model_id,
                                peer.name,
                                last_transport_err.unwrap_or_else(|| "unreachable".into())
                            )));
                        }
                        PeerFailureDisposition::Soft => {
                            tracing::info!(
                                peer = %peer.name,
                                "mesh-inference: typed peer failed, falling through to next route"
                            );
                            last_err =
                                last_transport_err.map(sovereign_core::error::Error::Inference);
                            continue;
                        }
                    }
                }
                RouteDecision::LocalFallback { total } => {
                    let stream = self.local.complete_stream_with_finish(request).await?;
                    let model_id = self.local.model_id_for(request.preferred_speed);
                    let observed: Pin<Box<dyn Stream<Item = StreamFrame> + Send>> =
                        Box::pin(TotalGuardedStream::new(
                            ThroughputObservedStream::new(
                                stream,
                                ThroughputTarget::Local(Arc::clone(&self.local_observations)),
                            )
                            .with_outcome(self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::LocalFallback {
                                    model_id: model_id.clone(),
                                },
                                attempt_index,
                                &failovers,
                            )),
                            total,
                        ));
                    return Ok((observed, model_id));
                }
            }
        }
        tracing::error!(
            target: "mesh.health",
            last_err = ?last_err,
            "mesh-inference: typed route cascade exhausted — every candidate peer and the local fallback failed for this request"
        );
        // Nothing served. Close the join anyway: a decision with no
        // outcome is indistinguishable from a lost record, and the
        // calibration contract needs "the mesh could not serve this"
        // to be a *measurable* result rather than a gap.
        {
            let err_text = last_err
                .as_ref()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "cascade exhausted".to_string());
            let shed = decision_log::looks_shed(&err_text);
            self.outcome_ctx(
                &decision_id,
                &oicp_request_id,
                ServedBy::Failed,
                failovers.len() as u32,
                &failovers,
            )
            .failed(err_text, shed);
        }
        Err(last_err.unwrap_or_else(|| {
            sovereign_core::error::Error::Routing(
                "mesh-inference: typed route cascade exhausted with no success".into(),
            )
        }))
    }

    /// Plain typed-stream surface — delegates to the cascade sibling
    /// and drops the attribution string, exactly like
    /// `complete_stream` does for the legacy shape. Without this
    /// override the trait default wraps `complete_stream` and
    /// synthesizes `Finish{Stop}` for every stream, erasing the real
    /// `Length`/`Cancelled` reasons the FIM inline-completion route
    /// and the desktop cutoff chip depend on.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = sovereign_core::types::StreamFrame> + Send>>> {
        Ok(self.complete_stream_with_id_and_finish(request).await?.0)
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

    fn embed_model_id(&self) -> String {
        // Embeds always run locally (see `embed`/`embed_batch` above),
        // so the local slot's id is the honest answer.
        self.local.embed_model_id()
    }

    fn code_model_id(&self) -> Option<String> {
        // Delegate so the mesh-level self-advertisement sees the
        // same code slot the underlying `EmbeddedLlamaCpp` sees.
        self.local.code_model_id()
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        // Reranking is local-slot work like embeds and FIM: the peer
        // path never carries it. Without this forward the mesh wrapper
        // — again, the provider the daemon actually installs — reports
        // the trait's `NotImplemented`, and `search_with_rerank`
        // catches that and silently returns un-reranked fusion. So a
        // configured `[rerank]` slot would sit loaded and unused, with
        // retrieval quietly worse and nothing in the logs to say so.
        self.local.rerank_batch(query, docs).await
    }

    fn fim_slot_info(&self) -> Option<sovereign_core::types::FimSlotInfo> {
        // FIM serving is inherently local (the keystroke path never
        // leaves this machine), so the honest answer is the local
        // engine's arrangement. Without this forward the mesh wrapper
        // — the provider the daemon actually installs — would report
        // the empty default and `/v1/completions` would 503 forever.
        self.local.fim_slot_info()
    }

    fn resident_slots(&self) -> Vec<sovereign_core::traits::ResidentSlot> {
        // Residency is about THIS node's locally-loaded weights, so
        // delegate to the underlying local engine. Without this forward
        // the mesh wrapper (the provider the daemon actually installs)
        // would report the empty default and `/status.inference.resident`
        // would always be blank.
        self.local.resident_slots()
    }

    fn compute_children(&self) -> Vec<sovereign_core::traits::ComputeChildStatus> {
        // Same reason as `resident_slots`: the compute children live under
        // THIS node's local routing facade, and the mesh wrapper is the
        // installed provider — without this forward `/status.inference.
        // compute_children` would always be empty.
        self.local.compute_children()
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.local.effective_context_size()
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.local.n_ctx_train_for_primary()
    }

    /// Delegate so the runtime budget calc sees the real BPE count
    /// (when local is `EmbeddedLlamaCpp`). Mesh-forwarded chat
    /// requests still budget against the *local* slot's ctx —
    /// `MeshInferenceProvider` doesn't know what tokenizer the peer
    /// will use, and the runtime decides compaction before routing.
    fn count_tokens(&self, text: &str) -> u32 {
        self.local.count_tokens(text)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.local.capabilities()
    }

    // Runtime slot management delegates to the wrapped local provider.
    // Without this override the trait's default-impl returns the
    // generic "this inference provider does not support runtime slot
    // load — only the embedded llama.cpp provider does" error even
    // when `local` is a real EmbeddedLlamaCpp. The bug surfaced
    // 2026-05-20 when `POST /internal/models/load` could not hot-load
    // Gemma into a daemon whose primary slot was Qwen3.6 — the load
    // adapter calls `self.provider.load_extra_slot`, which on the
    // MeshInferenceProvider path always hit the default.
    //
    // After a successful mutation we ALSO rebuild `self_manifest` so
    // mesh routing (`locate_named_model`) sees the new slot
    // immediately. Without the refresh, a hot-loaded slot serves chat
    // completions on a routing-by-model-id call only when the caller
    // bypasses MeshInferenceProvider's locator — which is not the
    // case for `/v1/chat/completions`. Confirmed 2026-05-20: bench
    // could load gemma-* into a Qwen-primary daemon but every
    // request 503'd with "no node in this mesh advertises model".
    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        let model_id = self.local.load_extra_slot(slot_name, path, context_size)?;
        self.refresh_self_manifest();
        Ok(model_id)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        let result = self.local.unload_extra_slot(slot_name)?;
        if result.is_some() {
            self.refresh_self_manifest();
        }
        Ok(result)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.local.extras_inventory()
    }
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
        Self {
            inner,
            _guard: guard,
        }
    }
}

impl<S> Stream for InflightGuardedStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// Stream wrapper that holds a [`LocalTotalGuard`] alive for the
/// stream's lifetime — drops it when the stream ends or is dropped.
/// Parallel to [`InflightGuardedStream`] but for the OICP-fallback
/// path where there is no explicit model_id and so no per-model
/// counter to maintain. Without this, fallback-to-local stream load
/// would never decrement the gossip publisher.
struct TotalGuardedStream<S> {
    inner: S,
    _guard: LocalTotalGuard,
}

impl<S> TotalGuardedStream<S> {
    fn new(inner: S, guard: LocalTotalGuard) -> Self {
        Self {
            inner,
            _guard: guard,
        }
    }
}

impl<S> Stream for TotalGuardedStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// Resolve a peer's effective in-flight count for scoring.
///
/// Returns `gossiped` when the peer's `NodeCapabilities` carried a
/// `current_in_flight` value (modern daemons); falls back to
/// `self_observed` otherwise (older peers + cold-start window before
/// the peer's first gossip round). This is the load-bearing rule
/// behind the gossip-load-awareness fix: the gossiped value reflects
/// the peer's *actual* serving load, while `self_observed` (from
/// `peer_observations[name].in_flight`) only counts founder-
/// originated dispatches and so undercounts peer-local traffic.
///
/// Extracted to a free function purely so the precedence rule is
/// unit-testable without spinning up the full `select_peer` HTTP
/// machinery. See [`tests::gossiped_in_flight_overrides_self_observed`].
fn effective_peer_in_flight(self_observed: u32, gossiped: Option<u32>) -> u32 {
    gossiped.unwrap_or(self_observed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::error::Error;
    use sovereign_core::oicp::{
        CapabilityHint, InferenceRequirements, LatencyClass, ModelStatus, ProviderModel,
        ShardingPrivacy,
    };
    use sovereign_core::types::Depth;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// A local provider whose advertised lineup FLIPS at runtime — the shape of
    /// a compute child that reaches Serving minutes after the daemon booted.
    /// Before the flip it answers the Slow tier with a small model and offers no
    /// extras, exactly as `ComputeRoutedProvider` does while its child is not
    /// yet serving.
    struct LateLoadingProvider {
        serving: Arc<AtomicBool>,
    }

    #[async_trait]
    impl InferenceProvider for LateLoadingProvider {
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
            Err(Error::NotImplemented("stub".into()))
        }

        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("stub".into()))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Err(Error::NotImplemented("stub".into()))
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Slow,
                relative_reasoning: Depth::Moderate,
            }
        }

        fn model_id_for(&self, speed: Speed) -> String {
            match speed {
                Speed::Slow | Speed::Medium if self.serving.load(Ordering::SeqCst) => {
                    "big-late-model".to_string()
                }
                _ => "small-fast-model".to_string(),
            }
        }

        fn extras_inventory(&self) -> Vec<(String, String)> {
            if self.serving.load(Ordering::SeqCst) {
                vec![("big-late-model".to_string(), "big-late-model".to_string())]
            } else {
                Vec::new()
            }
        }
    }

    struct NoPeers;

    #[async_trait]
    impl PeerEndpointSource for NoPeers {
        async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
            Vec::new()
        }
    }

    fn late_loading_mip() -> (Arc<AtomicBool>, MeshInferenceProvider) {
        let serving = Arc::new(AtomicBool::new(false));
        let local = Arc::new(LateLoadingProvider {
            serving: Arc::clone(&serving),
        });
        let mip = MeshInferenceProvider::with_peer_source(local, Arc::new(NoPeers));
        (serving, mip)
    }

    fn advertises(mip: &MeshInferenceProvider, id: &str) -> bool {
        mip.self_manifest.load().models.iter().any(|m| m.id == id)
    }

    /// The 2026-07-28 regression, at the unit level: a model that becomes
    /// serveable AFTER the manifest snapshot must become advertised when the
    /// refresh runs — otherwise `locate_named_model` misses and every request
    /// naming it 503s from a healthy node.
    #[test]
    fn refreshing_the_self_manifest_advertises_a_late_loading_model() {
        let (serving, mip) = late_loading_mip();
        assert!(
            !advertises(&mip, "big-late-model"),
            "nothing should advertise a model the provider cannot serve yet"
        );

        serving.store(true, Ordering::SeqCst);
        assert!(
            !advertises(&mip, "big-late-model"),
            "the manifest is a SNAPSHOT — it must not change until refreshed"
        );

        mip.refresh_self_manifest_because("compute child serving (test)");
        assert!(advertises(&mip, "big-late-model"));
    }

    /// Symmetry: a slot that stops serving must stop being advertised, or peers
    /// route into a guaranteed ComputeUnavailable.
    #[test]
    fn refreshing_un_advertises_a_model_that_stopped_serving() {
        let (serving, mip) = late_loading_mip();
        serving.store(true, Ordering::SeqCst);
        mip.refresh_self_manifest_because("serving");
        assert!(advertises(&mip, "big-late-model"));

        serving.store(false, Ordering::SeqCst);
        mip.refresh_self_manifest_because("retired");
        assert!(!advertises(&mip, "big-late-model"));
    }

    /// The detector. `reconcile` republishes only on drift, and says so; a
    /// second call must be a no-op, so a 60s tick never spams the log.
    #[test]
    fn reconcile_republishes_only_when_a_transition_was_missed() {
        let (serving, mip) = late_loading_mip();

        assert!(
            !mip.reconcile_self_manifest(),
            "no drift, nothing to republish"
        );

        // Flip WITHOUT refreshing — i.e. simulate the missing publish call.
        serving.store(true, Ordering::SeqCst);
        assert!(
            mip.reconcile_self_manifest(),
            "a missed transition must be detected and repaired"
        );
        assert!(advertises(&mip, "big-late-model"));

        assert!(
            !mip.reconcile_self_manifest(),
            "reconcile must be idempotent — the second call is a no-op"
        );
    }

    #[test]
    fn gossiped_in_flight_overrides_self_observed() {
        // Founder thinks the peer is idle (it never sent traffic
        // there); peer gossips that it's actually serving 7 local
        // requests. Scoring must use 7, not 0.
        assert_eq!(effective_peer_in_flight(0, Some(7)), 7);
    }

    #[test]
    fn gossiped_zero_overrides_nonzero_self_observed() {
        // Founder thinks 4 requests are in flight to peer (its own
        // dispatches still being tallied), but peer gossips 0 — a
        // legitimate signal that those have completed peer-side
        // and the founder's view is just lagging. The gossiped
        // value is fresher; trust it.
        assert_eq!(effective_peer_in_flight(4, Some(0)), 0);
    }

    #[test]
    fn falls_back_to_self_observed_when_no_gossip() {
        // Older peer (no current_in_flight field in its gossip).
        // The legacy founder-local view is the best we have —
        // scoring must use it rather than defaulting to 0.
        assert_eq!(effective_peer_in_flight(3, None), 3);
    }

    #[test]
    fn local_total_guard_inc_and_drop_balance() {
        // RAII guard correctness: every `enter_local_total` bump
        // must be matched by a Drop-time decrement so the published
        // counter converges back to its prior value.
        let publisher = Arc::new(AtomicU32::new(0));
        {
            // Simulate three concurrent local-serving dispatches.
            publisher.fetch_add(1, Ordering::Relaxed);
            publisher.fetch_add(1, Ordering::Relaxed);
            publisher.fetch_add(1, Ordering::Relaxed);
            assert_eq!(publisher.load(Ordering::Relaxed), 3);
            // Build three guards pointing at the same Arc — they
            // each decrement on drop.
            let g1 = LocalTotalGuard {
                publisher: Arc::clone(&publisher),
            };
            let g2 = LocalTotalGuard {
                publisher: Arc::clone(&publisher),
            };
            let g3 = LocalTotalGuard {
                publisher: Arc::clone(&publisher),
            };
            drop(g1);
            assert_eq!(publisher.load(Ordering::Relaxed), 2);
            drop(g2);
            assert_eq!(publisher.load(Ordering::Relaxed), 1);
            drop(g3);
            assert_eq!(publisher.load(Ordering::Relaxed), 0);
        }
        // A spurious drop on an already-zero counter must NOT
        // underflow — saturating subtract is the correctness
        // invariant for the publisher.
        let g_extra = LocalTotalGuard {
            publisher: Arc::clone(&publisher),
        };
        drop(g_extra);
        assert_eq!(
            publisher.load(Ordering::Relaxed),
            0,
            "LocalTotalGuard::drop must saturate, never underflow"
        );
    }

    #[test]
    fn published_counter_is_shared_across_clones() {
        // Acceptance test for the AppState ↔ MIP wire: the gossip
        // emitter reads via a clone of the same Arc the MIP's
        // guards write to. Writes on one Arc must be visible
        // through the clone.
        let mip_side = Arc::new(AtomicU32::new(0));
        let app_state_side = Arc::clone(&mip_side);
        mip_side.fetch_add(5, Ordering::Relaxed);
        assert_eq!(
            app_state_side.load(Ordering::Relaxed),
            5,
            "AppState reader must see MIP's writes when sharing the same Arc"
        );
    }

    // ── LocalAlternative: what a peer failure is allowed to mean ───
    //
    // `locate_named_model`'s own contract already said this: naming a
    // model MUST be honoured and silent substitution is forbidden,
    // but choosing between two nodes that BOTH advertise the id is a
    // LOAD-BALANCING decision, not a name-resolution one. The type
    // did not carry that distinction, so a failed load-balanced hop
    // was handled as though the name could no longer be honoured —
    // and once M5 piece 3 made peers actually shed, that turned
    // servable requests into client-visible 503s.

    /// A local provider that advertises exactly one id and serves it.
    struct ServesOne {
        id: &'static str,
        served: Arc<AtomicU32>,
        /// `model_id` of the last request this provider was handed —
        /// the only way to see what the routing layer PINNED.
        saw_model: Arc<std::sync::Mutex<Option<Option<String>>>>,
    }

    #[async_trait]
    impl InferenceProvider for ServesOne {
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
            self.served.fetch_add(1, Ordering::SeqCst);
            *self.saw_model.lock().expect("saw_model poisoned") = Some(_req.model_id.clone());
            Ok(CompletionResponse {
                text: "served locally".into(),
                tokens_used: 2,
                prompt_tokens: 1,
                model_id: self.id.to_string(),
                latency_ms: 1,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("stub".into()))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Err(Error::NotImplemented("stub".into()))
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Slow,
                relative_reasoning: Depth::Moderate,
            }
        }

        fn model_id_for(&self, _speed: Speed) -> String {
            self.id.to_string()
        }
    }

    struct OnePeer(PeerInferenceEndpoint);

    #[async_trait]
    impl PeerEndpointSource for OnePeer {
        async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
            vec![self.0.clone()]
        }
    }

    /// A peer at an address nothing listens on. Every attempt fails at
    /// connect, which is all these tests need — the fallback keys on
    /// whether WE hold the id, not on why the peer failed. (A shed is
    /// simply the failure that made this path reachable in practice.)
    fn dead_peer() -> PeerInferenceEndpoint {
        PeerInferenceEndpoint {
            node_id: commonwealth_core::ids::NodeId::from_u128(7),
            name: "DeadPeer".into(),
            base_urls: vec!["http://127.0.0.1:1/v1".into()],
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
            inference_availability: None,
            gossip_last_seen_unix: 0,
            transport: None,
        }
    }

    fn peer_manifest_for(id: &str) -> ProviderManifest {
        ProviderManifest {
            oicp_version: sovereign_contracts::oicp::OICP_VERSION.into(),
            provider: None,
            models: vec![ProviderModel {
                id: id.to_string(),
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
                size_gb: None,
                claims: Vec::new(),
                fingerprint: None,
            }],
            knowledge: None,
            federation: None,
            features: Vec::new(),
        }
    }

    /// MIP whose local side advertises `local_id` and whose single
    /// unreachable peer advertises `peer_id`. The peer manifest is
    /// pre-seeded so nothing is fetched, and `busy_local` raises our
    /// own in-flight count for `local_id` — which is the ONLY way the
    /// load-balance rule ever prefers a peer for a model we hold
    /// (ties go local).
    async fn mip_with_peer(
        local_id: &'static str,
        peer_id: &str,
        busy_local: bool,
    ) -> (MeshInferenceProvider, Arc<AtomicU32>) {
        let served = Arc::new(AtomicU32::new(0));
        let local = Arc::new(ServesOne {
            id: local_id,
            served: Arc::clone(&served),
            saw_model: Arc::new(std::sync::Mutex::new(None)),
        });
        let peer = dead_peer();
        let mip = MeshInferenceProvider::with_peer_source(local, Arc::new(OnePeer(peer.clone())));
        mip.peer_cache.write().await.insert(
            peer.node_id.to_hex(),
            CachedManifest {
                manifest: peer_manifest_for(peer_id),
                fetched_at: Instant::now(),
                rtt_ms: 1,
            },
        );
        if busy_local {
            mip.local_inflight_by_model
                .lock()
                .expect("local_inflight_by_model poisoned")
                .insert(local_id.to_string(), 1);
        }
        (mip, served)
    }

    fn named(id: &str) -> CompletionRequest {
        CompletionRequest {
            model_id: Some(id.to_string()),
            ..CompletionRequest::new("hi")
        }
    }

    #[tokio::test]
    async fn a_load_balanced_peer_route_remembers_we_hold_the_model_too() {
        let (mip, _) = mip_with_peer("shared-model", "shared-model", true).await;
        match mip.locate_named_model("shared-model").await {
            NamedModelLocation::Peer(_, _, LocalAlternative::LocalHasIt) => {}
            other => panic!(
                "both nodes advertise this id and we are busier, so the peer should \
                 win the load balance WITH a local alternative recorded; got {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn a_sole_holder_peer_route_says_so() {
        let (mip, _) = mip_with_peer("something-else", "peer-only", false).await;
        match mip.locate_named_model("peer-only").await {
            NamedModelLocation::Peer(_, _, LocalAlternative::SoleHolder) => {}
            other => panic!("only the peer advertises this id; got {other:?}"),
        }
    }

    /// THE REGRESSION, at the unit level. The balancer sent a named
    /// turn to a peer purely because we looked busier; the peer then
    /// failed. Failing the caller is wrong — we advertise the very id
    /// they asked for.
    #[tokio::test]
    async fn a_failed_load_balanced_peer_turn_is_served_locally_not_failed() {
        let (mip, served) = mip_with_peer("shared-model", "shared-model", true).await;

        let resp = mip
            .complete(&named("shared-model"))
            .await
            .expect("we advertise 'shared-model' ourselves — a peer declining it \
                     must not turn a servable request into an error");

        assert_eq!(resp.text, "served locally");
        assert_eq!(
            served.load(Ordering::SeqCst),
            1,
            "the local provider must actually have been asked to serve"
        );
    }

    /// The control, and the reason the test above is not a licence to
    /// substitute (§18.3): the peer is the ONLY holder, so there is no
    /// local copy of what was named. Falling back would serve a
    /// DIFFERENT model under the caller's chosen name. It must fail.
    #[tokio::test]
    async fn a_failed_sole_holder_peer_turn_still_fails_loud() {
        let (mip, served) = mip_with_peer("something-else", "peer-only", false).await;

        let err = mip
            .complete(&named("peer-only"))
            .await
            .expect_err("nobody local holds 'peer-only' — this must not silently \
                         become our own model");

        assert!(
            err.to_string().contains("peer-only"),
            "the error must name the model that could not be served; got {err}"
        );
        assert_eq!(
            served.load(Ordering::SeqCst),
            0,
            "SILENT SUBSTITUTION: the local provider served a request for a model \
             it does not advertise"
        );
    }

    /// The streaming half of the same fix. It is expressed as an extra
    /// cascade step rather than a branch, so it is asserted on the
    /// PLAN — cheap, and it does not need a live stream to be real.
    #[tokio::test]
    async fn the_streaming_cascade_puts_our_own_copy_behind_a_load_balanced_peer() {
        let (mip, _) = mip_with_peer("shared-model", "shared-model", true).await;
        let plan = mip
            .select_route(&named("shared-model"))
            .await
            .expect("a route plan");
        match plan.steps.as_slice() {
            [RouteDecision::Peer {
                disposition: PeerFailureDisposition::Soft,
                ..
            }, RouteDecision::LocalNamed { attribution, .. }] => {
                assert_eq!(attribution, "shared-model", "attribution keeps the caller's name");
            }
            _ => panic!(
                "a load-balanced peer step must be Soft and be followed by OUR copy \
                 of the same id, or a peer shed ends the cascade with an error"
            ),
        }
    }

    /// Control, matching the non-streaming one: sole-holder stays Hard.
    #[tokio::test]
    async fn the_streaming_cascade_leaves_a_sole_holder_route_hard() {
        let (mip, _) = mip_with_peer("something-else", "peer-only", false).await;
        let plan = mip
            .select_route(&named("peer-only"))
            .await
            .expect("a route plan");
        match plan.steps.as_slice() {
            [RouteDecision::Peer {
                disposition: PeerFailureDisposition::Hard { model_id },
                ..
            }] => {
                assert_eq!(model_id, "peer-only");
            }
            _ => panic!(
                "a sole-holder named route must stay Hard and have NO local step — \
                 falling back would serve a different model under the caller's name"
            ),
        }
    }

    /// DOUBLE-CHECK of the unification, and it caught something.
    ///
    /// A shared primary that resolves to THIS node must still reach the
    /// local provider carrying the id that was resolved. The old
    /// non-streaming body guaranteed it by rewriting `model_id` up
    /// front; the unified body pins on the PEER step, and the
    /// `LocalNamed` step must pin too or the provider falls back to
    /// picking a slot by speed — i.e. the caller asked for the shared
    /// model and silently got whatever this node felt like serving.
    #[tokio::test]
    async fn a_shared_primary_resolving_locally_still_names_the_model_it_resolved() {
        let served = Arc::new(AtomicU32::new(0));
        let saw = Arc::new(std::sync::Mutex::new(None));
        let local = Arc::new(ServesOne {
            id: "shared-model",
            served: Arc::clone(&served),
            saw_model: Arc::clone(&saw),
        });
        let mip = MeshInferenceProvider::with_peer_source(local, Arc::new(NoPeers));
        mip.set_shared_model_id(Some("shared-model".into()));

        let request = CompletionRequest::new("hi").with_speed(Speed::Slow).with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(ShardingPrivacy::MeshAllowed),
        );
        let _ = mip.complete(&request).await;

        assert_eq!(served.load(Ordering::SeqCst), 1, "the local provider must serve");
        let saw = saw
            .lock()
            .expect("saw_model poisoned")
            .clone()
            .expect("the local provider was never called");
        assert_eq!(
            saw.as_deref(),
            Some("shared-model"),
            "the resolved shared primary must be named on the request handed to the \
             local provider — otherwise its slot picker chooses by SPEED and the \
             caller silently gets a different model than the one that was resolved"
        );
    }
}
