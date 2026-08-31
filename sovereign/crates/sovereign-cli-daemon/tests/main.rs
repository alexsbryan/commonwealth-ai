// SPDX-License-Identifier: AGPL-3.0-or-later
//! One integration-test binary for this crate.
//!
//! Each former `tests/<name>.rs` is now `tests/main/<name>.rs`, declared
//! below, so cargo links ONE executable instead of one per file. Every
//! test still runs; its name gains the module path as a prefix, so a
//! filter that named a file now names a module:
//!
//!     cargo test -p <crate> --test main <module>::
//!
//! `#[path]` is load-bearing: `tests/main.rs` is a CRATE ROOT, so a bare
//! `mod foo;` resolves to `tests/foo.rs` — which cargo would then also
//! link as its own test binary, which is the thing this file exists to
//! stop. The attribute keeps the sources in `tests/main/`, a directory
//! cargo does not scan for targets.
//!
//! Files still sitting directly in `tests/` are there on purpose — they
//! need process isolation, or a `.config/nextest.toml` override keys on
//! their binary name. Do not fold those in.

#[path = "main/compute_child_e2e.rs"]
mod compute_child_e2e;
#[path = "main/containment_guard_e2e.rs"]
mod containment_guard_e2e;
#[path = "main/distributed_primary_respawn_e2e.rs"]
mod distributed_primary_respawn_e2e;
#[path = "main/named_model_routes_after_child_serves_e2e.rs"]
mod named_model_routes_after_child_serves_e2e;
