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
//! F2 (a), phase-b-30; phase-b-33). Ingest's enrichment-config port is built
//! from ingest's catalog and handed to svrn, which links no catalog
//! (pb-ingest-dial-tools-close). boundary-gate holds the face items: every
//! `sovereign_serve::`, `sovereign_daemon::`, `sovereign_code::` and
//! `sovereign_enrichment_catalog::` path below is on the
//! `[[distribution]] stock` row, spelled in full.

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
        })
        .await?;
        for line in &face.banner {
            tracing::info!(target: "code", "{line}");
        }
        Ok(sovereign_daemon::process::CodeMount {
            tools: std::sync::Arc::new(face.mcp),
            routes: face.routes,
            yield_to: face.runtime.yield_setter(),
            hold: Box::new(face.runtime),
        })
    });
    let ingest = sovereign_daemon::process::HostedIngest::new(std::sync::Arc::new(
        sovereign_enrichment_catalog::port::CatalogEnrichConfig,
    ));
    std::process::exit(sovereign_daemon::process::run(
        &args,
        Some(hosted),
        Some(code),
        Some(ingest),
    ));
}
