//! Serving's part of the node's state — the model, pipeline and slot aliases,
//! the servable model files, the local inference handle and RPC shard warmer,
//! the inference store, the peer and client admission schedulers with their
//! caps, switch, tallies, rejected-header record and reciprocity weights, the
//! contribution pause and yield-peers switch, the availability composite, the
//! in-flight gauge and the venue preferences.
//!
//! DC §4.2 assigns these twenty to Serving, whose home is
//! `sovereign-serving-host`, which `sovereign-api` may name
//! (`ARCH_LAYERS.toml:704`). Until `REVIEW-mint-daemon-move` relocates it this
//! part is scaffolding carried on `AppStateInner`; the route shells read it
//! directly rather than through delegating accessors.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use tokio::sync::RwLock;

use commonwealth_core::ids::NodeId;
use commonwealth_state::store_adapter::InferenceStateStore;
use commonwealth_state::PeerPreferenceStore;
use oicp_types::model_aliases::ModelAliasTable;
use serving_policy::fair_sched::SchedCore;
use sovereign_serving_host::admission::Principal;

use super::{LocalInferenceService, PeerTally, RejectedNodeIdHeader, RpcShardWarmer};

/// Serving's twenty fields, held as `AppStateInner::serving`.
pub struct ServingPart {
    /// Inference plan, model info, ledger, and llama addresses — all via MeshStore.
    pub inference_store: InferenceStateStore,
    pub model_aliases: ModelAliasTable,
    /// ATOS pipeline aliases — resolved before `model_aliases` when
    /// an incoming request carries a pipeline name like
    /// `commonwealth/sovereign-coder`. Loaded from the embedded
    /// `default_pipelines.toml` at `AppState::new` time.
    pub pipeline_aliases: serving_policy::pipeline_aliases::PipelineAliasTable,
    /// Dynamic slot-name aliases. Map keys are operator-friendly slot
    /// labels (`primary`, `fast`, `code`, `embed`) — possibly prefixed
    /// `commonwealth/` for namespaced lookups. Values are the GGUF
    /// stems (model_ids) currently bound to that slot.
    ///
    /// Populated at daemon boot from `SetupConfig.models.*` so an
    /// operator can write `commonwealth/primary` in opencode's config
    /// (or any other client) and have requests follow whatever GGUF
    /// the daemon happens to be loading without rewriting the client
    /// config when models swap. `ArcSwap` lets the admin reload path
    /// hot-swap the table when `[models]` changes on disk.
    ///
    /// Empty when the daemon hasn't installed slot bindings yet
    /// (early boot, or non-embedded daemons that don't own a
    /// `SetupConfig`). Resolution then falls through to the
    /// existing pipeline / model alias paths.
    pub slot_aliases: ArcSwap<std::collections::HashMap<String, String>>,
    /// Absolute paths of GGUF files this daemon will serve over
    /// `/internal/v1/models/*` to other mesh peers. Populated at
    /// daemon boot from `SetupConfig.models.*` so a friend or a
    /// fresh cloud pod can pull the model files from us instead of
    /// from R2/S3 — the friend doesn't have our bucket creds, and
    /// the cloud pod's R2 sync has been the slowest step of every
    /// fresh launch.
    ///
    /// `ArcSwap` so the admin reload path can update the list when
    /// `[models]` paths change on disk. Empty when the daemon has
    /// not installed bindings yet (early boot, or test fixtures
    /// that bypass the production `start_daemon` flow).
    ///
    /// Serving is an allowlist, not a directory browser: only paths
    /// listed here are exposed. A request whose `name` doesn't match
    /// the `file_name()` of one of these paths gets 404, even if
    /// the file exists somewhere on disk. Keeps the surface area to
    /// "files this daemon is configured to load and would have
    /// already loaded itself" — same trust boundary as the
    /// inference path.
    pub servable_model_files: ArcSwap<Vec<std::path::PathBuf>>,
    /// Current PUBLISHED inference availability (0.0–1.0) — read by gossip
    /// each round to populate `NodeCapabilities.inference_availability`.
    /// Default 1.0.
    ///
    /// This is a DERIVED value with exactly one writer,
    /// [`crate::state::AppState::recompute_local_availability`], and two named
    /// inputs: [`Self::activity_inference_availability`] (what
    /// sovereign-server's ActivityReporter reports) and the yield-to-local-user
    /// predicate (`crate::state::AppState::yield_availability_floor`). The
    /// published value is the MINIMUM of the two, because both are ceilings on
    /// what this node can actually serve and the honest advertisement is the
    /// tighter one.
    ///
    /// It is a composite rather than a plain setter target because the two
    /// inputs move independently: before 2026-08-14 the field was
    /// last-writer-wins, so a node that was refusing every peer request with
    /// `yielded_to_local` still gossiped `availability: 1.0` forever, and the
    /// mesh scheduler kept selecting it (measured: 421 of 421 dispatches
    /// refused — note 3234d770). Writing the yield state through the same
    /// plain setter would have re-created that bug in the other direction,
    /// with an "idle" activity report erasing a live yield window.
    pub local_inference_availability: RwLock<f32>,
    /// The ACTIVITY half of the availability composite: what
    /// sovereign-server's ActivityReporter last reported via
    /// POST /internal/node/activity ("hot" 0.20 … "idle" 1.00). Default 1.0
    /// on nodes where no reporter runs (the daemon-only case).
    ///
    /// Stored separately from the published value so a yield window can rise
    /// and fall without destroying the coding-activity signal underneath it.
    pub activity_inference_availability: RwLock<f32>,
    /// Optional in-process inference service. When Sovereign embeds
    /// the daemon, this is a wrapper over its `EmbeddedLlamaCpp` so
    /// `/v1/chat/completions` serves peer requests from the same
    /// model the local user would use. `None` in the standalone
    /// Commonwealth daemon — that path routes via the orchestrator
    /// to spawned `llama-server` processes instead.
    pub local_inference: Option<std::sync::Arc<dyn LocalInferenceService>>,
    /// Worker-side auto-warm hook for distributed inference. Installed by the
    /// daemon alongside `local_inference`; drives `POST /internal/rpc-warm`.
    /// `None` on a node that isn't an inference worker. See [`RpcShardWarmer`].
    pub rpc_shard_warmer: Option<std::sync::Arc<dyn RpcShardWarmer>>,
    /// Fair admission for peer-served inference — one accounting authority
    /// (the same `SchedCore` policy the chat server uses) holding the
    /// runtime-mutable global ceiling (`slots`) AND a per-node concurrency
    /// cap, so one peer can't hog the pool even under the ceiling. `slots =
    /// usize::MAX` (default) disables the ceiling ("share freely"); `0`
    /// rejects all peer work (equivalent to `SOVEREIGN_DISABLE_PEER_INFERENCE=1`).
    /// Set via `POST /internal/contribution/ceiling`. The middleware
    /// (`crate::admission`) calls `try_grant` per peer request and 503s on
    /// refusal; the per-request `PeerInflightGuard` `release`s on drop.
    pub peer_sched: Mutex<SchedCore<NodeId>>,

    /// Fair admission for **client**-served inference — the same `SchedCore`
    /// policy as `peer_sched`, keyed by [`sovereign_serving_host::admission::Principal`]
    /// instead of `NodeId`, so the population `MESH_SCALE_100_USERS_1000_CORPORA.md`
    /// §9.3 measured (ten local callers on one node) is rationed by *who is
    /// asking* rather than by arrival order.
    ///
    /// The key is the published `Principal`, not the wire-side `PrincipalKey`:
    /// `admission` derives both its fairness and its peer keys from the one
    /// identity type (`DAEMON_CORE.md` §3.3), so there is no second identity
    /// scheme to drift (ARCH principle 8). The daemon's resolver maps the two
    /// partitions one-to-one.
    ///
    /// Its global slot budget is deliberately `usize::MAX`: this gate must
    /// never refuse on depth. §7.1 R2's correction is explicit that a depth
    /// shed here would double-queue against the inference slot queue's
    /// deliberate predicted-wait shed, which remains THE shed decider. The
    /// only rule this scheduler enforces is the per-principal equal share
    /// ([`serving_policy::fair_sched::fair_share_cap`]), and `try_grant`
    /// never leaves a waiter behind — so there is no second queue either.
    pub client_sched: Mutex<SchedCore<Principal>>,

    /// Concurrency budget divided among active principals by
    /// [`serving_policy::fair_sched::fair_share_cap`]. See
    /// [`crate::admission::DEFAULT_CLIENT_FAIR_CONCURRENCY`] for how the
    /// default is derived and `SOVEREIGN_CLIENT_FAIR_CONCURRENCY` to override.
    pub client_fair_concurrency: std::sync::atomic::AtomicU32,

    /// Kill switch for the client fairness gate
    /// (`SOVEREIGN_CLIENT_FAIRNESS=0`). Default on. When off, the gate
    /// resolves and LOGS the principal but never caps — which is exactly the
    /// §9.3 red, reachable on the shipped binary for A/B.
    pub client_fairness_enabled: std::sync::atomic::AtomicBool,

    /// Per-peer request tally (order `seat-resource-commons` UC-R1).
    /// Written by the admission middleware (begin on admit, end when
    /// the response BODY ends — see `crate::admission::GuardedBody`);
    /// read by `/status` to answer "is this daemon serving the peer
    /// right now?" Keyed by the `X-Node-Id` header value (the only
    /// peer attribution available; see [`PeerTally`]).
    ///
    /// `std::sync::RwLock` on purpose: a short-lived counter map with
    /// sync read/write (no await points on the admission hot path),
    /// the same shape as `peer_sched`'s `std::sync::Mutex`.
    pub peer_tally: std::sync::RwLock<HashMap<NodeId, PeerTally>>,

    /// The most recent present-but-malformed `X-Node-Id` header value
    /// (order commons-fluency fix 7). A peer request whose header
    /// fails [`crate::headers::parse_x_node_id`] still gets gated and
    /// tallied under the zero node, and `/status` must NAME the
    /// rejected value and the expected wire form instead of showing an
    /// opaque `node-0000000000000000` row — absence is reported, never
    /// defaulted (ARCH §18.3). `None` until the first malformed header
    /// arrives. Written on the admission path, read by `/status`.
    pub peer_tally_rejected: std::sync::Mutex<Option<RejectedNodeIdHeader>>,

    /// Cached reciprocity weight per peer node (`1.0 + k·norm(contribution)`),
    /// refreshed out-of-band from the contribution ledger by a daemon loop.
    /// Scales each node's effective concurrency cap when the operator is
    /// rationing (a finite ceiling) — a contributor may hold more slots at
    /// once. Absent nodes are neutral (`1.0`). `ArcSwap` for lock-free reads
    /// on the admission hot path.
    pub reciprocity_weights: ArcSwap<HashMap<NodeId, f64>>,

    /// Unix-seconds expiry for a runtime contribution pause. `0`
    /// means not paused. `now >= paused_until` means the pause has
    /// expired; the middleware simply compares without writing the
    /// field. Settable via `POST /internal/contribution/pause`.
    pub contribution_paused_until: std::sync::atomic::AtomicI64,

    /// When `true`, peer-served requests honour the foreground-yield
    /// window just like ingest workers do — a peer chat that lands
    /// during the window after a local turn 503s with `Retry-After`
    /// rather than competing with the user for the GPU. Default
    /// `true`; the setting is exposed via the same Settings surface
    /// as the foreground-yield window itself.
    pub yield_peers_to_foreground: std::sync::atomic::AtomicBool,

    /// Per-peer preference store (Ostrom sanctions). Local-only,
    /// never gossiped — see
    /// `commonwealth_state::peer_preferences` for the structural
    /// invariants. The manifest endpoint reads this on every
    /// fetch to apply per-requester affinity multipliers.
    pub peer_preferences: PeerPreferenceStore,

    /// Shared in-flight counter for local-serve inference. Installed
    /// once by the daemon bootstrap after `InferenceRouter::new`
    /// returns. Read by the gossip emitter
    /// (`sovereign-mesh::capabilities::build_local_capabilities`) on
    /// every tick to populate
    /// [`commonwealth_core::capabilities::NodeCapabilities::current_in_flight`].
    ///
    /// Lifecycle:
    /// * Cold start: the router creates its own private `Arc<AtomicU32>`,
    ///   then the bootstrap calls
    ///   [`crate::state::AppState::install_in_flight_publisher`] with that Arc.
    ///   `OnceLock::set` succeeds on the first call.
    /// * Hot reload (`replace_models_and_reload`): the new router is
    ///   constructed via [`InferenceRouter::with_in_flight_publisher`]
    ///   passing the *already-installed* Arc back in. The OnceLock
    ///   is unchanged; old router guards and new router guards share the
    ///   same atomic, so the counter stays accurate across the swap.
    ///
    /// Empty in tests and on storage-only nodes that never construct
    /// a `InferenceRouter`; gossip then emits
    /// `current_in_flight: None`, which is the legacy / "no signal"
    /// behaviour every scoring path handles correctly.
    pub local_in_flight_publisher: std::sync::OnceLock<Arc<std::sync::atomic::AtomicU32>>,
}
