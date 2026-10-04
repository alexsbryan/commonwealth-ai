// SPDX-License-Identifier: AGPL-3.0-or-later
//! UI-friendly types for mesh status, member info, and contributions.
//!
//! Defined in `sovereign_contracts::daemon_wire::mesh` since sv-surface
//! svt-3 (2026-09-11) — they are what a client parses off `/v1/mesh/status`
//! and `/v1/mesh/join/preview`, and a client that only parses them should
//! not link this crate to name them. Re-exported here so `state.rs`,
//! `deep_link.rs`, the CLI and `pub use types::*` in `lib.rs` are unchanged.

pub use sovereign_contracts::daemon_wire::{
    ContributionSummary, CorpusStatus, JoinConfirmation, MemberStatus, MeshCorpus, MeshMember,
    MeshStatus,
};
