// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn refresh` — re-export the SCIP call graph and rebuild the LanceDB
//! index when the on-disk embed model has drifted.
//!
//! Renamed from `svrn project refresh` per the CLI refactor plan.
//!
//! Under `code-intel` this runs in-process (`crate::code_refresh`); otherwise
//! it still execs the `sovereign-cli-dev` sibling, which is what a
//! workbench-only build wants. The SHIPPED binary is built with `code-intel`
//! (`scripts/release-cli-local.sh`), so users get the in-process path and no
//! longer need a sibling that was never packaged.

pub async fn run(args: &[String]) -> i32 {
    #[cfg(feature = "code-intel")]
    {
        crate::code_refresh::cmd_refresh(args).await
    }
    #[cfg(not(feature = "code-intel"))]
    {
        crate::dev_bin::exec("project-refresh", args)
    }
}
