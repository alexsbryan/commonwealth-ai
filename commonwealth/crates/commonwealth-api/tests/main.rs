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

#[path = "main/app_state_privacy.rs"]
mod app_state_privacy;
#[path = "main/client_auth.rs"]
mod client_auth;
#[path = "main/corpus_lifecycle.rs"]
mod corpus_lifecycle;
#[path = "main/gossip_route.rs"]
mod gossip_route;
#[path = "main/join_route.rs"]
mod join_route;
#[path = "main/knowledge_fanout.rs"]
mod knowledge_fanout;
#[path = "main/next_edit_symbol_lane_e2e.rs"]
mod next_edit_symbol_lane_e2e;
#[path = "main/openai_wire_fidelity.rs"]
mod openai_wire_fidelity;
#[path = "main/turn_reshape_fidelity.rs"]
mod turn_reshape_fidelity;
#[path = "main/storage_budget_route.rs"]
mod storage_budget_route;
