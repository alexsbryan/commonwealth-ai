// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-onprem` — the on-prem distribution as ONE process
//! (FIVE_PROGRAMS §2c; phase-b-86, -87).
//!
//! It composes svrn, serve and ingest, and nothing that could reach a shell,
//! the web or an arbitrary server-side path: no code program (this crate has
//! no sovereign-code edge, so no code tool or solve job exists here), no mesh
//! (serve joins no cw-rails roster and opens no member port), no recipe
//! authoring (no sovereign-recipe-author edge, so no `probe_url`), and svrn's
//! posture is `Sealed`: web reach, the wikipedia bundle and the `/mcp` route
//! are withheld, each by name. The withholding is the composition, not a
//! config switch (principle 10). boundary-gate holds the face items on the
//! `[[distribution]] onprem` row.

/// Ingest, composed by the stock distribution's one file, so the two
/// distributions build ingest one way (principle 8).
#[path = "../../sovereign-stock/src/ingest.rs"]
mod ingest;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The compute child and the RPC worker re-exec this binary: routed first,
    // before any runtime, as serve's own binary routes them.
    if let Some(code) = sovereign_serve::child_launch(&args) {
        std::process::exit(code);
    }
    let hosted = sovereign_daemon::process::HostedServe::new(
        sovereign_serve::tracing_filter(),
        |data_dir, config_path, ports| async move {
            let assembly = sovereign_serve::assemble(&data_dir, &config_path).await?;
            // A port another listener holds refuses boot by name.
            let listener = host_kit::shell::bind_with_retry(assembly.listen, "serve")
                .await
                .map_err(|e| {
                    format!(
                        "serve's port {} is not this process's to bind: {e}",
                        assembly.listen
                    )
                })?;
            let bound = listener.local_addr().unwrap_or(assembly.listen);
            tracing::info!(target: "serve", listen = %bound, "hosted serve bound in the on-prem process, on no mesh");
            let ranking = sovereign_serve::rank(
                std::sync::Arc::clone(&assembly.cell) as _,
                ports.venues,
                ports.host,
            )
            .await;
            let parts = sovereign_daemon::process::HostedParts {
                cell: assembly.cell,
                ranked: ranked(ranking),
            };
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
            Ok(parts)
        },
    )
    .env_contract(|args, shared_model| {
        sovereign_serve::apply_rpc_worker_flag(args);
        sovereign_serve::apply_shared_model_role_to_env(shared_model);
    })
    .ner(sovereign_serve::served_ner)
    .rank(|provider, ports| async move {
        ranked(sovereign_serve::rank(provider, ports.venues, ports.host).await)
    });
    let exit_code = sovereign_daemon::process::run(
        &args,
        Some(hosted),
        None,
        Some(ingest::hosted(None)),
        None,
        sovereign_daemon::process::Posture::Sealed,
    );
    // macOS: the loader's fast-exit past `__cxa_finalize_ranges`, as stock's.
    #[cfg(target_os = "macos")]
    {
        sovereign_serve::fast_exit_skip_destructors(exit_code)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::process::exit(exit_code)
    }
}

/// serve's ranking as svrn is handed it.
fn ranked(ranking: sovereign_serve::Ranking) -> sovereign_daemon::process::Ranked {
    sovereign_daemon::process::Ranked {
        provider: ranking.provider,
        service: ranking.service,
        in_flight: Some(ranking.in_flight),
        slot_aliases: Some(ranking.slot_aliases),
    }
}
