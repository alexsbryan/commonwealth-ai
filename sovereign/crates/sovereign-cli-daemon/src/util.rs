//! Util shim — re-exports from sovereign-cli-shared so files moved
//! from sovereign-cli-atos keep their `crate::util::*` imports.

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
pub mod urls {
    pub use sovereign_cli_shared::urls::*;
}
pub mod log_rotation {
    pub use crate::log_rotation::*;
}
