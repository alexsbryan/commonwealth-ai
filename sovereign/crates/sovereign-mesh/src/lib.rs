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

pub mod admin_http;
pub mod assets_http;
pub mod atlas_http;
pub mod auto_ingest;
pub mod auto_resume;
pub mod canonical_pull;
pub mod capabilities;
pub mod commit_harvest;
pub mod corpus_catalog_http;
pub mod corpus_watch_http;
pub mod daemon;
pub mod daemon_services;
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
pub mod documents_http;
#[cfg(feature = "dst")]
pub mod dst;
pub mod enrich_http;
pub use sovereign_serving_host::entry_endpoint; // shim: moved by domains REVIEW-build-serving-move-peer
pub mod features_http;
pub use sovereign_serving_host::fim_adapter; // shim: moved by domains REVIEW-build-serving-move-adapter
pub mod gossip;
pub mod governance_http;
pub use sovereign_serving_host::guest_lender; // shim: moved by domains REVIEW-build-serving-move-throughput-guest
pub mod guest_source;
pub mod guest_tunnel;
pub mod http_response;
pub use sovereign_serving_host::inference_adapter; // shim: moved by domains REVIEW-build-serving-move-adapter
pub mod ingest_executor;
/// The daemon's insight surface (sv-surface rung 6): clip/list/search/delete
/// over the `InsightService` the commissioning host built.
pub mod insight_http;
/// Dial-by-key mesh access over iroh (Track W, W1). Server half: binds
/// the daemon's identity endpoint and routes by ALPN to the local
/// internal + client listeners. Runtime-gated by `[iroh] enabled`.
pub mod iroh_access;
pub mod iroh_watchdog;
/// The table every long-running route keeps its jobs in — one form,
/// six routes (ARCH principle 8).
pub mod job_registry;
pub mod join;
pub mod knowledge_client;
pub mod landscape_digest_client;
pub mod landscape_digest_http;
pub mod lc_http;
pub mod local_only;
pub mod loopback_guard;
#[cfg(feature = "treesitter")]
pub mod lsp_tier;
pub mod mcp_config_http;
pub mod mcp_router;
pub mod measurements_rail;
pub mod media_reach;
pub mod mesh_discovery;
pub mod mesh_http;
/// Tier-1 scheduler simulator — `SCHEDULER_QUALITY.md` §5. Behind a
/// feature flag beside `dst`: same crate (only this crate can name
/// the scheduler's internals), same "never in a production build"
/// rationale.
#[cfg(feature = "mesh-sim")]
pub mod mesh_sim;
pub mod meshapp_http;
pub use sovereign_serving_host::model_fetch; // shim: moved by domains dm-serving-move-leaves
pub mod newsworthy_host;
pub mod notes_http;
pub(crate) use sovereign_scheduler::oicp_select; // shim: moved by domains REVIEW-build-sched-move
pub mod origin_fanout;
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
#[cfg(feature = "treesitter")]
pub mod project_http;
pub mod projects;
pub mod publish_http;
pub mod rail_bind;
pub mod rail_kv_pump;
pub mod reading_formatters;
pub mod reading_http;
pub mod recipe_http;
pub mod recipe_project_http;
#[cfg(feature = "treesitter")]
pub mod reindexer;
pub mod research_http;
pub mod research_run_dir;
pub mod ring_roster;
pub mod ring_sync;
pub mod roster_repair;
pub mod rpc_warm_http;
/// The routing decision as a pure function — shared by the production
/// selector and the Tier-1 simulator (`SCHEDULER_QUALITY.md` §5).
pub(crate) use sovereign_scheduler::scheduler_core; // shim: moved by domains REVIEW-build-sched-move
pub use sovereign_scheduler::slot_aliases; // shim: moved by domains dm-sched-move-slot-aliases
/// The daemon's `SlotManifest` port implementation over `sovereign-core`'s
/// bundled manifest; supplied to the serving host's inference adapter and
/// self-manifest advertisement (domains REVIEW-build-serving-move-adapter).
pub mod slot_manifest;
pub use sovereign_serving_host::source_content_validator; // shim: moved by domains dm-serving-move-leaves
pub mod state;
pub mod supervised_task;
/// Capability bands — the tier floor of `SCHEDULER_QUALITY.md` §4.1:
/// capability filters the candidate set, predicted cost ranks what
/// survives.
pub use sovereign_scheduler::tier;
pub use sovereign_serving_host::throughput_tracking; // shim: moved by domains REVIEW-build-serving-move-throughput-guest
pub use sovereign_serving_host::tool_profile; // shim: moved by domains REVIEW-build-serving-move-adapter
pub mod turn_approval;
pub mod turn_extras_http;
pub mod turn_http;
pub mod types;
pub mod venue_host;
pub mod watched_folder_runtime;
pub mod watched_folder_setup;
pub mod work_atlas_broadcaster;
pub mod work_donor;
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

pub use daemon::{ClientListener, EmbeddedDaemon};
pub use daemon_services::{
    assemble, AssemblyRefusal, DaemonServices, EmbedAdvertisement, HeadlessExtras, HeadlessRails,
    HeadlessServices, LaunchParts, McpMount, McpSurface, MeshAdminWitness, ServingCapability,
    ServingCore, ServingProfile,
};
pub use deep_link::{parse_deep_link, DeepLink};
pub use local_only::{LocalOnlyProfile, LocalOnlySource, MeshService, RunningServices};
pub use state::MeshState;
pub use types::*;
pub use venue_host::DeferredDaemon;
pub use work_atlas_broadcaster::MeshBroadcaster;
