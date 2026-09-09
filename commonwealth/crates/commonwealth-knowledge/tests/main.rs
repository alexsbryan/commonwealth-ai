// SPDX-License-Identifier: AGPL-3.0-or-later
//! One integration-test binary for this crate.
//!
//! Same shape as `commonwealth-api/tests/main.rs`: each source lives in
//! `tests/main/` and is declared here with `#[path]`, so cargo links ONE
//! executable instead of one per file. `#[path]` is load-bearing —
//! `tests/main.rs` is a crate root, so a bare `mod foo;` would resolve to
//! `tests/foo.rs`, which cargo would then also link as its own target.

#[path = "main/merge_participants_coverage.rs"]
mod merge_participants_coverage;
