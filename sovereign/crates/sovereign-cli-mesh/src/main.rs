// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cmnwlth`'s CLI sibling. The dispatcher execs this for the mesh verbs.

use sovereign_cli_mesh::*;

#[tokio::main]
async fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = raw
        .split_first()
        .map(|(c, r)| (c.as_str(), r))
        .unwrap_or(("", &[]));
    let code: i32 = match cmd {
        "meshapp" => meshapp_cmd::run(rest).await,
        "ring" => ring_cmd::run(rest).await,
        "job" => job_cmd::run(rest).await,
        "mesh" => mesh_cmd::run_mesh(rest).await,
        "publish" => publish_cmd::run(rest).await,
        "unpublish" => publish_cmd::run_unpublish(rest).await,
        "run" => run_cmd::run(rest).await,
        "" => {
            eprintln!("sovereign-cli-mesh: usage: sovereign-cli-mesh <subcommand> [args...]");
            2
        }
        other => {
            eprintln!("sovereign-cli-mesh: unknown subcommand '{other}'");
            2
        }
    };
    std::process::exit(code);
}
