//! Shared help formatter. Implementation moved to
//! `sovereign-cli-shared::help` so sibling binaries (atos, future
//! meta) can render the same `--help` blocks without depending on
//! `sovereign-cli`. This shim preserves the in-crate
//! `crate::util::help::*` import path used across every `*_cmd.rs`.

pub use sovereign_cli_shared::help::{print, wants_help, Help, HelpSection};
