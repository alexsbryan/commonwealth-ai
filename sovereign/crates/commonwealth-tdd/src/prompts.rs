//! Prompt assets. Per ARCH §6 ("data vs program"), prompt text
//! lives as `assets/*.md` loaded via `include_str!`. Edit the
//! asset, not Rust string literals.
//!
//! Collapsed surface (2026-05-24): one core asset. Per-task
//! prefixes live in [`crate::tasks`].

pub const TRIAL_SYSTEM_PROMPT: &str = include_str!("../assets/trial_prompt.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_prompt_is_non_empty() {
        assert!(!TRIAL_SYSTEM_PROMPT.trim().is_empty());
    }
}
