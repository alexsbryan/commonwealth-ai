//! `sovereign design` — agent-collaborative DESIGN.md session.
//!
//! Renamed from `sovereign project design` per the CLI refactor plan.

pub async fn run(args: &[String]) -> i32 {
    crate::dev_bin::exec("project-design", args)
}
