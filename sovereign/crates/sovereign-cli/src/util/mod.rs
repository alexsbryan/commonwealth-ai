// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crate-internal helpers shared across every subcommand.
//!
//! The `sovereign-cli` binary grew organically: each new subcommand
//! (setup, daemon, mesh, project, code, doctor, …) added a few lines
//! of argument parsing, a few lines of directory resolution, a few
//! hardcoded URLs, a few `eprintln!("    ✓ ...")` calls. Without a
//! shared home, the duplication drifted — three different
//! `default_data_dir()` bodies, five flavours of "confirm [y/N]", a
//! literal `:8080` lingering in one file after we flipped to `:9741`.
//!
//! Every helper here is a focused, testable primitive used by two or
//! more `*_cmd.rs` modules. Nothing in `util` depends on anything
//! outside the CLI crate.

pub mod deprecation;
pub mod dirs;
pub mod help;
pub mod prompts;
pub mod tracing_init;
pub mod urls;
