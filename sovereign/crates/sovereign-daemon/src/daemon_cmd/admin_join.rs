// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Launch::AdminJoin`] — the setup wizard's mesh join as a process.
//!
//! The same assembly and the same two calls the wizard made in-process
//! (`svrn setup --terminal`): `assemble(.., LaunchParts::Admin)`, then
//! `expose_client_api` and `join_mesh`. The join has to happen here, not over
//! HTTP afterwards: an `EmbeddedDaemon` binds its client API only inside
//! `start_daemon`, which a join reaches and an idle admin assembly does not
//! (five-programs-61).

use std::io::{BufRead, Write};
use std::path::Path;

use mesh_join_vocab::deep_link::parse_join_argument;
use sovereign_contracts::launch::{Launch, JOINED_LINE_PREFIX};
use sovereign_contracts::setup_config::SetupConfig;

use super::lifecycle::wait_for_shutdown;
use crate::daemon::EmbeddedDaemon;

/// Run the join; on success print the joined line and serve until SIGINT or
/// SIGTERM. Exit 2 for a malformed launch, 1 for a refused join (error on
/// stderr), 0 after a clean stop.
pub async fn run(launch: &Launch, config: Option<&Path>, node_name: Option<&str>) -> i32 {
    let (Some(config_path), Some(node_name)) = (config, node_name) else {
        eprintln!("error: `sovereign-daemon join` needs --config <path> and --node-name <name>");
        return 2;
    };
    // Parsed, not `SetupConfig::load_from`: that refuses a config declaring
    // neither `[models]` nor an entry binding, and a node about to join has
    // neither yet — the wizard binds the entry only after the join. The
    // in-process wizard handed its daemon the same class-less config.
    let setup_config = match std::fs::read_to_string(config_path)
        .map_err(|e| format!("read {}: {e}", config_path.display()))
        .and_then(|s| {
            toml::from_str::<SetupConfig>(&s)
                .map_err(|e| format!("parse {}: {e}", config_path.display()))
        }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    // The invite is ONE stdin line, never argv: it carries the join key.
    let mut line = String::new();
    if let Err(e) = std::io::stdin().lock().read_line(&mut line) {
        eprintln!("error: could not read the join link from stdin: {e}");
        return 2;
    }
    let Some(link) = parse_join_argument(&line) else {
        eprintln!("error: stdin did not carry a join link");
        return 2;
    };

    let services = match crate::assemble(launch, crate::LaunchParts::Admin) {
        Ok(s) => s,
        Err(refusal) => {
            eprintln!("error: {refusal}");
            return 1;
        }
    };
    let data_dir = setup_config.data.dir.clone();
    tracing::info!(data_dir = %data_dir.display(), node_name, "admin-join: joining");
    let daemon = EmbeddedDaemon::new(data_dir, setup_config, services);
    daemon.expose_client_api();
    let mesh_name = match daemon.join_mesh(&link, node_name).await {
        Ok(result) => result.mesh_name,
        Err(e) => {
            tracing::info!(error = %e, "admin-join: join refused");
            eprintln!("error: could not join the mesh: {e}");
            return 1;
        }
    };
    println!("{JOINED_LINE_PREFIX}\"{mesh_name}\"");
    if let Err(e) = std::io::stdout().flush() {
        eprintln!("error: joined, but could not report it on stdout: {e}");
    }
    tracing::info!(mesh_name, "admin-join: joined; serving until stopped");

    wait_for_shutdown().await;
    if let Err(e) = daemon.shutdown().await {
        eprintln!("error: shutdown after join: {e}");
        return 1;
    }
    0
}
