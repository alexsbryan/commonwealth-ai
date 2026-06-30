// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn refresh` — nudge the daemon to rebuild SCIP.
//!
//! Renamed from `svrn project refresh` per the CLI refactor plan.

pub async fn run(args: &[String]) -> i32 {
    crate::dev_bin::exec("project-refresh", args)
}
