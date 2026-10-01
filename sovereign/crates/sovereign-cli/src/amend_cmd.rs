// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn amend [architecture|charter]` — adversarial doc edit.
//!
//! Renamed from `svrn project amend` per the CLI refactor plan.
//! Phase 1 delegates to the existing handler.

pub async fn run(args: &[String]) -> i32 {
    // `amend design` is retired; the charter flow ignores its argument, so
    // forwarding it would run the charter amend in its place.
    if let Some(code) = sovereign_cli_shared::deprecation::refuse_retired(&["amend"], args) {
        return code;
    }
    crate::dev_bin::exec("project-amend", args)
}
