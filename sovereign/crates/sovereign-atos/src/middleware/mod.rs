// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ATOS middleware cluster — Answering's ATOS surface.
//!
//! These middlewares read and write ATOS state (approvals, the session
//! handle, the feature store). They lived in `sovereign-api`'s `middleware/`
//! directory and reached UP into `sovereign_atos`; domains
//! `REVIEW-build-answering-inversion` inverted that edge — the middlewares
//! moved to ATOS and the host installs them through [`registrations`].
//!
//! The composition (`MiddlewareRegistry`, `Pipeline`) stays host code
//! (`quality/DAEMON_CORE.md` §4.2 "Risks carried"). The seam (`Middleware`,
//! `MiddlewareSession`, `PipelineContext`, `MiddlewareError`, `ResponseView`)
//! lives in `sovereign-contracts` and is re-exported here so the middlewares
//! name it exactly as they did beside `shared.rs`.

use std::sync::Arc;

pub mod approval_gate;
pub mod context_injector;
pub mod session_briefing;

pub use approval_gate::ApprovalGate;
pub use context_injector::ContextInjector;
pub use session_briefing::SessionBriefing;

// The seam and the shared `.sovereign/` conventions, re-exported so the
// submodules reach them through `super::`.
pub use sovereign_core::middleware::{
    features_db_path, notes_db_path, prepend_to_system, Middleware, MiddlewareError,
    MiddlewareSession, PipelineContext, ResponseView,
};

/// The ATOS middlewares, in registration order, for the host's registry.
///
/// This is the inversion's entry point: the daemon's bootstrap calls it and
/// registers each middleware under its own id, so the host never names an
/// ATOS middleware type. The registry itself is host code and stays with the
/// daemon (`quality/DAEMON_CORE.md` §4.2 "Risks carried").
pub fn registrations() -> Vec<Arc<dyn Middleware>> {
    vec![
        Arc::new(ApprovalGate::new()),
        Arc::new(ContextInjector::empty()),
        Arc::new(SessionBriefing::new()),
    ]
}
