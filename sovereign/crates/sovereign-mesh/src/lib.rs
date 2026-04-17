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
pub mod capabilities;
pub mod daemon;
pub mod loopback_guard;
pub mod project_http;
pub mod projects;
pub mod reindexer;
pub mod supervised_task;
pub mod deep_link;
pub mod gossip;
pub mod inference_adapter;
pub mod join;
pub mod knowledge_client;
pub mod mcp_router;
pub mod mesh_http;
pub(crate) mod oicp_select;
pub mod peer_inference;
pub mod persist;
pub mod state;
pub mod types;

pub use daemon::EmbeddedDaemon;
pub use deep_link::{DeepLink, parse_deep_link};
pub use state::MeshState;
pub use types::*;
