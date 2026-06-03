//! Shim re-export of `sovereign-cli-shared::*` modules so files moved
//! from `sovereign-cli` keep their `crate::util::*` import paths.
//! Same shape as the sibling `sovereign-cli-atos::util` shim.

pub mod help {
    pub use sovereign_cli_shared::help::{print, wants_help, Help, HelpSection};
}
pub mod deprecation {
    
}
pub mod prompts {
    pub use sovereign_cli_shared::prompts::{confirm, stdin_is_tty};
}
pub mod dirs {
    pub use sovereign_cli_shared::dirs::{
        mesh_data_dir, sovereign_indexes, sovereign_root,
    };
}
pub mod tracing_init {
    pub use sovereign_cli_shared::tracing_init::init_tracing;
}
pub mod urls {
    pub use sovereign_cli_shared::urls::*;
}
