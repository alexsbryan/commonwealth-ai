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
pub mod auto_ingest;
pub mod auto_resume;
pub mod canonical_pull;
pub mod capabilities;
pub mod commit_harvest;
pub mod corpus_watch_http;
pub mod daemon;
pub mod daemon_services;
/// Routing decision records — Phase 0 (P1/P2) of
/// `docs/specs/SCHEDULER_QUALITY.md`. One structured record per
/// routing decision (full candidate set, every scorer input stamped
/// with its provenance and age) joined by `decision_id` to one record
/// per completion (served-by / TTFT / total / tokens / shed). Pure
/// instrumentation: it changes no routing decision.
pub mod decision_log;
/// Decision replay — Phase 1 (S1). Re-runs the live scorer and the
/// live ranking policy over a captured `decision_log` record and
/// reports whether the record reproduces its own scores and verdict.
pub mod decision_replay;
/// Trace-replay fixtures — Phase 0 (P3/P4). Reads a `decision_log`
/// JSONL stream plus an observation-state snapshot back into the
/// episode the Tier-1 simulator replays.
pub mod decision_trace;
pub mod deep_link;
#[cfg(feature = "dst")]
pub mod dst;
pub mod entry_endpoint;
pub mod fim_adapter;
pub mod gossip;
pub mod guest_lender;
pub mod guest_tunnel;
pub mod http_response;
pub mod inference_adapter;
/// Dial-by-key mesh access over iroh (Track W, W1). Server half: binds
/// the daemon's identity endpoint and routes by ALPN to the local
/// internal + client listeners. Runtime-gated by `[iroh] enabled`.
pub mod iroh_access;
pub mod iroh_watchdog;
pub mod join;
pub mod knowledge_client;
pub mod landscape_digest_client;
pub mod landscape_digest_http;
pub mod loopback_guard;
pub mod mcp_router;
pub mod mesh_discovery;
pub mod mesh_http;
/// Tier-1 scheduler simulator — `SCHEDULER_QUALITY.md` §5. Behind a
/// feature flag beside `dst`: same crate (only this crate can name
/// the scheduler's internals), same "never in a production build"
/// rationale.
#[cfg(feature = "mesh-sim")]
pub mod mesh_sim;
pub mod model_fetch;
pub mod newsworthy_host;
pub(crate) mod oicp_select;
pub mod oicp_synthesis;
pub mod peer_inference;
pub mod persist;
/// The §4.1 candidate objective — rank on predicted time-to-answer
/// rather than on a product of dimensionless multipliers
/// (`SCHEDULER_QUALITY.md` §4.1). Public because it is scored from a
/// capture as well as from the live path.
pub mod predicted_time;
#[cfg(feature = "treesitter")]
pub mod project_http;
pub mod projects;
pub mod prompt_compactor;
pub mod reading_formatters;
pub mod reading_http;
#[cfg(feature = "treesitter")]
pub mod reindexer;
pub mod roster_repair;
pub mod rpc_warm_http;
/// The routing decision as a pure function — shared by the production
/// selector and the Tier-1 simulator (`SCHEDULER_QUALITY.md` §5).
pub(crate) mod scheduler_core;
pub mod slot_aliases;
pub mod source_content_validator;
pub mod state;
pub mod supervised_task;
pub mod throughput_tracking;
/// Capability bands — the tier floor of `SCHEDULER_QUALITY.md` §4.1:
/// capability filters the candidate set, predicted cost ranks what
/// survives.
pub mod tier;
pub mod tool_profile;
pub mod turn_http;
pub mod types;
pub mod watched_folder_runtime;
pub mod watched_folder_setup;
pub mod work_atlas_broadcaster;
pub mod worker_eligibility;
// Short-lived memory of peers that refused with `yielded_to_local`, so
// the next turn does not re-dial into the same refusal.
pub mod yield_backoff;
// Ephemeral worker pods — owner-initiated TLS-pinned transport that
// replaces the full-mesh-pod path. Pods become single-owner workers,
// not gossip peers. Spec: sovereign/docs/EPHEMERAL_WORKER_PODS.md.
pub mod multi_pod_coordinator;
pub mod worker_controller;
pub mod worker_daemon;
pub mod worker_http;
pub mod worker_inference_proxy;
pub mod worker_pod;
pub mod worker_subprocess_runner;
// Pinned-pod inference routing — lets ephemeral worker pods join the
// mesh scheduler's inference pool as one more peer, scored by the
// same load balancer. Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md.
pub mod pinned_pod_snapshot;
pub mod pinned_transport;
pub mod pinned_worker_source;

pub use daemon::EmbeddedDaemon;
pub use daemon_services::{
    assemble, AssemblyRefusal, DaemonServices, EmbedAdvertisement, HeadlessExtras, HeadlessRails,
    HeadlessServices, LaunchParts, McpMount, McpSurface, MeshAdminWitness, ServingCapability,
    ServingCore, ServingProfile,
};
pub use deep_link::{parse_deep_link, DeepLink};
pub use peer_inference::DeferredDaemon;
pub use state::MeshState;
pub use types::*;
pub use work_atlas_broadcaster::MeshBroadcaster;
