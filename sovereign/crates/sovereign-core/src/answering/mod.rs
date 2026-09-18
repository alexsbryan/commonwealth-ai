// SPDX-License-Identifier: AGPL-3.0-or-later
//! Answering's non-ATOS middleware pieces.
//!
//! These are the files from `sovereign-api`'s `middleware/` cluster that name
//! neither ATOS nor the host: the tool injector (a concrete [`Middleware`] over
//! `oicp_types::ToolDescriptor`) and the turn-fidelity switches. Domains
//! `REVIEW-build-answering-inversion` split the cluster by what each file
//! names — the ATOS middlewares moved to `sovereign-atos` — and landed these
//! two here, the Answering context's home (`quality/DAEMON_CORE.md` §4.2).
//!
//! [`Middleware`]: crate::middleware::Middleware
pub mod tool_injector;
pub mod turn_fidelity;

/// The repo root the answering pipeline is anchored to — the directory that
/// contains `.sovereign/features/`, used by `ApprovalGate` for git lookups and
/// by `ContextInjector` for reading `spec.md`.
///
/// Answering owns this fact (`quality/DAEMON_CORE.md` §4.2), so the reader
/// lives here and the daemon takes its value at construction rather than
/// keeping the path as its own field. `None` when the process was not started
/// in a repo-like context, which degrades the ATOS pipelines to a noop.
pub fn repo_root() -> Option<std::path::PathBuf> {
    std::env::current_dir().ok()
}
