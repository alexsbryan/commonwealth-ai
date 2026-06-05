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
pub mod deep_link;
pub mod gossip;
pub mod http_response;
pub mod inference_adapter;
pub mod join;
pub mod knowledge_client;
pub mod landscape_digest_client;
pub mod landscape_digest_http;
pub mod loopback_guard;
pub mod mcp_router;
pub mod mesh_discovery;
pub mod mesh_http;
pub mod model_fetch;
pub mod newsworthy_host;
pub(crate) mod oicp_select;
pub mod oicp_synthesis;
pub mod peer_inference;
pub mod persist;
#[cfg(feature = "treesitter")]
pub mod project_http;
pub mod projects;
pub mod prompt_compactor;
pub mod reading_formatters;
pub mod reading_http;
#[cfg(feature = "treesitter")]
pub mod reindexer;
pub mod rpc_warm_http;
pub mod source_content_validator;
pub mod state;
pub mod supervised_task;
pub mod throughput_tracking;
pub mod tool_profile;
pub mod types;
pub mod watched_folder_runtime;
pub mod watched_folder_setup;
pub mod work_atlas_broadcaster;
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
pub use deep_link::{parse_deep_link, DeepLink};
pub use state::MeshState;
pub use types::*;
pub use work_atlas_broadcaster::MeshBroadcaster;
