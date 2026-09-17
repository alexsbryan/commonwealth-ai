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
//! `REVIEW-build-daemon-embedded-split` splits `daemon.rs` by owner next,
//! moving Fabric's membership operations back to `sovereign-mesh`. Its
//! consumers are `sovereign-cli-daemon`, `sovereign-cli-llm` for the
//! `MeshAdmin` variant and worker mode until Phase 6, and its own tests
//! (DAEMON_CORE.md §4.1).

pub mod admin_http;
pub mod assets_http;
pub mod atlas_http;
pub mod corpus_catalog_http;
pub mod corpus_watch_http;
pub mod daemon;
pub mod daemon_services;
pub mod documents_http;
pub mod enrich_http;
pub mod features_http;
pub mod governance_http;
pub mod http_response;
/// The daemon's insight surface (sv-surface rung 6): clip/list/search/delete
/// over the `InsightService` the commissioning host built.
pub mod insight_http;
pub mod landscape_digest_http;
pub mod lc_http;
pub mod local_only;
pub mod loopback_guard;
pub mod mcp_config_http;
pub mod mcp_router;
pub mod media_reach;
pub mod mesh_http;
pub mod meshapp_http;
pub mod notes_http;
pub mod origin_fanout;
#[cfg(feature = "treesitter")]
pub mod project_http;
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
pub mod turn_extras_http;
pub mod turn_http;
pub mod types;
pub mod venue_host;

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
