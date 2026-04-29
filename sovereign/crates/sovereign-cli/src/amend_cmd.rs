//! `sovereign amend [architecture|charter]` — adversarial doc edit.
//!
//! Renamed from `sovereign project amend` per the CLI refactor plan.
//! Phase 1 delegates to the existing handler.

pub async fn run(args: &[String]) -> i32 {
    crate::project_cmd::cmd_amend(args).await
}
