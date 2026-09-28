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
//! - [`code_index`]: `tempfile_dir` only. The `svrn code index` verb and its
//!   incremental plan/stamp model moved to the code program,
//!   `sovereign-cli-dev` (pb-code-index); `project init` still takes its
//!   scratch directory from here.
//!
//! `dirs`, `repo`, `help`, `deprecation`, `prompts`, `tracing_init`,
//! `models`, `mcp_client` and `code_index` live in the leaf
//! `sovereign-cli-base` and are re-exported here at their historical paths
//! (pb-code-cli-base). The project model (`observation`, `project_toml`)
//! moved to the code program, `sovereign-cli-dev`, whose `project-observe`
//! arm `project init` execs; the `scip` re-export went with no reader left.

pub mod args;
pub mod cli_contract;
pub mod cli_contract_report;
pub use sovereign_cli_base::code_index;
pub use sovereign_cli_base::deprecation;
pub use sovereign_cli_base::dirs;
pub use sovereign_cli_base::dispatcher;
pub mod flag_surface;
pub use sovereign_cli_base::guest_link;
pub use sovereign_cli_base::help;
pub mod host_load;
pub mod lane_verdict;
#[cfg(feature = "mcp-client")]
pub use sovereign_cli_base::mcp_client;
pub use sovereign_cli_base::models;
pub use sovereign_cli_base::prompts;
#[cfg(feature = "rail-client")]
pub use sovereign_cli_base::rail;
pub use sovereign_cli_base::repo;
pub use sovereign_cli_base::tracing_init;
pub use sovereign_cli_base::urls;
