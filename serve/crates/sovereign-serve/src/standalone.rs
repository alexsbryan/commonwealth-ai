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
    if let Ok(config) = SetupConfig::load_from(&config_path) {
        crate::apply_shared_model_role_to_env(&config.shared_model);
    }
    let assembly = match assemble(&args.data_dir, &config_path).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sovereign-serve: {e}");
            return 1;
        }
    };

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
        rails_base,
        ..
    } = assembly;
    let local = cell as Arc<dyn InferenceProvider>;
    // serve ranks (pb-serve-ranks): the node's one router, over its cell and
    // cw-rails' roster's peers, and serve's OpenAI face answers through it.
    // A peer's turn arrives on the member client (`rails_mesh::join`), over
    // the cell, and is never re-ranked here.
    let venues = Arc::new(crate::rails_mesh::RailsVenues::new(
        crate::rails_mesh::RailsRoster::new(rails_base.clone()),
    ));
    let ranking = crate::rank(Arc::clone(&local), venues.clone(), venues).await;
    let before = routes.len();
    routes.retain(|b| b.name() != crate::OPENAI_BUNDLE);
    let replaced = before - routes.len();
    routes.push(crate::openai_face(Arc::clone(&ranking.provider)));
    info!(target: "serve", replaced, "serve's OpenAI face ranks over cw-rails' roster");
    crate::rails_mesh::join(
        &rails_base,
        bound_addr,
        local,
        ranking.router,
        distribute,
        servable,
        &mut routes,
    )
    .await;
    match host_kit::shell::serve([listener], routes, shutdown).await {
        Ok(()) => 0,
        Err(e) => {
            error!(target: "serve", error = %e, "the listener stopped");
            1
        }
    }
}
