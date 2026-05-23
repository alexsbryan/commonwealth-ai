//! Canonical filesystem paths used by every sovereign-cli subcommand.
//!
//! The implementation moved to `sovereign-cli-shared` so the
//! `sovereign-cli-atos` sibling binary can call the same helpers
//! without depending on `sovereign-cli`. This file is a thin
//! re-export shim so existing `crate::util::dirs::*` call sites
//! keep compiling.

pub use sovereign_cli_shared::dirs::{mesh_data_dir, sovereign_indexes, sovereign_root};
