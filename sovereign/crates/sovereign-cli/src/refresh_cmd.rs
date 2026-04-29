//! `sovereign refresh` — nudge the daemon to rebuild SCIP.
//!
//! Renamed from `sovereign project refresh` per the CLI refactor plan.

pub async fn run(args: &[String]) -> i32 {
    crate::project_cmd::cmd_refresh(args).await
}
