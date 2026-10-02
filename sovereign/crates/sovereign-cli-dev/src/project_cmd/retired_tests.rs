// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-dev project <retired>` refuses by name, the same as the
//! dispatcher does, so invoking this program directly never falls to
//! "Unknown project subcommand" or runs the charter amend for `amend design`.

use super::*;

#[tokio::test]
async fn retired_project_subcommands_refuse_with_exit_2() {
    for argv in [&["design"][..], &["plan"], &["amend", "design"]] {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        assert_eq!(run_project(&args).await, 2, "project {argv:?}");
    }
}
