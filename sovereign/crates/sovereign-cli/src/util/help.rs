// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared help formatter. Implementation moved to
//! `sovereign-cli-shared::help` so sibling binaries (atos, future
//! meta) can render the same `--help` blocks without depending on
//! `sovereign-cli`. This shim preserves the in-crate
//! `crate::util::help::*` import path used across every `*_cmd.rs`.

pub use sovereign_cli_shared::help::{print, wants_help, Help, HelpSection};
// Only the `--features dev-tools` help addendum (the "Developer toolchain"
// section in main.rs) uses this; gate the re-export to match so the default
// build doesn't flag it as an unused import.
#[cfg(feature = "dev-tools")]
pub use sovereign_cli_shared::help::print_subcommands_titled;
