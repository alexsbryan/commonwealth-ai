// SPDX-License-Identifier: AGPL-3.0-or-later
//! One integration-test binary for this crate.
//!
//! Each former `tests/<name>.rs` is now `tests/main/<name>.rs`, declared
//! below with `#[path]`, so cargo links ONE executable instead of one per
//! file. Every test still runs; its name gains the module path as a prefix,
//! so a filter that named a file now names a module:
//!
//!     cargo test -p sovereign-serving-host --test main <module>::
//!
//! `#[path]` is load-bearing: `tests/main.rs` is a CRATE ROOT, so a bare
//! `mod foo;` resolves to `tests/foo.rs` — which cargo would then also link
//! as its own test binary, which is the thing this file exists to stop. The
//! attribute keeps the sources in `tests/main/`, a directory cargo does not
//! scan for targets.
//!
//! The serving package's physical lift runs one of these by name:
//! `scripts/serving-lift.sh` steps 5-8 drive
//! [`serving_lift_harness`](serving_lift_harness) inside the sandbox and grep
//! its `LIFT ` evidence lines.

#[path = "main/serving_lift_harness.rs"]
mod serving_lift_harness;
