// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node's host crate: assembly, the surface shells, the edge and the
//! adapters.
//!
//! `sovereign-daemon` is the host library at tier 5 in the `mesh-api` layer
//! beside `sovereign-mesh` (DAEMON_CORE.md §4.1). Not `sovereign-cli-daemon`:
//! at tier 6, `sovereign-cli-llm` and `sovereign-cli-dev` would depend on a
//! sibling host, which the layer map forbids, and `sovereign-mesh`'s own tests
//! could not reach it.
//!
//! Its modules follow DAEMON_CORE.md §3's classes, so each phase lands inside
//! one module family:
//!
//! - `assemble` — constructs the node's parts in dependency order
//! - `edge` — who may call, and a caller's dialect translated into the node's
//!   protocol (`client_auth`, `loopback_guard`, `local_only`, `headers`,
//!   `frontdoor`)
//! - `turn` — the turn surface
//! - `jobs` — the job surface and its drivers
//! - `node` — the node's own state
//! - `resources` — the corpus engine handle, the storage budget and the
//!   foreground signal
//! - `store` — the store surface
//! - `adapters` — one context's port implemented with another's capability
//!   (DAEMON_CORE.md §4.3)
//!
//! The mesh host cluster moved in whole at `dm-daemon-mesh-edge` (2026-09-17):
//! the assembly root (`daemon`, `daemon_services`), the 21 route shells, the
//! edge leaves (`local_only`, `loopback_guard`, `http_response`, `types`,
//! `slot_manifest`), the MCP mount (`mcp_router`, `mcp_config_http`) and the
//! four files carrying the daemon's `impl EmbeddedDaemon` (`media_reach`,
//! `origin_fanout`, `roster_repair`, `venue_host`) — one strongly-connected
//! component, so it moved in one commit (ralph/DECISIONS.md 2026-09-17).
//! Its `jobs` family followed at `dm-daemon-mesh-jobs` and its two `adapters`
//! (`newsworthy_host`, `work_atlas_broadcaster`) at `dm-daemon-mesh-adapters`
//! (2026-09-17).
//! `REVIEW-build-daemon-embedded-split` splits `daemon.rs` by owner next,
//! moving Fabric's membership operations back to `sovereign-mesh`. Its
//! consumers are `sovereign-cli-daemon`, `sovereign-cli-llm` for the
//! `MeshAdmin` variant and worker mode until Phase 6, and its own tests
//! (DAEMON_CORE.md §4.1).

pub mod admin_http;
pub mod assets_http;
pub mod atlas_builder;
pub mod atlas_http;
/// The auto-collaborate pull loop and its heartbeat verdict — ingest run as
/// background work (DAEMON_CORE.md §3.2, `jobs`).
pub mod auto_ingest;
pub mod auto_resume;
/// The composition half of `sovereign-cli-daemon`'s `daemon_cmd`
/// (DAEMON_CORE.md §4.1 row 4), moved whole at domains
/// `dm-daemon-cli-composition` (2026-09-17): the bootstrap phases, the
/// build/preflight pair, the tool registry, the solve surface, the
/// work-atlas wiring and the runtime-support leaves. `run_daemon` itself
/// stays with the binary — it owns log rotation, the memory watchdog and
/// the process exit code (DC §4 preamble), and calls these.
///
/// `bootstrap` and `tool_registry` are gated on `treesitter`: the bootstrap
/// mounts `project_http` and the registry registers `sovereign-tools`' code
/// tools, both of which are behind that feature. The daemon binary enables it
/// unconditionally.
#[cfg(feature = "treesitter")]
pub mod bootstrap;
pub mod build;
pub mod corpus_catalog_http;
pub mod corpus_maintenance;
pub mod corpus_watch_http;
pub mod daemon;
pub mod daemon_services;
pub mod discovery_policy;
pub mod documents_http;
pub mod enrich_http;
pub mod features_http;
pub mod governance_http;
pub mod http_response;
/// The `ingest:v1` `JobExecutor` — one corpus partition per unit
/// (DAEMON_CORE.md §3.2, `jobs`).
pub mod ingest_executor;
/// The daemon's insight surface (sv-surface rung 6): clip/list/search/delete
/// over the `InsightService` the commissioning host built.
pub mod insight_http;
/// The table every long-running route keeps its jobs in — one form,
/// six routes (ARCH principle 8).
pub mod job_registry;
pub mod landscape_digest_http;
pub mod lc_http;
pub mod listener_watch;
pub mod local_only;
pub mod loopback_guard;
pub mod mcp_config_http;
pub mod mcp_router;
pub mod media_reach;
pub mod mesh_http;
pub mod meshapp_http;
/// `corpus-engine`'s `NewsworthyHost` implemented over the roster, the
/// identity watch, the KV store and the engine handle (DAEMON_CORE.md §4.3,
/// `adapters`).
pub mod newsworthy_host;
pub mod notes_http;
pub mod ocr_install;
pub mod origin_fanout;
pub mod principal;
#[cfg(feature = "treesitter")]
pub mod project_http;
pub mod provider;
pub mod publish_http;
pub mod reading_http;
pub mod recipe_http;
pub mod recipe_project_http;
pub mod research_http;
pub mod roster_repair;
pub mod rpc_warm_http;
/// The daemon's `SlotManifest` port implementation over `sovereign-core`'s
/// bundled manifest; supplied to the serving host's inference adapter and
/// self-manifest advertisement (domains REVIEW-build-serving-move-adapter).
pub mod slot_manifest;
pub mod solve_http;
pub mod solve_tools;
pub mod startup;
pub mod supervise;
/// The panic-boundary supervisor every long-running watcher runs under
/// (DAEMON_CORE.md §3.2, `jobs`).
pub mod supervised_task;
/// The `/mcp` tool registry. Gated with `bootstrap`: it registers
/// `sovereign-tools`' code-intel tools, which are behind `treesitter`.
#[cfg(feature = "treesitter")]
pub mod tool_registry;
pub mod turn_extras_http;
pub mod turn_http;
pub mod types;
pub mod venue_host;
/// The watched-folder scheduler's process-wide singleton and its installer
/// (DAEMON_CORE.md §3.2, `jobs`).
pub mod watched_folder_runtime;
pub mod watched_folder_setup;
pub mod watcher_supervisor;
/// `sovereign-work-atlas`'s `ClaimBroadcaster` over the rail — the adapter
/// that hurries a claim onto the ring (DAEMON_CORE.md §4.3, `adapters`).
pub mod work_atlas_broadcaster;
/// The `work` donor loop — the node's own lease-and-run half of the work
/// plane (DAEMON_CORE.md §3.2, `jobs`).
pub mod work_donor;
pub mod worker;
pub mod workflow_trigger;
pub mod workspace;

// ── sovereign-api's host cluster (domains dm-daemon-api-edge, 2026-09-18) ──
// The whole host cluster moved in one commit: the edge, `state` with its six
// parts, `server` and the routes. Every `crate::<name>` path inside the moved
// modules keeps resolving here; the shims below cover the modules that had
// already left sovereign-api for a leaf, so `crate::<name>` keeps resolving
// for the moved modules too. `principal.rs` was already the daemon's own
// (the corpus-ceiling resolver from the cli-composition move), so the HTTP
// edge resolver landed beside it as `client_principal`.
pub mod admission;
pub mod client_auth;
/// The HTTP edge's one request-to-principal resolver. Named apart from
/// [`principal`] because the daemon already had a module by that name; the
/// design's one resolver (`REVIEW-mint-principal`) collapses them.
pub mod client_principal;
pub mod client_surface;
pub mod frontdoor;
pub mod headers;
pub mod middleware;
pub mod reshaping;
pub mod routes_apps;
pub mod routes_completions;
pub mod routes_edit_predictions;
pub mod routes_inference;
pub mod routes_internal;
pub mod routes_knowledge;
pub mod routes_oicp;
pub mod routes_oicp_ingest;
pub mod routes_ollama;
pub mod routes_rail;
pub mod routes_responses;
pub mod routes_status;
pub mod server;
pub mod state;
pub mod yield_hook;

// The shims sovereign-api's lib.rs carried, re-homed here so the moved
// modules' `crate::<name>` paths keep resolving.
pub use code_next_edit::next_edit;
pub use code_next_edit::next_edit_journal;
pub use code_next_edit::next_edit_model;
pub use code_next_edit::next_edit_symbols;
pub use code_next_edit::next_edit_syntax;
pub use commonwealth_core::{Error, Result};
pub use commonwealth_transport::fanout;
pub use oicp_types::openai_types;
pub use oicp_types::responses_types;
pub use sovereign_core::answering::turn_fidelity;
pub use sovereign_grants::auto_recover;

// Re-exports the moved modules reached through the mesh crate root, so their
// own `crate::` paths keep resolving here (the leaves live in the serving host
// and the scheduler, not in this crate).
pub use sovereign_core::deep_research::research_run_dir;
pub use sovereign_core::turn_approval;
pub use sovereign_scheduler::slot_aliases;
pub use sovereign_serving_host::inference_adapter;
pub use sovereign_serving_host::model_fetch;
pub use sovereign_serving_host::worker_eligibility;

pub use daemon::{ClientListener, EmbeddedDaemon};
pub use daemon_services::{
    assemble, AssemblyRefusal, DaemonServices, EmbedAdvertisement, HeadlessExtras, HeadlessRails,
    HeadlessServices, LaunchParts, McpMount, McpSurface, MeshAdminWitness, ServingCapability,
    ServingCore, ServingProfile,
};
pub use local_only::{LocalOnlyProfile, LocalOnlySource, MeshService, RunningServices};
pub use types::*;
pub use venue_host::DeferredDaemon;
