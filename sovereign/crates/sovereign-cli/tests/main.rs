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

#[path = "main/aliases.rs"]
mod aliases;
#[path = "main/cli_contract_code.rs"]
mod cli_contract_code;
#[path = "main/cli_contract_flags.rs"]
mod cli_contract_flags;
#[path = "main/cli_contract_docs.rs"]
mod cli_contract_docs;
#[path = "main/cli_contract_journeys.rs"]
mod cli_contract_journeys;
#[path = "main/cli_journey_dispatch.rs"]
mod cli_journey_dispatch;
#[path = "main/default_build_gate.rs"]
mod default_build_gate;
#[path = "main/phase3_serve_lifecycle.rs"]
mod phase3_serve_lifecycle;
#[path = "main/phase4_daemon_setup.rs"]
mod phase4_daemon_setup;
#[path = "main/phase6_retired_ceremony.rs"]
mod phase6_retired_ceremony;
