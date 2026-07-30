// SPDX-License-Identifier: AGPL-3.0-or-later
//! Neutral helpers shared across every `sovereign-cli-*` binary.
//!
//! Lives outside `sovereign-cli` so the new per-domain binaries
//! (`sovereign-cli-atos`, future `sovereign-cli-meta`, etc.) can
//! depend on these helpers without dragging in all of the dispatcher
//! crate. Each module is kept small on purpose — the test for adding
//! anything here is "would forcing every CLI binary to link this still
//! be cheap?"
//!
//! Filesystem + repo:
//! - [`dirs`]: canonical filesystem layout (`~/.sovereign/…`).
//! - [`repo`]: git-repo path resolution + branch lookup.
//! - [`scip`]: merged SCIP graph loader for code-intelligence tools.
//!
//! CLI plumbing (was `sovereign-cli/src/util/`):
//! - [`cli_contract`]: loader for the CLI contract manifest (`docs/cli-contract.toml`).
//! - [`cli_contract_report`]: renders that manifest's quality surface for
//!   `svrn contract` and for the ratchet tests — one census, one renderer.
//! - [`help`]: shared `Help` struct + `print` formatter.
//! - [`deprecation`]: standard deprecation / retired announcements.
//! - [`prompts`]: interactive confirm / line-read helpers.
//! - [`tracing_init`]: one-line `init_tracing(default_filter)`.

pub mod args;
pub mod cli_contract;
pub mod cli_contract_report;
pub mod deprecation;
pub mod dirs;
pub mod help;
pub mod prompts;
pub mod repo;
#[cfg(feature = "scip")]
pub mod scip;
pub mod tracing_init;
pub mod urls;
