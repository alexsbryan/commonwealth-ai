// SPDX-License-Identifier: AGPL-3.0-or-later
//! Privacy posture of a search-query egress — the boundary's
//! consultation type.
//!
//! Lives in the shared contract crate (not the studio tools-base) so
//! the egress boundary in sovereign-core (`egress.rs`) can consult it
//! without a studio dependency (ARCH_LAYERS.toml: sovereign-core may
//! not depend on sovereign-tools-base). `sovereign-tools-base`
//! re-exports it at its historical `web::search` path so importers
//! are unaffected.

/// Privacy posture for a search backend. Drives orchestrator-side
/// filtering: a request with OICP `LocalOnly` privacy must only see
/// `Local` backends. Per ARCH §7.1, this is encoded on the backend
/// itself rather than passed as a parameter — a caller cannot flip
/// it via config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPrivacy {
    /// Query never leaves this node. Mock fixtures, an in-process
    /// internal-corpus search, anything that doesn't make a network
    /// call to a third party.
    Local,
    /// Query may be sent to mesh peers (federated knowledge search).
    /// Acceptable when the request's OICP privacy is `MeshAllowed`
    /// or `External`. Not used in Phase 0 — placeholder for the
    /// federated search workstream.
    Mesh,
    /// Query goes to an external provider (Tavily, Brave, …). The
    /// `provider` field is the stable id used in tracing + budget
    /// accounting; it must match the backend's `id()` for
    /// audit-log correlation.
    External { provider: &'static str },
}

impl SearchPrivacy {
    /// Total ordering for the orchestrator's "max privacy" filter:
    /// `Local <= Mesh <= External`. A request with `max_privacy =
    /// Local` may only use `Local` backends; with `max_privacy =
    /// External` any backend is allowed.
    pub fn rank(&self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Mesh => 1,
            Self::External { .. } => 2,
        }
    }
}
