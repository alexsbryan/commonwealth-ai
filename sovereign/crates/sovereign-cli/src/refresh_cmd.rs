// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn refresh` — re-export the SCIP call graph and rebuild the LanceDB
//! index when the on-disk embed model has drifted.
//!
//! Renamed from `svrn project refresh` per the CLI refactor plan.
//!
//! Both builds exec the `sovereign-cli-dev` sibling, the code program. Under
//! `code-intel` it runs `refresh` (`code_refresh`, which ran in-process here
//! until pb-code-index moved it); otherwise `project-refresh`, which is what a
//! workbench-only build wants.

pub async fn run(args: &[String]) -> i32 {
    #[cfg(feature = "code-intel")]
    {
        crate::dev_bin::exec("refresh", args)
    }
    #[cfg(not(feature = "code-intel"))]
    {
        crate::dev_bin::exec("project-refresh", args)
    }
}
