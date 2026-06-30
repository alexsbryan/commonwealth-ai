// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn amend [architecture|charter]` — adversarial doc edit.
//!
//! Renamed from `svrn project amend` per the CLI refactor plan.
//! Phase 1 delegates to the existing handler.

pub async fn run(args: &[String]) -> i32 {
    crate::dev_bin::exec("project-amend", args)
}
