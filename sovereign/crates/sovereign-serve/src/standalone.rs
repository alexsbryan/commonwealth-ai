// SPDX-License-Identifier: AGPL-3.0-or-later
//! serve's own process: the assembly, bound on serve's listener, until a
//! shutdown signal. A distribution hosting serve's assembly binds it itself
//! and never runs this.

use sovereign_contracts::setup_config::SetupConfig;
use tracing::{error, info};

use crate::{assemble, ServeArgs, ServeAssembly};

pub(crate) async fn serve(args: ServeArgs) -> i32 {
    info!(target: "serve", data_dir = %args.data_dir.display(), listen = %args.listen, "serve starting");
    let config_path = SetupConfig::path_in(&args.data_dir);
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
    let bound = listener
        .local_addr()
        .map_or_else(|_| args.listen.to_string(), |a| a.to_string());
    println!("sovereign-serve: listening on http://{bound}");

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
        routes,
        run_lock: _run_lock,
        ..
    } = assembly;
    match host_kit::shell::serve([listener], routes, shutdown).await {
        Ok(()) => 0,
        Err(e) => {
            error!(target: "serve", error = %e, "the listener stopped");
            1
        }
    }
}
