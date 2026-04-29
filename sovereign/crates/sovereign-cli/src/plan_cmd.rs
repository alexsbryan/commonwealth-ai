//! `sovereign plan [--allow-open]` — derive IMPLEMENTATION_PLAN.md.
//!
//! Renamed from `sovereign project plan` per the CLI refactor plan.

pub async fn run(args: &[String]) -> i32 {
    crate::project_cmd::cmd_plan(args).await
}
