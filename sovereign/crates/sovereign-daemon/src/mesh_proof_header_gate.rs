// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ratchet behind "one outbound stamp" — a test, not a comment.
//!
//! `x-mesh-proof` is minted in exactly one place
//! (`commonwealth_transport::mesh_proof`), and since the flip nothing in
//! svrn reads it: svrn's internal port believes only cw-rails' registration
//! tie (`sovereign_daemon::internal_principal`, pb-mesh-exit-transport). A
//! second production file spelling the literal would be a second sender or a
//! reader of a credential nobody checks — the shape ARCH principle 8 forbids,
//! and the shape `x-node-id` had before [`crate::mesh_principal_gate`]
//! closed it: nine sites, each deciding for itself what the header meant.
//!
//! A grep and not a lint for the reason that module gives: a comment saying
//! "mint it through the one function" is exactly what a drifting call site
//! ignores. This fails the normal test run instead, naming the file
//! (`quality/xtask/tests/mesh_proof_header_gate.rs`: it scans the
//! monorepo, which a lifted svrn does not carry).
//!
//! Both trees are walked, because the minting side is in `commonwealth/` and
//! a reader would be in `sovereign/`. Test code is exempt twice over —
//! `tests/` trees are not walked, and everything from a file's first
//! `#[cfg(test)]` on is skipped — because typing the header is how a refusal
//! is proved.

/// The one production file that may spell the literal: the one that mints
/// it. Repo-relative, matched as a suffix so the test runs from any cwd.
pub const PROOF_HEADER_ALLOWED: &[&str] =
    &["commonwealth/crates/commonwealth-transport/src/mesh_proof.rs"];

/// The wire form, in both spellings a Rust file could hold it in.
pub const PROOF_WIRE_FORM: &[&str] = &["x-mesh-proof", "X-Mesh-Proof"];
