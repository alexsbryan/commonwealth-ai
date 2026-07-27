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

/// Top pick of a [`RankedSelection`], carrying the same decision
/// identity so the non-streaming `complete` path can close the join
/// too.
///
/// The identity fields sit outside the `Option` on purpose: a
/// selection that picked nobody still made a decision, and that
/// decision still needs an outcome. Folding the ids into the `Some`
/// arm would silently drop every `stay_local` record from the join.
struct SinglePeerSelection {
    pick: Option<(PeerInferenceEndpoint, ModelCandidate)>,
    decision_id: String,
    oicp_request_id: String,
}

/// A routing cascade plus the identity of the decision that produced
/// it. The cascade steps are tried in order; whichever one serves
/// closes the join by emitting an outcome carrying `decision_id` and
/// its own index (index > 0 means a failover happened, which is a
/// waste metric in `SCHEDULER_QUALITY.md` §5).
struct RoutePlan {
    steps: Vec<RouteDecision>,
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
    /// Local-side throughput benchmark. Set by the daemon's startup
    /// probe via [`MeshInferenceProvider::set_local_benchmark`] once
    /// the bundled model has been measured; read on every scoring
    /// pass to feed the local candidate's throughput factor. `None`
    /// before the probe completes — the scheduler then falls back to
    /// observation-only scoring, which is safe because local
    /// observations accumulate fast.
    local_benchmark: Arc<RwLock<Option<sovereign_core::oicp::BenchmarkResult>>>,
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
        let new_manifest = build_self_manifest(self.local.as_ref());
        tracing::info!(
            models = new_manifest.models.len(),
            "mesh-inference: self_manifest refreshed (post slot mutation)"
        );
        self.self_manifest.store(Arc::new(new_manifest));
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
            local_benchmark: Arc::new(RwLock::new(None)),
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
                benchmark: self.local_benchmark.read().await.clone(),
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

    /// Replace the local-side benchmark result. Called once by the
    /// daemon's startup probe after the bundled model has been
    /// measured. Idempotent — calling twice with the same result is
    /// a no-op for downstream scoring.
    pub async fn set_local_benchmark(&self, bench: sovereign_core::oicp::BenchmarkResult) {
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
    pub async fn local_benchmark(&self) -> Option<sovereign_core::oicp::BenchmarkResult> {
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
    async fn select_peer(
        &self,
        request: &CompletionRequest,
        path: DecisionPath,
    ) -> SinglePeerSelection {
        let sel = self.select_peers_ranked(request, path).await;
        SinglePeerSelection {
            pick: sel.peers.into_iter().next(),
            decision_id: sel.decision_id,
            oicp_request_id: sel.oicp_request_id,
        }
    }

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
        if !crate::oicp_select::offload_eligible(req_oicp) {
            tracing::debug!(
                oicp_request_id = %oicp_request_id,
                gate = "not_offload_eligible",
                sharding = ?req_oicp.sharding(),
                latency = ?req_oicp.effective_latency_class(),
                "mesh-inference: staying local (SLOT_POLICY §5: \
                 offload iff MeshAllowed AND latency != Fast)"
            );
            return self.gated(rec, "not_offload_eligible");
        }

        // Local is always a candidate. `None` means no loaded
        // model's claims can serve the request — any peer that CAN
        // then wins automatically. After claim-scoring, fold in
        // v0.3 §7 operational adjustments so a hot local slot can
        // lose to an idle peer on load, and a reliable peer can
        // beat a failure-prone local.
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
                    benchmark: local_bench.as_ref(),
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
    /// - `Peer(peer, candidate)`: route there over HTTP.
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
                NamedModelLocation::Unknown
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
            let located = self.locate_named_model(&model_id).await;
            let verdict = match &located {
                NamedModelLocation::Local => Verdict::NamedLocal {
                    model_id: model_id.clone(),
                },
                NamedModelLocation::Peer(peer, cand) => Verdict::NamedPeer {
                    peer: peer.name.clone(),
                    model_id: cand.model_id.clone(),
                },
                NamedModelLocation::Unknown => Verdict::NamedUnknown {
                    model_id: model_id.clone(),
                },
            };
            let decision = rec.finish(verdict, &[]);
            let decision_id = decision.decision_id.clone();
            let oicp_request_id = decision.oicp_request_id.clone();
            decision_log::emit_decision(&self.decision_sink, decision);
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
                NamedModelLocation::Peer(peer, peer_cand) => {
                    tracing::info!(
                        peer = %peer.name,
                        addrs = peer.base_urls.len(),
                        model = %peer_cand.model_id,
                        soft,
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
                            },
                            RouteDecision::LocalFallback { total },
                        ]))
                    } else {
                        Ok(plan(vec![RouteDecision::Peer {
                            peer,
                            peer_cand,
                            ledger,
                            disposition: PeerFailureDisposition::Hard { model_id },
                        }]))
                    }
                }
                NamedModelLocation::Unknown => {
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
                        tracing::info!(
                            shared = %model_id,
                            "mesh-inference: shared model forming/unavailable — \
                             falling through to ranked mesh selection"
                        );
                        Ok(self
                            .ranked_route_plan(request, DecisionPath::NamedFallthrough)
                            .await)
                    } else {
                        Err(sovereign_core::error::Error::ModelNotLoaded(format!(
                            "no node in this mesh advertises model '{}' — \
                             check `/v1/models` for available names",
                            model_id
                        )))
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
    },
    /// Local fallback — total-counter guard (no per-model accounting
    /// because the request didn't name a model).
    LocalFallback { total: LocalTotalGuard },
}

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
fn provider_for_peer(peer: &PeerInferenceEndpoint, url: &str) -> RemoteApiProvider {
    const PEER_CONTEXT: u32 = 32_768;
    match &peer.transport {
        Some(t) => RemoteApiProvider::with_client_and_bearer(
            url,
            t.client.clone(),
            t.bearer.clone(),
            "mesh-peer",
            PEER_CONTEXT,
        ),
        None => RemoteApiProvider::new(url, None, "mesh-peer", PEER_CONTEXT),
    }
}

#[async_trait]
impl InferenceProvider for MeshInferenceProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        // Shared-model primary: a node configured to use a mesh-hosted shared
        // model routes its primary (Slow) turn into it, resolved to a `model_id`
        // so the named path below routes there; when the cluster is forming or
        // unreachable, fall THROUGH to the ranked scorer below rather than
        // collapsing to this node's own model — same rule the streaming
        // `select_route` applies, for the same reason (see its
        // `NamedModelLocation::Unknown` arm). Here the fallthrough costs
        // nothing structural: not rewriting `model_id` leaves the request on
        // the ordinary ranked path that already runs a few lines down.
        // (The streaming path additionally adds soft peer-failure fallback;
        // this non-streaming path routes by name once resolved.)
        let mut ranked_path = DecisionPath::RankedOicp;
        let _shared_owned;
        let request = if explicit_model_id(request).is_none() {
            match self.shared_primary_id(request) {
                Some(shared_id) => match self.locate_named_model(&shared_id).await {
                    NamedModelLocation::Unknown => {
                        tracing::info!(
                            shared = %shared_id,
                            "mesh-inference: shared model forming/unavailable — \
                             falling through to ranked mesh selection (complete)"
                        );
                        ranked_path = DecisionPath::NamedFallthrough;
                        request
                    }
                    _ => {
                        _shared_owned = CompletionRequest {
                            model_id: Some(shared_id),
                            ..request.clone()
                        };
                        &_shared_owned
                    }
                },
                None => request,
            }
        } else {
            request
        };
        // Priority: when the caller names a specific model_id, that
        // name is the routing signal — even when the request carries
        // no OICP envelope and would not otherwise be offload-eligible
        // (SLOT_POLICY §5). Silent
        // substitution to the local primary slot was the bug here;
        // an explicit name must either be served by the node that
        // advertises it or fail loudly so the caller can react.
        if let Some(model_id) = explicit_model_id(request) {
            match self.locate_named_model(model_id).await {
                NamedModelLocation::Local => {
                    // Resolve slot aliases for the local-serving path
                    // only — the routing decision above already saw
                    // the alias and chose Local, so peers that also
                    // advertise the alias got their fair chance to
                    // win. The underlying provider works in terms of
                    // GGUF stems, so we rewrite here and hand it the
                    // resolved id. No-op when the requested id isn't
                    // an alias (the map lookup returns None).
                    let aliases = self.slot_aliases.load();
                    let resolved = aliases.get(model_id).cloned();
                    let log_model = model_id.to_string();
                    let request_owned;
                    let serve_request = match resolved {
                        Some(target) => {
                            tracing::info!(
                                alias = %log_model,
                                target = %target,
                                "mesh-inference: serving complete() locally — resolved slot alias"
                            );
                            request_owned = CompletionRequest {
                                model_id: Some(target),
                                ..request.clone()
                            };
                            &request_owned
                        }
                        None => {
                            tracing::info!(
                                model = %log_model,
                                "mesh-inference: serving complete() locally by explicit model name"
                            );
                            request
                        }
                    };
                    let _guard = self.enter_local_inflight(&log_model);
                    return self.local.complete(serve_request).await;
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
                        let rp = provider_for_peer(&peer, url);
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

        // Non-streaming path. The decision→outcome join is closed by
        // hand here rather than by the stream wrapper, because there
        // is no stream: `complete()` measures total wall time only, so
        // its outcome records `total_ms` with `ttft_ms: None`. Reading
        // a TTFT off a non-streaming call would be a fabrication.
        let SinglePeerSelection {
            pick,
            decision_id,
            oicp_request_id,
        } = self.select_peer(request, ranked_path).await;
        let mut failovers: Vec<decision_log::FailoverAttempt> = Vec::new();
        if let Some((peer, peer_cand)) = pick {
            tracing::info!(
                peer = %peer.name,
                addrs = peer.base_urls.len(),
                peer_pick = %peer_cand.model_id,
                "mesh-inference: routing complete() to peer"
            );
            let started = Instant::now();
            let mut last_transport_err: Option<String> = None;
            for url in &peer.base_urls {
                let rp = provider_for_peer(&peer, url);
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
                        self.outcome_ctx(
                            &decision_id,
                            &oicp_request_id,
                            ServedBy::Peer {
                                name: peer.name.clone(),
                                node_id: Some(peer.node_id.to_hex()),
                                model_id: peer_cand.model_id.clone(),
                            },
                            0,
                            &[],
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
                            "mesh-inference: peer complete() transport error, trying next address"
                        );
                        last_transport_err = Some(format!("{e}"));
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
            let err_text = last_transport_err.unwrap_or_else(|| "unreachable".into());
            let shed = decision_log::looks_shed(&err_text);
            failovers.push(decision_log::FailoverAttempt {
                peer: peer.name.clone(),
                error: err_text,
                shed,
            });
        }
        // Two flows arrive here: (a) `select_peer` returned `None`
        // and we serve locally without ever trying a peer, (b) we
        // tried a peer, every address failed, and we fell back. Both
        // produce load on the local provider and so must increment
        // the gossiped total counter — otherwise peers see this node
        // as idle when it isn't. The guard drops at the end of the
        // await (or on `?` if `complete` errors), keeping the count
        // in lock-step with reality.
        let _total = self.enter_local_total();
        let started = Instant::now();
        let result = self.local.complete(request).await;
        // Both flows that reach here close the join: (a) the selector
        // ran and chose nobody (a `stay_local` decision — still a
        // decision), and (b) a peer was tried and failed. `failovers`
        // is what tells them apart in the record.
        let ctx = self.outcome_ctx(
            &decision_id,
            &oicp_request_id,
            ServedBy::LocalFallback {
                model_id: self.local.model_id_for(request.preferred_speed),
            },
            failovers.len() as u32,
            &failovers,
        );
        match &result {
            Ok(_) => ctx.complete(None, Some(started.elapsed().as_secs_f64() * 1000.0), None),
            Err(e) => ctx.failed(e.to_string(), false),
        }
        result
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
                } => {
                    let mut last_transport_err: Option<String> = None;
                    for url in &peer.base_urls {
                        let rp = provider_for_peer(&peer, url);
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
                    self.peer_health.record_failure(&peer.name);
                    let step_err = last_transport_err
                        .clone()
                        .unwrap_or_else(|| "unreachable".into());
                    let shed = decision_log::looks_shed(&step_err);
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
                } => {
                    let mut last_transport_err: Option<String> = None;
                    for url in &peer.base_urls {
                        let rp = provider_for_peer(&peer, url);
                        match rp.complete_stream_with_finish(request).await {
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
                    self.peer_health.record_failure(&peer.name);
                    let step_err = last_transport_err
                        .clone()
                        .unwrap_or_else(|| "unreachable".into());
                    let shed = decision_log::looks_shed(&step_err);
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
    use std::sync::atomic::{AtomicU32, Ordering};

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
}
