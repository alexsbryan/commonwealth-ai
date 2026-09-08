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
//! - [`dirs`]: canonical filesystem layout (`~/.svrnmesh/…`).
//! - [`repo`]: git-repo path resolution + branch lookup.
//! - [`scip`]: merged SCIP graph loader for code-intelligence tools.
//!
//! CLI plumbing (was `sovereign-cli/src/util/`):
//! - [`cli_contract`]: loader for the CLI contract manifest (`docs/cli-contract.toml`).
//! - [`cli_contract_report`]: renders that manifest's quality surface for
//!   `svrn contract` and for the ratchet tests — one census, one renderer.
//! - [`flag_surface`]: `parse::<T: clap::Parser>` — the one rendering rule for
//!   every derived flag surface, so a caller's error prefix is never doubled.
//! - [`help`]: shared `Help` struct + `print` formatter.
//! - [`deprecation`]: standard deprecation / retired announcements.
//! - [`prompts`]: interactive confirm / line-read helpers.
//! - [`tracing_init`]: one-line `init_tracing(default_filter)`.
//! - [`code_index`] (`code-index`): the whole `svrn code index` verb. Both the
//!   dispatcher and the workbench serve it, so both used to carry a copy;
//!   gated because it is the one module here that is NOT cheap (corpus-engine
//!   + oicp-client), and only those two binaries enable it.
//! - [`code_index_incremental`]: the plan/stamp model behind `svrn code
//!   index`'s incremental refresh. Shared because BOTH the shipped dispatcher
//!   (`sovereign-cli`) and the workbench (`sovereign-cli-dev`) run that verb;
//!   until 2026-08-20 each carried a byte-identical 597-line copy of this
//!   module, which is two deciders for one plan (§10.6). std + serde only, so
//!   it costs every CLI binary nothing.
//! - [`observation`] / [`project_toml`] (`project-model`): what a repo IS
//!   (languages, dependencies, SCIP tooling) and the durable
//!   `.sovereign/project.toml` record derived from it. Shared because
//!   `project init` writes the file and `found` / `phase` / `audit` /
//!   `charter amend` read it back — across two binaries since 2026-08-07.

pub mod args;
pub mod cli_contract;
pub mod cli_contract_report;
#[cfg(feature = "code-index")]
pub mod code_index;
pub mod code_index_incremental;
pub mod deprecation;
pub mod dirs;
pub mod flag_surface;
pub mod help;
pub mod host_load;
pub mod lane_verdict;
#[cfg(feature = "mcp-client")]
pub mod mcp_client;
pub mod models;
#[cfg(feature = "project-model")]
pub mod observation;
#[cfg(feature = "project-model")]
pub mod project_toml;
pub mod prompts;
pub mod repo;
#[cfg(feature = "scip")]
pub mod scip;
pub mod tracing_init;
pub mod urls;
