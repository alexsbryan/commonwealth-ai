// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Launch::AdminJoin`] — the setup wizard's mesh join as a process.
//!
//! `assemble(.., LaunchParts::Admin)`, then the join through cw-rails' join
//! door — the join and the node key are cw-rails' since pb-mesh-exit-transport
//! — then this daemon's `start`, so the wizard's poll of `/v1/mesh/venues`
//! finds it serving (five-programs-61, -62).

use std::io::{BufRead, Write};
use std::path::Path;

use mesh_join_vocab::deep_link::parse_join_argument;
use sovereign_contracts::launch::{Launch, JOINED_LINE_PREFIX};
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_turn_client::rails_kv::{resolve_rails_base, RAILS_BRING_UP_VERB};

use super::lifecycle::wait_for_shutdown;
use crate::daemon::EmbeddedDaemon;

/// cw-rails' join answer, the one field this child reports.
#[derive(serde::Deserialize)]
struct Joined {
    mesh_name: String,
}

/// Run the join; on success print the joined line and serve until SIGINT or
/// SIGTERM. Exit 2 for a malformed launch, 1 for a refused join (error on
/// stderr), 0 after a clean stop.
pub async fn run(
    launch: &Launch,
    config: Option<&Path>,
    node_name: Option<&str>,
    mesh: Option<crate::hosted_mesh::HostedMesh>,
) -> i32 {
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
    if parse_join_argument(&line).is_none() {
        eprintln!("error: stdin did not carry a join link");
        return 2;
    }

    // The wizard reads its holders from this child's venues, so the child
    // reads the roster the distribution composed for the cw-rails it joins
    // through; svrn alone reads none, named.
    let rails = resolve_rails_base(&setup_config.daemon);
    let mesh = match mesh {
        Some(m) => m.compose(&rails),
        None => crate::hosted_mesh::MeshAccess::absent(),
    };
    let services = match crate::assemble(launch, crate::LaunchParts::Admin { mesh }) {
        Ok(s) => s,
        Err(refusal) => {
            eprintln!("error: {refusal}");
            return 1;
        }
    };
    let data_dir = setup_config.data.dir.clone();
    tracing::info!(data_dir = %data_dir.display(), node_name, rails = %rails, "admin-join: joining through cw-rails");
    let daemon = EmbeddedDaemon::new(data_dir, setup_config, services);
    // The door parses all three invite forms itself; an absent cw-rails is
    // its error, naming the base and the verb that brings it up.
    let mesh_name = match sovereign_turn_client::TurnClient::new(&rails)
        .mesh_join::<Joined>(line.trim(), Some(node_name))
        .await
    {
        Ok(j) => j.mesh_name,
        Err(e) => {
            tracing::info!(error = %e, rails = %rails, "admin-join: join refused");
            eprintln!(
                "error: could not join the mesh through cw-rails at {rails}: {e} \
                 (`{RAILS_BRING_UP_VERB}` brings it up)"
            );
            return 1;
        }
    };
    if let Err(e) = daemon.start().await {
        eprintln!("error: joined \"{mesh_name}\", but this daemon did not start: {e}");
        return 1;
    }
    // The wizard polls `/v1/mesh/venues` the moment it reads the joined line,
    // so the line waits for the bind outcome, as `run`'s "running" does
    // (boot.rs).
    match daemon
        .client_listener(std::time::Duration::from_secs(60))
        .await
    {
        crate::ClientListener::Bound(addr) => {
            tracing::info!(%addr, "admin-join: client API listening");
        }
        crate::ClientListener::Failed(e) => {
            eprintln!("error: joined \"{mesh_name}\", but the client API is not listening — {e}");
            return 1;
        }
        crate::ClientListener::Pending => {
            eprintln!(
                "error: joined \"{mesh_name}\", but the client API bind did not settle within 60s"
            );
            return 1;
        }
    }
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
