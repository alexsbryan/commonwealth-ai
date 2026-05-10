//! The `SetupConfig` schema and load/save helpers now live in
//! `sovereign-core` so the desktop app can depend on them without
//! pulling in this CLI-only crate. This file is a thin re-export so
//! existing `crate::setup_config::SetupConfig` call sites in the rest
//! of `sovereign-cli` keep compiling unchanged.

pub use sovereign_core::setup_config::*;
