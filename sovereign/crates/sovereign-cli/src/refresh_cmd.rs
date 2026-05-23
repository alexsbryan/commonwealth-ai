//! `sovereign refresh` — nudge the daemon to rebuild SCIP.
//!
//! Renamed from `sovereign project refresh` per the CLI refactor plan.

pub async fn run(args: &[String]) -> i32 {
    crate::dev_bin::exec("project-refresh", args)
}
