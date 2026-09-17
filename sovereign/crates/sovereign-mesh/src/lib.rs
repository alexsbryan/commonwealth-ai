// SPDX-License-Identifier: AGPL-3.0-or-later
//! sovereign-mesh — Commonwealth mesh integration layer for Sovereign.
//!
//! This crate embeds the Commonwealth daemon as a library, managing its
//! lifecycle from within Sovereign. Users never need a separate binary
//! or terminal — mesh operations happen through Sovereign's UI.
//!
//! Key responsibilities:
//! - Start/stop the embedded Commonwealth daemon
//! - Parse `sovereign://join/...` deep links
//! - Translate mesh state into UI-friendly representations
//! - Expose mesh operations for both GUI and CLI
//!
//! The mesh host cluster — `daemon`, `daemon_services`, the 21 route shells,
//! the edge leaves and the MCP mount — moved to `sovereign-daemon` at
//! `dm-daemon-mesh-edge` (2026-09-17; ralph/DECISIONS.md), and the `jobs`
//! family (the auto-collaborate loop, the resume sweep, the ingest executor,
//! the watched-folder scheduler, the job table, the watcher supervisor and the
//! work donor) followed at `dm-daemon-mesh-jobs` (2026-09-17). Fabric keeps
//! its own modules here; the host observes them through readers.

pub mod canonical_pull;
pub mod capabilities;
pub mod commit_harvest;
/// Routing decision records — Phase 0 (P1/P2) of
/// `docs/specs/SCHEDULER_QUALITY.md`. One structured record per
/// routing decision (full candidate set, every scorer input stamped
/// with its provenance and age) joined by `decision_id` to one record
/// per completion (served-by / TTFT / total / tokens / shed). Pure
/// instrumentation: it changes no routing decision.
pub use sovereign_scheduler::decision_log; // shim: moved by domains REVIEW-build-sched-move
/// Decision replay — Phase 1 (S1). Re-runs the live scorer and the
/// live ranking policy over a captured `decision_log` record and
/// reports whether the record reproduces its own scores and verdict.
pub use sovereign_scheduler::decision_replay; // shim: moved by domains REVIEW-build-sched-move
/// Trace-replay fixtures — Phase 0 (P3/P4). Reads a `decision_log`
/// JSONL stream plus an observation-state snapshot back into the
/// episode the Tier-1 simulator replays.
pub use sovereign_scheduler::decision_trace; // shim: moved by domains REVIEW-build-sched-move
pub mod deep_link;
pub use deep_link::{parse_deep_link, DeepLink};
pub mod gossip;
pub use sovereign_serving_host::guest_lender; // shim: moved by domains REVIEW-build-serving-move-throughput-guest
pub mod guest_source;
pub mod guest_tunnel;
pub use sovereign_serving_host::inference_adapter; // shim: moved by domains REVIEW-build-serving-move-adapter
/// Dial-by-key mesh access over iroh (Track W, W1). Server half: binds
/// the daemon's identity endpoint and routes by ALPN to the local
/// internal + client listeners. Runtime-gated by `[iroh] enabled`.
pub mod iroh_access;
pub mod iroh_watchdog;
pub mod join;
pub use sovereign_turn_client::knowledge_client; // shim: moved by domains REVIEW-build-mesh-client-pair
pub use sovereign_turn_client::landscape_digest_client; // shim: moved by domains REVIEW-build-mesh-client-pair
#[cfg(feature = "treesitter")]
pub mod lsp_tier;
pub mod measurements_rail;
pub mod mesh_discovery;
/// Tier-1 scheduler simulator — `SCHEDULER_QUALITY.md` §5. Behind a
/// feature flag beside `dst`: same crate (only this crate can name
/// the scheduler's internals), same "never in a production build"
/// rationale.
#[cfg(feature = "mesh-sim")]
pub mod mesh_sim;
pub use sovereign_serving_host::model_fetch; // shim: moved by domains dm-serving-move-leaves
pub mod newsworthy_host;
pub(crate) use sovereign_scheduler::oicp_select; // shim: moved by domains REVIEW-build-sched-move
/// The mesh implementations of `sovereign-contracts::peer`'s two ports — the
/// N>1 half of what the daemon speaks to its peers through (cw-lift 3b).
pub mod peer_adapter;
pub use sovereign_serving_host::peer_inference; // shim: moved by domains REVIEW-build-serving-move-peer
pub mod persist;
/// The §4.1 candidate objective — rank on predicted time-to-answer
/// rather than on a product of dimensionless multipliers
/// (`SCHEDULER_QUALITY.md` §4.1). Public because it is scored from a
/// capture as well as from the live path.
pub use sovereign_scheduler::predicted_time; // shim: moved by domains REVIEW-build-sched-move
pub mod projects;
pub mod rail_bind;
pub mod rail_kv_pump;
pub use corpus_engine_vocab::reading_formatters; // shim: moved by domains dm-mesh-move-reading-formatters
#[cfg(feature = "treesitter")]
pub mod reindexer;
pub use sovereign_core::deep_research::research_run_dir; // shim: moved by domains REVIEW-build-research-run-dir
pub mod ring_roster;
pub mod ring_sync;
/// The routing decision as a pure function — shared by the production
/// selector and the Tier-1 simulator (`SCHEDULER_QUALITY.md` §5).
pub(crate) use sovereign_scheduler::scheduler_core; // shim: moved by domains REVIEW-build-sched-move
pub use sovereign_scheduler::slot_aliases; // shim: moved by domains dm-sched-move-slot-aliases
pub mod state;
/// Capability bands — the tier floor of `SCHEDULER_QUALITY.md` §4.1:
/// capability filters the candidate set, predicted cost ranks what
/// survives.
pub use sovereign_scheduler::tier;
pub use sovereign_serving_host::throughput_tracking; // shim: moved by domains REVIEW-build-serving-move-throughput-guest

pub use sovereign_core::turn_approval; // shim: moved by domains dm-mesh-move-turn-approval
pub mod work_atlas_broadcaster;
pub use sovereign_serving_host::worker_eligibility; // shim: moved by domains dm-serving-move-leaves
                                                    // Ephemeral worker pods — owner-initiated TLS-pinned transport that
                                                    // replaces the full-mesh-pod path. Pods become single-owner workers,
                                                    // not gossip peers. Spec: sovereign/docs/EPHEMERAL_WORKER_PODS.md.
pub use sovereign_contracts::worker_pod; // shim: moved by domains REVIEW-build-serving-worker-port
                                         // Pinned-pod inference routing — lets ephemeral worker pods join the
                                         // mesh scheduler's inference pool as one more peer, scored by the
                                         // same load balancer. Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md.
pub use sovereign_serving_host::pinned_pod_snapshot; // shim: moved by domains REVIEW-build-serving-move-peer
pub use sovereign_serving_host::pinned_worker_source; // shim: moved by domains REVIEW-build-serving-move-peer

pub use state::MeshState;
pub use work_atlas_broadcaster::MeshBroadcaster;
