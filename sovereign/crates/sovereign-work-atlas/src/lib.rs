// SPDX-License-Identifier: AGPL-3.0-or-later
//! Work Atlas — coordination layer for agents sharing a mesh repo.
//!
//! See `sovereign/docs/WORK_ATLAS.md` for the why; this crate is the
//! Phase 1 implementation of the v0.1 spec — Sessions + Claims, no
//! Observations yet.
//!
//! Public surface:
//! - [`WorkAtlasStore`] — typed facade over a
//!   [`sovereign_contracts::peer::PeerStore`].
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

// The port `WorkAtlasStore::new` takes, re-exported because it is in this
// crate's public signature: a caller must be able to name the trait it is
// handing over without taking its own dependency on the contracts crate.
pub use confidence::ConfidenceGrade;
pub use config::WorkAtlasConfig;
pub use model::{AgentKind, ClaimRecord, ObservationRecord, Privacy, SessionRecord, SymbolRef};
pub use observer::AtlasObserver;
pub use repo_id::{resolve_repo_id, resolve_repo_id_allowing_local, RepoIdError, RepoIdSource};
pub use sovereign_contracts::peer::PeerStore;
pub use store::{ScopeMatch, SessionIdentity, WorkAtlasError, WorkAtlasStore};
