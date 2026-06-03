//! Util shim — re-exports from sovereign-cli-shared so files moved
//! from sovereign-cli-atos keep their `crate::util::*` imports.

pub mod help {
    pub use sovereign_cli_shared::help::{print, wants_help, Help, HelpSection};
}
pub mod deprecation {
    pub use sovereign_cli_shared::deprecation::announce;
}
pub mod prompts {
    pub use sovereign_cli_shared::prompts::prompt_path;
}
pub mod dirs {}
pub mod tracing_init {}
pub mod urls {}
pub mod log_rotation {
    pub use crate::log_rotation::*;
}
