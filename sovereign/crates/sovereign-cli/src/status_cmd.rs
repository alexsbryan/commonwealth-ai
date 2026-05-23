//! `sovereign status` — unified health and project state report.
//!
//! Renamed from `sovereign project status` per the CLI refactor plan.
//!
//! Phase 1 (this file): delegates to [`crate::project_cmd::cmd_status`].
//! Future phases merge in the additional sections the spec calls for
//! (atos status, phase status, watch status — and in Phase 4+, daemon
//! slot inventory and mesh activity). The merge happens here so the
//! old `project status` alias keeps producing exactly today's output
//! without surprises.

pub async fn run(args: &[String]) -> i32 {
    crate::dev_bin::exec("project-status", args)
}
