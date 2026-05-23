//! Interactive prompt helpers. Implementation moved to
//! `sovereign-cli-shared::prompts`. This shim preserves the in-crate
//! `crate::util::prompts::*` import path.

pub use sovereign_cli_shared::prompts::{confirm, prompt_path, prompt_string, stdin_is_tty};
