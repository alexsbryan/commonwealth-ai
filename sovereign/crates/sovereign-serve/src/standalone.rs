// SPDX-License-Identifier: AGPL-3.0-or-later
//! serve's own process: the assembly, bound on serve's listener, until a
//! shutdown signal. A distribution hosting serve's assembly binds it itself
//! and never runs this.

use std::sync::Arc;

use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::InferenceProvider;
use tracing::{error, info};

use crate::{assemble, ServeArgs, ServeAssembly};

pub(crate) async fn serve(args: ServeArgs) -> i32 {
    info!(target: "serve", data_dir = %args.data_dir.display(), listen = %args.listen, "serve starting");
    let config_path = SetupConfig::path_in(&args.data_dir);
    // The loader's env contract, before the engine is built: the
    // `[shared_model]` role binds this node's rpc worker and arms discovery,
    // as svrn's boot applies it for a stock node (pb-serve-distributes-
    // standalone). A config that does not load refuses in `assemble` below.
    let config = SetupConfig::load_from(&config_path).ok();
    if let Some(config) = &config {
        crate::apply_shared_model_role_to_env(&config.shared_model);
    }
    let assembly = match assemble(&args.data_dir, &config_path).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sovereign-serve: {e}");
            return 1;
        }
    };
    // `assemble` loaded the same file, so a config that did not load has
    // already refused above.
    let Some(config) = config else {
        error!(target: "serve", config = %config_path.display(), "the config did not load before the assembly");
        return 1;
    };
    let rails_base = sovereign_turn_client::rails_kv::resolve_rails_base(&config.daemon);

    let listener = match host_kit::shell::bind_with_retry(args.listen, "serve").await {
        Ok(l) => l,
        Err(e) => {
            error!(target: "serve", listen = %args.listen, error = %e, "cannot bind");
            eprintln!("sovereign-serve: {e}");
            return 1;
        }
    };
    let bound_addr = listener.local_addr().unwrap_or(args.listen);
    println!("sovereign-serve: listening on http://{bound_addr}");

    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut term) => {
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = tokio::signal::ctrl_c() => {}
                    }
                }
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        info!(target: "serve", "shutdown signal received");
    };
    let ServeAssembly {
        cell,
        mut routes,
        run_lock: _run_lock,
        distribute,
        servable,
        ..
    } = assembly;
    // On a mesh, through cw-rails alone: discovery and the warm orchestrator
    // over its roster and reach, the worker's rpc-warm, and serve's origins
    // in its origin table (pb-serve-distributes-standalone). With no cw-rails
    // answering, every discovery tick scans nothing and the registrations
    // retry; the OpenAI wire serves as before.
    let local = cell as Arc<dyn InferenceProvider>;
    let roster = crate::rails_mesh::RailsRoster::new(rails_base.clone());
    info!(target: "serve", rails = %rails_base, "distribution over cw-rails' roster and reach");
    distribute(
        crate::rails_mesh::mesh_ports(roster.clone(), bound_addr),
        crate::rails_mesh::solo_router(Arc::clone(&local)),
    );
    routes.push(crate::rpc_warm::bundle(
        servable,
        roster,
        Arc::new(crate::MeshRpcShardWarmer::new()),
    ));
    // The member client on a loopback port of its own, registered whole on
    // cw-rails' `cwth/client/0`: a member's router reaches this node's
    // models there and nothing else of serve's. Without it serve still
    // serves this host; members see no models here.
    let member_addr = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(member) => match member.local_addr() {
            Ok(addr) => {
                info!(target: "serve", member = %addr, "member client listening for cw-rails' forwards");
                tokio::spawn(host_kit::shell::serve(
                    [member],
                    vec![crate::member_client_bundle(Arc::clone(&local))],
                    std::future::pending(),
                ));
                Some(addr)
            }
            Err(e) => {
                error!(target: "serve", error = %e, "the member client's port is unreadable; members reach no model here");
                None
            }
        },
        Err(e) => {
            error!(target: "serve", error = %e, "the member client could not bind; members reach no model here");
            None
        }
    };
    crate::rails_mesh::spawn_registrations(&rails_base, bound_addr, member_addr);
    match host_kit::shell::serve([listener], routes, shutdown).await {
        Ok(()) => 0,
        Err(e) => {
            error!(target: "serve", error = %e, "the listener stopped");
            1
        }
    }
}
