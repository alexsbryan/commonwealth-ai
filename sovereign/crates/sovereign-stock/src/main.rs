// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-stock` — the stock install as ONE process (FIVE_PROGRAMS §2c;
//! phase-b-29 Q1, Q2, Q4). What `svrn daemon run` execs.
//!
//! It composes two programs through their faces and owns nothing else:
//! serve's assembly is built in this process and its router bound on serve's
//! port, so cw-rails, code's FIM and cli-llm still dial serve there, and svrn
//! gets the SAME provider cell through the `InferenceProvider` port. svrn
//! decides whether serve is hosted here (`ServingPath::decide`); this binary
//! only hands it the composition. boundary-gate holds the face items: every
//! `sovereign_serve::` and `sovereign_daemon::` path below is on the
//! `[[distribution]] stock` row, spelled in full.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The compute child and the RPC worker re-exec this binary: routed first,
    // before any runtime, as serve's own binary routes them.
    if let Some(code) = sovereign_serve::child_launch(&args) {
        std::process::exit(code);
    }
    let hosted = sovereign_daemon::process::HostedServe::new(
        sovereign_serve::DEFAULT_FILTER,
        |data_dir, config_path| async move {
            let assembly = match sovereign_serve::assemble(&data_dir, &config_path).await {
                Ok(a) => a,
                Err(e) => return Err(e),
            };
            // A port another listener holds (a developer's own serve) refuses
            // boot by name: two serves on one port is never the stock install.
            let listener = match host_kit::shell::bind_with_retry(assembly.listen, "serve").await {
                Ok(l) => l,
                Err(e) => {
                    return Err(format!(
                        "serve's port {} is not this process's to bind: {e}",
                        assembly.listen
                    ))
                }
            };
            tracing::info!(target: "serve", listen = %assembly.listen, "hosted serve bound in the stock process");
            let (routes, run_lock) = (assembly.routes, assembly.run_lock);
            tokio::spawn(async move {
                // serve's hold on the data root lives as long as its listener.
                let _run_lock = run_lock;
                if let Err(e) =
                    host_kit::shell::serve([listener], routes, std::future::pending::<()>()).await
                {
                    tracing::error!(target: "serve", error = %e, "the hosted serve's listener stopped");
                }
            });
            Ok(assembly.cell)
        },
    );
    std::process::exit(sovereign_daemon::process::run(&args, Some(hosted)));
}
