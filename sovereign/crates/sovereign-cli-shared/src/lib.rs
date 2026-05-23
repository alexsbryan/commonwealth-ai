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
//! - [`help`]: shared `Help` struct + `print` formatter.
//! - [`deprecation`]: standard deprecation / retired announcements.
//! - [`prompts`]: interactive confirm / line-read helpers.
//! - [`tracing_init`]: one-line `init_tracing(default_filter)`.

pub mod deprecation;
pub mod dirs;
pub mod help;
pub mod prompts;
pub mod repo;
#[cfg(feature = "scip")]
pub mod scip;
pub mod tracing_init;
pub mod urls;
