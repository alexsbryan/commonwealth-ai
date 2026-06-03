//! Shim module so files moved out of `sovereign-cli` keep their
//! `crate::util::help::*` / `crate::util::deprecation::*` import paths
//! without a global find-replace. Each submodule re-exports the
//! corresponding `sovereign_cli_shared::*` module.
//!
//! When more than two CLI binaries exist and the duplication is no
//! longer worth the convenience, switch the moved files to import
//! `sovereign_cli_shared::help` directly and delete this shim.

pub mod help {
    pub use sovereign_cli_shared::help::{print, wants_help, Help, HelpSection};
}

pub mod deprecation {
    pub use sovereign_cli_shared::deprecation::{announce, announce_retired};
}

pub mod prompts {
    pub use sovereign_cli_shared::prompts::{confirm, prompt_path, prompt_string, stdin_is_tty};
}

pub mod dirs {
    pub use sovereign_cli_shared::dirs::{
        default_data_dir, mesh_data_dir, sovereign_indexes, sovereign_root,
    };
}

pub mod tracing_init {
    pub use sovereign_cli_shared::tracing_init::init_tracing;
}

// `log_rotation` lives in sovereign-cli-daemon now (alongside the
// daemon that uses it). Dev-bin doesn't need it.
