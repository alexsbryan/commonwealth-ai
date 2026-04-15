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

pub mod daemon;
pub mod deep_link;
pub mod gossip;
pub mod join;
pub mod persist;
pub mod state;
pub mod types;

pub use daemon::EmbeddedDaemon;
pub use deep_link::{DeepLink, parse_deep_link};
pub use state::MeshState;
pub use types::*;
