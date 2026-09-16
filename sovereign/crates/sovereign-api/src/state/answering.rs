//! Answering's part of the node's state — the ATOS middleware registry, the
//! session store and the repo root the daemon is anchored to.
//!
//! DC §4.2 assigns these three to Answering, whose home is `sovereign-core`'s
//! pipeline after the ATOS inversion. Until that move this part is scaffolding
//! carried on `AppStateInner`; the route shells read it directly rather than
//! through delegating accessors.

use std::sync::Arc;

/// Answering's three fields, held as `AppStateInner::answering`.
pub struct AnsweringPart {
    /// ATOS middleware registry. Holds one instance of each
    /// middleware the pipelines can reference by id.
    pub middleware_registry: Arc<crate::middleware::MiddlewareRegistry>,
    /// ATOS session-state store. `None` until a M4.4+ daemon wires
    /// it (tests without a MeshStore handle leave this empty; the
    /// handler skips ATOS pipeline processing when the store is
    /// absent). ATOS-only — absent entirely in product builds.
    #[cfg(feature = "atos")]
    pub session_store: Option<sovereign_atos::session::SessionStore>,
    /// Repository root the Commonwealth daemon is anchored to —
    /// the directory that contains `.sovereign/features/`. Used by
    /// ApprovalGate for git lookups and by ContextInjector for
    /// reading spec.md. `None` when the daemon wasn't started in a
    /// repo-like context (degrades ATOS pipelines to a noop).
    pub repo_root: Option<std::path::PathBuf>,
}
