//! Work Atlas — coordination layer for agents sharing a mesh repo.
//!
//! See `sovereign/docs/WORK_ATLAS.md` for the why; this crate is the
//! Phase 1 implementation of the v0.1 spec — Sessions + Claims, no
//! Observations yet.
//!
//! Public surface:
//! - [`WorkAtlasStore`] — typed facade over [`commonwealth_state::MeshStore`].
//! - [`tools`] module — the three MCP tools.
//! - [`gc::WorkAtlasGc`] — TTL eviction task spawned by the daemon.
//! - [`config::WorkAtlasConfig`] — toml-backed operator settings.

pub mod confidence;
pub mod config;
pub mod gc;
pub mod model;
pub mod observer;
pub mod repo_id;
pub mod store;
pub mod tools;

pub use confidence::ConfidenceGrade;
pub use config::WorkAtlasConfig;
pub use model::{AgentKind, ClaimRecord, ObservationRecord, Privacy, SessionRecord, SymbolRef};
pub use observer::AtlasObserver;
pub use repo_id::{resolve_repo_id, RepoIdError};
pub use store::{ScopeMatch, SessionIdentity, WorkAtlasError, WorkAtlasStore};
