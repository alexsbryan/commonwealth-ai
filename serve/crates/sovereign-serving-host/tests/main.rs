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

#[path = "main/manifest_fanout_concurrency.rs"]
mod manifest_fanout_concurrency;

// The Tier-1 scheduler simulator (moved from sovereign-mesh-test-harness,
// pb-mesh-dissolve): the ranker's own instrument, so it lives beside the
// ranker's tests and no build links it. It was a library, and five of its
// report accessors are read by no test here; they are the instrument's
// surface, kept as moved.
#[allow(dead_code)]
#[path = "main/mesh_sim/mod.rs"]
mod mesh_sim;

#[path = "main/mesh_sim_ring_room.rs"]
mod mesh_sim_ring_room;

#[path = "main/mesh_sim_scoreboard.rs"]
mod mesh_sim_scoreboard;

#[path = "main/openai_finish_reason.rs"]
mod openai_finish_reason;

#[path = "main/scheduler_decision_records.rs"]
mod scheduler_decision_records;

#[path = "main/scheduler_replay_agreement.rs"]
mod scheduler_replay_agreement;

#[path = "main/serving_lift_harness.rs"]
mod serving_lift_harness;

#[path = "main/sheds_commonwealth.rs"]
mod sheds_commonwealth;

#[path = "main/throughput_ledger_emission.rs"]
mod throughput_ledger_emission;
