//! `sovereign design` — agent-collaborative DESIGN.md session.
//!
//! Renamed from `sovereign project design` per the CLI refactor plan.

pub async fn run(args: &[String]) -> i32 {
    crate::project_cmd::cmd_design(args).await
}
