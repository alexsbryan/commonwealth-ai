// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-stock` — the stock install as ONE process (FIVE_PROGRAMS §2c;
//! phase-b-29 Q1, Q2, Q4). What `svrn daemon run` execs.
//!
//! It composes three programs through their faces and owns nothing else:
//! serve's assembly is built in this process and its router bound on serve's
//! port, so cw-rails, code's FIM and cli-llm still dial serve there, and svrn
//! gets the SAME provider cell through the `InferenceProvider` port. svrn
//! decides whether serve is hosted here (`ServingPath::decide`); this binary
//! only hands it the composition. Code is composed over svrn's data root and
//! mounted on svrn's one `:9741/mcp` and client surface (pb-code-daemon-exit;
//! F2 (a), phase-b-30; phase-b-33). Ingest is composed here too: its
//! enrichment-config port from ingest's catalog (pb-ingest-dial-tools-close),
//! and the engine svrn's daemon holds, built by ingest's face for what svrn
//! hands it (pb-ingest-dial-daemon); svrn links neither. boundary-gate holds
//! the face items: every `sovereign_serve::`, `sovereign_daemon::`,
//! `sovereign_code::`, `sovereign_enrichment_catalog::`, `corpus_engine::`
//! and `sovereign_authoring_harness::` path below is on the
//! `[[distribution]] stock` row, spelled in full.

mod ingest;

/// Code's editor door takes its grammar lookup from the host (pb-meshapp-rest):
/// corpus-engine's registry, the one that routes `.tsx` apart from `.ts`,
/// which code may not name. One supplier, one registry.
fn grammar_for(ext: &str) -> Option<sovereign_code::face::Grammar> {
    let cfg = corpus_engine::extractors::code::language_for_extension(ext)?;
    Some(sovereign_code::face::Grammar {
        id: cfg.id,
        language: cfg.lang.into(),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The compute child and the RPC worker re-exec this binary: routed first,
    // before any runtime, as serve's own binary routes them.
    if let Some(code) = sovereign_serve::child_launch(&args) {
        std::process::exit(code);
    }
    // serve's placement measurements stay on cw-rails' journal on every stock
    // node, hosted serve or not: `assemble` below runs only when it is hosted.
    sovereign_serve::measurements_rail::spawn_reconcile(None);
    let hosted = sovereign_daemon::process::HostedServe::new(
        sovereign_serve::tracing_filter(),
        |data_dir, config_path, ports| async move {
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
            // serve ranks over its own cell, once: a reload swaps the cell
            // under the router (pb-serve-ranks).
            let ranking = sovereign_serve::rank(
                std::sync::Arc::clone(&assembly.cell) as _,
                ports.venues,
                ports.host,
            )
            .await;
            // serve's distribution starts over svrn's mesh, whose ports are
            // composed here (pb-serve-ranks-discovery), with serve's router.
            let distribute = assembly.distribute;
            let router = std::sync::Arc::clone(&ranking.router);
            let parts = sovereign_daemon::process::HostedParts {
                cell: assembly.cell,
                ranked: ranked(ranking),
                distribute: Box::new(move |daemon| distribute(mesh_ports(&daemon), router)),
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
    // The loader's env contract, applied by svrn's boot on every path.
    .env_contract(|args, shared_model| {
        sovereign_serve::apply_rpc_worker_flag(args);
        sovereign_serve::apply_shared_model_role_to_env(shared_model);
    })
    // The NER kind svrn's ingest and retrieval take their handle from, where
    // this process loads for itself (hosted, or a terminal).
    .ner(sovereign_serve::served_ner)
    // The worker-side warmer svrn's `/internal/rpc-warm` hands each request.
    .rpc_warmer(std::sync::Arc::new(
        sovereign_serve::MeshRpcShardWarmer::new(),
    ))
    // The RPC-worker rows svrn's `/v1/mesh/status` reports: serve's view in
    // this process (pb-serve-ranks-discovery).
    .rpc_workers(sovereign_serve::rpc_worker_views)
    // Where serve is not hosted here (the dialing path, a terminal), its
    // router still ranks svrn's turns, over the provider svrn holds.
    .rank(|provider, ports| async move {
        ranked(sovereign_serve::rank(provider, ports.venues, ports.host).await)
    });
    // Placement is this binary's (FIVE_PROGRAMS §2c): code's indexes and
    // result stores are svrn's root's, as they were when svrn hosted them.
    let code = sovereign_daemon::process::HostedCode::new(|host| async move {
        let face = sovereign_code::face::compose(sovereign_code::face::CodeParts {
            indexes_dir: host.data_dir.join("indexes"),
            stores_dir: host.data_dir.clone(),
            // Code opens its own notes.db under `stores_dir` (pb-notes-memory).
            notes: None,
            index: host.index,
            workspace: host.workspace,
            sovereign_dir: None,
            session_prefix: "daemon",
            extra_watchers: Vec::new(),
            notes_rail: sovereign_code::face::NotesRail {
                embed: Some(host.notes_embed),
                gliner: host.notes_gliner,
                node_id: Some(host.node_id),
                roster: host.roster,
                convergence: Some(host.convergence),
            },
            grammar: Some(grammar_for),
        })
        .await?;
        for line in &face.banner {
            tracing::info!(target: "code", "{line}");
        }
        Ok(sovereign_daemon::process::CodeMount {
            tools: std::sync::Arc::new(face.mcp),
            routes: face.routes,
            edit_routes: face.edit_routes,
            yield_to: face.runtime.yield_setter(),
            hold: Box::new(face.runtime),
        })
    });
    let ingest = ingest::hosted();
    let exit_code = sovereign_daemon::process::run(&args, Some(hosted), Some(code), Some(ingest));
    // macOS: past `__cxa_finalize_ranges`, so the ggml-metal device sweeper
    // never asserts on still-resident resources; the loader's fast-exit,
    // through serve's face, where the daemon's `run` used to call it.
    #[cfg(target_os = "macos")]
    {
        sovereign_serve::fast_exit_skip_destructors(exit_code)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::process::exit(exit_code)
    }
}

/// serve's ranking as svrn is handed it: the router as svrn's provider, its
/// OpenAI face, gauge and alias sink (pb-serve-ranks).
fn ranked(ranking: sovereign_serve::Ranking) -> sovereign_daemon::process::Ranked {
    sovereign_daemon::process::Ranked {
        provider: ranking.provider,
        service: ranking.service,
        in_flight: Some(ranking.in_flight),
        slot_aliases: Some(ranking.slot_aliases),
    }
}

/// svrn's mesh, as serve's discovery loop and warm orchestrator read it
/// (compute's `distributed_discovery` and `distributed_warm`,
/// pb-serve-distributes), composed here where both programs meet
/// (pb-serve-ranks-discovery): the roster, transport and identity of a Running
/// daemon, `None` otherwise; the host role published for `/v1/mesh/status`;
/// the discovery memory the warm orchestrator resolves endpoints through, one
/// per process; where this daemon serves model files (its internal port, and
/// the reachable bases on it); and its mesh proof.
fn mesh_ports(
    daemon: &std::sync::Arc<sovereign_daemon::EmbeddedDaemon>,
) -> sovereign_serve::MeshPorts {
    use std::sync::Arc;
    let reader = Arc::clone(daemon);
    let origin = Arc::clone(daemon);
    let prover = Arc::clone(daemon);
    sovereign_serve::MeshPorts {
        model_origin: Arc::new(move || {
            let daemon = Arc::clone(&origin);
            Box::pin(async move {
                let (_client_port, internal_port) = daemon.resolved_ports().await;
                sovereign_serve::ModelOrigin {
                    internal_port,
                    bases: sovereign_mesh::mesh_discovery::reachable_addresses(internal_port)
                        .into_iter()
                        .map(|a| format!("http://{a}"))
                        .collect(),
                }
            })
        }),
        proof: Arc::new(move || {
            let daemon = Arc::clone(&prover);
            Box::pin(async move {
                let stamp = daemon.app_state().await?.mesh_proof_stamp().await?;
                let (name, value) = stamp.pair();
                Some((name, value.to_string()))
            })
        }),
        mesh: Arc::new(move || {
            let daemon = Arc::clone(&reader);
            Box::pin(async move {
                let app = daemon.app_state().await?;
                Some(sovereign_serve::MeshNow {
                    roster: Arc::clone(&app.inner.fabric.membership),
                    transport: app.peer_transport(),
                    self_id: app.inner.fabric.identity.current(),
                })
            })
        }),
        on_host_role: Arc::new(sovereign_daemon::mesh_http::set_shared_model_host),
        discovery: Arc::default(),
    }
}
