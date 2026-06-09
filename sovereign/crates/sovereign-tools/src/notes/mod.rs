// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 7 audit-hardening primitives.
//!
//! Lives next to `mcp_surface` and `spec_watcher` because all three
//! are concerned with what the agent _does_ (vs. tool implementations
//! in `code/`). The four extraction streams the audit relies on:
//!
//! - [`patterns`] — observe sequences/gaps in `tool_call_log` and
//!   write `source='observed'` notes (e.g. "investigated impact of X
//!   before modifying Y"). Phase 7.1.
//! - `commit_harvest` (in `sovereign_mesh::reindexer`) — harvest
//!   non-noisy git commit messages as `source='committed'` notes.
//!   Phase 7.1.
//! - [`nudge`] — surface a single-line "note worth recording?" hint
//!   when architectural signals fire (struct/trait/impl change,
//!   manifest write, spec-invariant code touch). Phase 7.1.
//! - `diff_extract` / `response_mine` — run an extraction prompt or
//!   regex library over diff-and-transcript to surface
//!   `source='extracted'` / `source='inferred'` notes. Phase 7.2.
//!
//! The audit's job is to merge all five sources (`agent`,
//! `committed`, `extracted`, `inferred`, `observed`) into one
//! reviewer-ready rollup. See `audit_cmd` in `sovereign-cli` for
//! the assembly side. Phase 7.3.

pub mod diff_extract;
pub mod diff_extract_backend;
pub mod nudge;
pub mod patterns;
pub mod response_mine;
