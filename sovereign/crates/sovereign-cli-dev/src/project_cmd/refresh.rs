// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project refresh` — re-export the SCIP call graph and rebuild the
//! LanceDB corpus index when the on-disk embed model has drifted. Bundles
//! the LanceDB rebuild decision (`maybe_rebuild_lancedb_corpus` /
//! `lancedb_rebuild_reason`) and the SCIP DB reset. Split out of
//! `project_cmd` (2026-07-13); pure move. Shared plumbing via `use super::*`.

use super::*;

const HELP_REFRESH: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project refresh",
    summary: "Re-export the SCIP call graph + rebuild the LanceDB index when embeddings are stale.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn project refresh [--quiet] [--rebuild-index]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            (
                "--quiet",
                "Suppress progress output (use from hook scripts)",
            ),
            (
                "--rebuild-index",
                "Force-rebuild the LanceDB corpus index (chunks + embeddings) \
                 even when the on-disk embed model matches the current daemon.",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Runs automatically on commit via the installed hook (SCIP only; the hook does \
             NOT force a LanceDB rebuild). The nudge path VERIFIES the rebuild completes: \
             it reports a named failure when the export cannot finish (never a silent no-op) \
             and falls back to an in-process rebuild when the daemon path cannot deliver. \
             The LanceDB index auto-rebuilds on an embed-model mismatch or when its content \
             is older than 7 days — once the indexes are on the current model and recent, \
             subsequent refreshes stay SCIP-only and fast.",
        ),
    ],
};

// ─── Refresh ─────────────────────────────────────────────────

pub(crate) async fn cmd_refresh(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_REFRESH);
        return 0;
    }
    let mut quiet = false;
    let mut local = false;
    let mut data_dir: Option<PathBuf> = None;
    let mut explicit_name: Option<String> = None;
    // `--rebuild-index` forces a LanceDB rebuild unconditionally.
    // Without it, the LanceDB rebuild only runs when an embed-
    // model mismatch is detected (see `needs_lancedb_rebuild`).
    let mut force_rebuild_index = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--quiet" | "-q" => quiet = true,
            // Escape hatch: run the full in-process export instead
            // of nudging the daemon. Useful when the daemon is
            // down or the user is debugging the exporter itself.
            "--local" => local = true,
            "--rebuild-index" => force_rebuild_index = true,
            "--name" => {
                i += 1;
                explicit_name = args.get(i).cloned();
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    // Default path: nudge the running daemon so its Reindexer
    // handles the rebuild in-process. Coalesces with any FS /
    // git-poll signals that might have fired concurrently; keeps
    // the CLI decoupled from the exporter plumbing. Falls back to
    // the legacy in-process path when --local is set or the
    // daemon is unreachable.
    //
    // The nudge is now VERIFIED, not fire-and-forget: the pre-fix
    // CLI printed "✓ Rebuild nudged" on any 2xx while the daemon
    // could sit in a silent active-no-op wedge for hours (live
    // 2026-08-14). We poll /v1/projects until the graph reaches git
    // HEAD, the rebuild fails loudly, or the budget expires — and
    // only a Completed verdict reports success.
    if !local {
        let corpus_id = explicit_name
            .clone()
            .unwrap_or_else(|| derive_corpus_id(&repo_root));
        // Capture the pre-nudge failure baseline so a stale
        // last_rebuild_error is not misattributed to this nudge.
        let baseline_error_ts = daemon_get("/v1/projects")
            .await
            .ok()
            .and_then(|s| {
                s["projects"]
                    .as_array()
                    .and_then(|ps| {
                        ps.iter()
                            .find(|p| p["corpus_id"].as_str() == Some(corpus_id.as_str()))
                    })
                    .cloned()
            })
            .and_then(|p| p["last_rebuild_error"][1].as_u64())
            .unwrap_or(0);
        match daemon_post(
            &format!("/v1/projects/{corpus_id}/rebuild"),
            serde_json::json!({ "reason": "cli refresh" }),
        )
        .await
        {
            Ok(_) => {
                let mut verdict =
                    await_rebuild_completion(&corpus_id, &repo_root, baseline_error_ts, quiet)
                        .await;
                // Recovery: a crashed rebuild leaves the slots
                // self-cleared (the RAII guard + watchdog), so one
                // re-nudge is safe and cheap.
                if matches!(verdict, NudgeVerdict::Crashed { .. }) {
                    if !quiet {
                        eprintln!("    Re-nudging once to recover...");
                    }
                    if daemon_post(
                        &format!("/v1/projects/{corpus_id}/rebuild"),
                        serde_json::json!({ "reason": "cli refresh (recovery)" }),
                    )
                    .await
                    .is_ok()
                    {
                        verdict = await_rebuild_completion(&corpus_id, &repo_root, 0, quiet).await;
                    }
                }
                match verdict {
                    NudgeVerdict::Completed => {
                        if !quiet {
                            println!("  \u{2713} SCIP graph at HEAD for \"{corpus_id}\".");
                        }
                        // SCIP is fresh; now gate the LanceDB corpus
                        // rebuild on the explicit `--rebuild-index`
                        // flag, an embed-model mismatch, or content
                        // age. Common case: none → no-op → `refresh`
                        // stays fast.
                        let data_dir_for_rebuild = data_dir
                            .clone()
                            .or_else(default_data_dir)
                            .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));
                        let abs_repo = repo_root
                            .canonicalize()
                            .unwrap_or_else(|_| repo_root.clone());
                        return maybe_rebuild_lancedb_corpus(
                            &abs_repo,
                            &corpus_id,
                            &data_dir_for_rebuild,
                            force_rebuild_index,
                            quiet,
                        )
                        .await;
                    }
                    NudgeVerdict::Failed { error } => {
                        eprintln!("  \u{2717} Rebuild FAILED: {error}");
                        eprintln!("    The graph is frozen at its last indexed commit.");
                        eprintln!("    Falling back to local in-process rebuild.");
                    }
                    NudgeVerdict::Crashed { reason } => {
                        eprintln!("  \u{2717} Rebuild CRASHED: {reason}");
                        eprintln!("    Falling back to local in-process rebuild.");
                    }
                    NudgeVerdict::Wedged { detail } => {
                        eprintln!("  \u{2717} Rebuild WEDGED: {detail}");
                        eprintln!("    Falling back to local in-process rebuild.");
                    }
                    NudgeVerdict::DaemonGone { detail } => {
                        eprintln!("  \u{26a0} Daemon lost mid-verification: {detail}");
                        eprintln!("    Falling back to local in-process rebuild.");
                    }
                    NudgeVerdict::Pending => {
                        unreachable!("await_rebuild_completion never returns Pending")
                    }
                }
                // Fall through to the local in-process path below —
                // the escape hatch the pre-fix nudge path never
                // reached (order defect 5).
            }
            Err(e) => {
                if !quiet {
                    eprintln!("  \u{26a0} Daemon nudge failed: {e}");
                    eprintln!("    Falling back to local in-process rebuild.");
                }
                // Fall through to legacy path below.
            }
        }
    }
    // `--name` on the LOCAL path: resolve the project's ROOT from the
    // registry, do not fall through to the cwd.
    //
    // This used to be `let _ = explicit_name;` — the flag was parsed,
    // then dropped, and the rebuild ran against whatever repo the caller
    // happened to be standing in, reporting ✓ with that repo's symbol
    // counts under no name at all. `svrn project refresh --name go-demo
    // --local` from this workspace re-exported commonwealth-ai and said
    // "250484 symbols (+0)" (observed 2026-08-07). Doctor's own repair
    // hint for an empty graph is exactly this form, so following the
    // advice refreshed the wrong project and cleared nothing.
    //
    // Absence is REFUSED rather than defaulted (ARCH §18.3): an
    // unregistered name is a typo or a stale hint, and silently
    // rebuilding a different project is the failure this whole arm
    // exists to stop.
    let repo_root = match &explicit_name {
        None => repo_root,
        Some(name) => {
            let registry = match sovereign_mesh::projects::Registry::load() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("  error: could not read the project registry: {e}");
                    return 1;
                }
            };
            match registry.find(name) {
                Some(entry) => {
                    if !quiet {
                        println!("  \u{2192} {name} at {}", entry.root.display());
                    }
                    entry.root.clone()
                }
                None => {
                    eprintln!("  error: no registered project named \"{name}\".");
                    eprintln!(
                        "  registered: {}",
                        if registry.entries().is_empty() {
                            "(none — `svrn project register` from a repo first)".to_string()
                        } else {
                            registry
                                .entries()
                                .iter()
                                .map(|e| e.corpus_id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    );
                    eprintln!("  refusing to rebuild a different project instead.");
                    return 1;
                }
            }
        }
    };

    let abs_path = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    let config = load_project_config(&repo_root);
    // An explicit `--name` wins: it is the id the caller named and the id
    // the registry entry we resolved `repo_root` from is keyed by, so
    // writing the rebuild under a *different* id derived from the
    // directory would leave the registered project's graph untouched —
    // the same wrong-target failure in a quieter form.
    let corpus_id = explicit_name
        .clone()
        .or_else(|| {
            config
                .as_ref()
                .and_then(|c| c["corpus_id"].as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| {
            repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string()
        });

    // Workspace roots: read from project.json if stored (monorepo mode set at init).
    // None means export_all will auto-detect from the git root (single-repo default).
    let scip_workspace_roots: Option<Vec<PathBuf>> = config
        .as_ref()
        .and_then(|c| c["workspace_roots"].as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(PathBuf::from))
                .collect()
        });

    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");

    if !quiet {
        eprintln!("  Refreshing call graph...");
    }

    // Surface missing exporters before running so the user gets actionable
    // guidance rather than a silent empty call graph.
    {
        let check_roots: Vec<PathBuf> = scip_workspace_roots
            .as_deref()
            .map(|r| r.to_vec())
            .unwrap_or_else(|| vec![abs_path.clone()]);
        let check = corpus_engine_scip::scip_export::check_exporters(&check_roots);
        for m in &check.missing {
            if !quiet {
                eprintln!(
                    "  \u{26a0} {} exporter ({}) not found in PATH",
                    m.language_id, m.command
                );
                eprintln!("    {}", m.install_hint);
                eprintln!("    Install it and re-run `svrn project refresh`");
            }
        }
    }

    // Use the integrity-checking open so a v1 / corrupt DB left over
    // from a past schema is self-healed into a fresh v2 DB rather than
    // wedging on `no such column: corpus_id` at index-creation time —
    // which is exactly what `svrn doctor`'s `scip_integrity`
    // repair hint used to run into. Mirrors `Reindexer::register`.
    let graph =
        match corpus_engine_scip::ScipGraph::open_with_integrity(&scip_graph_path, &corpus_id) {
            Ok(g) => g,
            Err(corpus_engine_scip::OpenError::Corrupt { moved_to }) => {
                if !quiet {
                    eprintln!(
                        "  \u{26a0} SCIP DB was corrupt; quarantined to {}",
                        moved_to.display()
                    );
                    eprintln!("    Rebuilding from scratch.");
                }
                match corpus_engine_scip::ScipGraph::open_with_integrity(
                    &scip_graph_path,
                    &corpus_id,
                ) {
                    Ok(g) => g,
                    Err(e) => {
                        eprintln!("error: cannot open SCIP graph after quarantine: {e}");
                        return 1;
                    }
                }
            }
            Err(corpus_engine_scip::OpenError::SchemaMismatch { found, expected }) => {
                if !quiet {
                    eprintln!(
                    "  \u{26a0} SCIP DB schema v{found} is stale (current: v{expected}); resetting."
                );
                }
                if let Err(e) = reset_scip_db(&scip_graph_path) {
                    eprintln!(
                        "error: cannot reset stale SCIP DB at {}: {e}",
                        scip_graph_path.display()
                    );
                    return 1;
                }
                match corpus_engine_scip::ScipGraph::open_with_integrity(
                    &scip_graph_path,
                    &corpus_id,
                ) {
                    Ok(g) => g,
                    Err(e) => {
                        eprintln!("error: cannot open SCIP graph after reset: {e}");
                        return 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("error: cannot open SCIP graph: {e}");
                eprintln!("Run `svrn project init` first.");
                return 1;
            }
        };

    // Get pre-refresh counts for delta display.
    let prev_symbols = graph.symbol_count().await;
    let prev_refs = graph.ref_count().await;

    let progress_fn = |p: corpus_engine_scip::scip_export::ScipProgress<'_>| {
        if quiet {
            return;
        }
        match p {
            corpus_engine_scip::scip_export::ScipProgress::Exporting { language } => {
                eprint!("\r    Exporting {language}...      ");
            }
            corpus_engine_scip::scip_export::ScipProgress::Ingested {
                language,
                symbols,
                refs,
            } => {
                eprintln!(
                    "\r    \u{2713} {language}: {} symbols, {} references    ",
                    symbols, refs
                );
            }
            corpus_engine_scip::scip_export::ScipProgress::Skipped { language, reason } => {
                eprintln!("\r    \u{26a0} {language}: skipped ({reason})    ");
            }
        }
    };

    let start = std::time::Instant::now();
    // ONE writer for the SCIP DB across the daemon and `--local`
    // paths (order defect 8): export into a staging file under the
    // cross-process rebuild lock, then atomically rename over the
    // live DB. The pre-fix path opened the live DB directly and
    // collided with the daemon's handle ("attempt to write a
    // readonly database", live 2026-08-14). The graph handle above
    // is read-only from here on (delta counts only).
    match corpus_engine_scip::ScipGraph::export_to_live(
        &abs_path,
        scip_workspace_roots.as_deref(),
        &scip_graph_path,
        &corpus_id,
        "manual refresh",
        prev_symbols,
        &progress_fn,
    )
    .await
    {
        Err(e) if e == corpus_engine_scip::REBUILD_COALESCED => {
            eprintln!("error: another writer holds the rebuild lock (the daemon is rebuilding).");
            eprintln!("  Re-run `svrn project refresh` once the daemon's rebuild completes.");
            1
        }
        Err(e) => {
            eprintln!("error: SCIP export failed: {e}");
            1
        }
        Ok(outcome) => {
            let elapsed = start.elapsed().as_secs();
            let sym_delta = outcome.summary.total_symbols as i64 - prev_symbols as i64;
            let ref_delta = outcome.summary.total_refs as i64 - prev_refs as i64;

            if !quiet {
                eprintln!(
                    "    \u{2713} {} symbols ({}{})",
                    outcome.summary.total_symbols,
                    if sym_delta >= 0 { "+" } else { "" },
                    sym_delta
                );
                eprintln!(
                    "    \u{2713} {} edges ({}{})",
                    outcome.summary.total_refs,
                    if ref_delta >= 0 { "+" } else { "" },
                    ref_delta
                );
                eprintln!("    \u{2713} Done in {elapsed} seconds");
            }
            // Fall through to the LanceDB rebuild gate — see the
            // matching branch in the daemon-nudge path above for
            // the rationale.
            maybe_rebuild_lancedb_corpus(
                &abs_path,
                &corpus_id,
                &data_dir,
                force_rebuild_index,
                quiet,
            )
            .await
        }
    }
}

// ─── Nudge verification ──────────────────────────────────────

/// Budget for waiting on a nudge to complete. A full-workspace SCIP
/// export on a large repo can take minutes; 50 minutes is far beyond
/// any legitimate rebuild (the daemon's own watchdog aborts wedged
/// rebuilds at 45 minutes), so expiry means the daemon is wedged or
/// gone — never "still going".
const VERIFY_BUDGET: std::time::Duration = std::time::Duration::from_secs(50 * 60);

/// Poll cadence for nudge verification.
const VERIFY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// How many consecutive failed `/v1/projects` polls count as "daemon
/// gone" (a daemon restart can transiently refuse connections for
/// seconds; one bad poll is a hiccup, three in a row is a story).
const VERIFY_DAEMON_GONE_POLLS: u32 = 3;

/// The five-verdict answer a `refresh` nudge can get (ARCH §18.2 —
/// never a silent sixth). `Pending` is internal to the poll loop and
/// never returned to the caller.
#[derive(Debug, Clone, PartialEq)]
enum NudgeVerdict {
    /// The on-disk graph is indexed at git HEAD.
    Completed,
    /// The daemon recorded a rebuild failure after the nudge.
    Failed {
        error: String,
    },
    /// The rebuild task panicked / the watcher is in the crashed
    /// state. The daemon self-clears its slots, so one re-nudge is
    /// the sanctioned recovery.
    Crashed {
        reason: String,
    },
    /// Neither completed nor failed within the budget.
    Wedged {
        detail: String,
    },
    /// The daemon stopped answering mid-verification.
    DaemonGone {
        detail: String,
    },
    Pending,
}

/// Pure verdict from one `/v1/projects` sample (ARCH §18.5 — one
/// sample, one verdict, no hidden state).
fn nudge_verdict(
    status_state: Option<&str>,
    last_error: Option<&str>,
    last_error_ts: Option<u64>,
    baseline_error_ts: u64,
    indexed_head: Option<&str>,
    git_head: Option<&str>,
    since_start: std::time::Duration,
) -> NudgeVerdict {
    if status_state == Some("crashed") || status_state == Some("disabled") {
        return NudgeVerdict::Crashed {
            reason: last_error
                .map(str::to_string)
                .unwrap_or_else(|| format!("watcher is in the {} state", status_state.unwrap())),
        };
    }
    // A failure recorded AFTER the pre-nudge baseline belongs to this
    // nudge; one recorded before it is stale history and not our
    // fault. `record_rebuild_success` clears the record, so a fresh
    // failure here means the current rebuild genuinely failed.
    if let (Some(e), Some(ts)) = (last_error, last_error_ts) {
        if ts > baseline_error_ts {
            return NudgeVerdict::Failed {
                error: e.to_string(),
            };
        }
    }
    if let (Some(i), Some(g)) = (indexed_head, git_head) {
        if i == g {
            return NudgeVerdict::Completed;
        }
    }
    if since_start > VERIFY_BUDGET {
        return NudgeVerdict::Wedged {
            detail: format!(
                "graph never reached git HEAD within {:.0} min (indexed at: {})",
                VERIFY_BUDGET.as_secs_f64() / 60.0,
                indexed_head.unwrap_or("<never indexed>"),
            ),
        };
    }
    NudgeVerdict::Pending
}

/// Poll `/v1/projects` until the graph reaches git HEAD, the rebuild
/// fails loudly, the budget expires, or the daemon stops answering.
/// Returns a terminal verdict; `Pending` is never returned.
async fn await_rebuild_completion(
    corpus_id: &str,
    repo_root: &Path,
    baseline_error_ts: u64,
    quiet: bool,
) -> NudgeVerdict {
    let git_head = git_head(repo_root);
    if git_head.is_none() {
        // Absence is REFUSED, not defaulted (ARCH §18.3): without a
        // readable HEAD we cannot verify daemon completion by commit,
        // and a head-less poll would burn the whole budget to say so.
        // Fall back to the local rebuild, which produces the graph
        // and its own honest success/failure either way.
        return NudgeVerdict::Wedged {
            detail: format!(
                "git HEAD unreadable at {} — daemon completion cannot be verified; falling back to the in-process rebuild",
                repo_root.display()
            ),
        };
    }
    let start = std::time::Instant::now();
    let mut gone_polls: u32 = 0;
    loop {
        let project = match daemon_get("/v1/projects").await {
            Ok(body) => body["projects"]
                .as_array()
                .and_then(|ps| {
                    ps.iter()
                        .find(|p| p["corpus_id"].as_str() == Some(corpus_id))
                })
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            Err(e) => {
                gone_polls += 1;
                if gone_polls >= VERIFY_DAEMON_GONE_POLLS {
                    return NudgeVerdict::DaemonGone { detail: e };
                }
                tokio::time::sleep(VERIFY_POLL_INTERVAL).await;
                continue;
            }
        };
        if project.is_null() {
            return NudgeVerdict::DaemonGone {
                detail: format!("project \"{corpus_id}\" is no longer registered with the daemon"),
            };
        }
        gone_polls = 0;
        let verdict = nudge_verdict(
            project["status"]["scip"]["state"].as_str(),
            project["last_rebuild_error"][0].as_str(),
            project["last_rebuild_error"][1].as_u64(),
            baseline_error_ts,
            project["last_indexed_head"].as_str(),
            git_head.as_deref(),
            start.elapsed(),
        );
        match verdict {
            NudgeVerdict::Pending => {
                if !quiet {
                    eprint!(
                        "\r    Rebuilding in the daemon... ({:.0}s)      ",
                        start.elapsed().as_secs_f64()
                    );
                }
                tokio::time::sleep(VERIFY_POLL_INTERVAL).await;
            }
            terminal => {
                if !quiet {
                    eprintln!("\r                                                        \r");
                }
                return terminal;
            }
        }
    }
}

/// The git commit at HEAD of `root`'s repository, or `None` when the
/// command cannot run (not a git checkout, git missing).
fn git_head(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Decide whether the LanceDB corpus index needs to be rebuilt,
/// then do it (or skip explaining why).
///
/// Rebuild triggers, in precedence order:
///   1. `--rebuild-index` on the command line (operator override).
///   2. The on-disk `_corpus_meta.json.embedding_model` differs from
///      the daemon's currently-configured embed model stem. This is
///      the common "I changed embed models, reindex" path and the
///      one that rescues the historical 768-dim-zero-vector code
///      corpora.
///   3. No `_corpus_meta.json` exists — corpus isn't indexed yet;
///      treat the first refresh as an initial build.
///
/// On skip, prints a one-liner so the user sees the branch was
/// considered rather than silently bypassed (glassbox principle).
async fn maybe_rebuild_lancedb_corpus(
    abs_repo: &Path,
    corpus_id: &str,
    data_dir: &Path,
    force: bool,
    quiet: bool,
) -> i32 {
    let meta_path = data_dir.join(corpus_id).join("_corpus_meta.json");
    let current_embed_stem = sovereign_core::setup_config::SetupConfig::load()
        .ok()
        .and_then(|c| {
            c.models
                .embed
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        });

    let reason = lancedb_rebuild_reason(force, &meta_path, current_embed_stem.as_deref());
    let reason = match reason {
        Some(r) => r,
        None => {
            if !quiet {
                eprintln!(
                    "  \u{2713} LanceDB index current — skipping (pass --rebuild-index to force)."
                );
            }
            return 0;
        }
    };

    if !quiet {
        eprintln!();
        eprintln!("  Rebuilding LanceDB corpus index: {reason}");
    }
    match sovereign_cli_shared::code_index::rebuild_code_corpus(abs_repo, corpus_id, data_dir).await
    {
        Ok(stats) => {
            if !quiet {
                eprintln!(
                    "  \u{2713} LanceDB: {} chunks in {}s ({} KB)",
                    stats.chunks_created,
                    stats.duration_secs,
                    stats.index_size_bytes / 1024,
                );
            }
            0
        }
        Err(e) => {
            eprintln!("  \u{2717} LanceDB rebuild failed: {e}");
            1
        }
    }
}

/// Returns `Some(human-readable reason)` when the LanceDB index
/// should be rebuilt, `None` when it can be left alone. Pure for
/// testability — no filesystem mutations, no daemon calls.
///
/// Deliberately does NOT do a string comparison between the
/// on-disk `embedding_model` and the daemon's model stem. The
/// corpus-engine ingest path today writes its recipe-level default
/// (`qwen3-embedding-0.6b`) to `_corpus_meta.json` regardless of
/// which model actually produced the vectors, so the names drift
/// even when the embeddings are fully compatible. Dim-based checks
/// are the reliable truth — a 1024-dim query against a 1024-dim
/// index works regardless of model-name cosmetics; a 768-dim
/// index is always the legacy zero-vector artefact and needs
/// rebuilding.
///
/// `current_embed_stem` is accepted for future use (when corpus-
/// engine's ingest is fixed to record the real model name) but is
/// currently unused. The compiler won't flag it unused because we
/// accept it by value.
fn lancedb_rebuild_reason(
    force: bool,
    meta_path: &Path,
    _current_embed_stem: Option<&str>,
) -> Option<String> {
    if force {
        return Some("--rebuild-index flag set".into());
    }
    let meta_bytes = match std::fs::read_to_string(meta_path) {
        Ok(b) => b,
        Err(_) => {
            return Some(format!(
                "no existing index at {} — first build",
                meta_path.display()
            ));
        }
    };
    let meta: serde_json::Value = match serde_json::from_str(&meta_bytes) {
        Ok(v) => v,
        Err(e) => {
            return Some(format!(
                "could not parse existing {}: {e}",
                meta_path.display()
            ));
        }
    };
    if meta.get("embedding_dimensions").and_then(|v| v.as_u64()) == Some(768) {
        return Some("on-disk index is 768-dim (legacy zero-vector code index)".into());
    }
    // Content-age gate (order defect 3): `_corpus_meta.json` is
    // re-stamped by the ingest's write_meta on every index build, so
    // its mtime IS the content age — a 20-day-old file means
    // 20-day-old vectors, regardless of how current the model name
    // or dims look. Matches fieldglass's own freshness warn window
    // (chunk_index_age_days > 7.0).
    if let Ok(meta) = std::fs::metadata(meta_path) {
        if let Ok(modified) = meta.modified() {
            if let Ok(age) = std::time::SystemTime::now().duration_since(modified) {
                let days = age.as_secs() / 86400;
                if age.as_secs() > MAX_LANCEDB_INDEX_AGE_DAYS * 86400 {
                    return Some(format!(
                        "index content is {days}d old — older than the {}d freshness window",
                        MAX_LANCEDB_INDEX_AGE_DAYS
                    ));
                }
            }
        }
    }
    None
}

/// How old `_corpus_meta.json` (i.e. the index content) may be before
/// `refresh` rebuilds it unprompted. Mirrors fieldglass's
/// `chunk_index_age_days > 7.0` warn window.
const MAX_LANCEDB_INDEX_AGE_DAYS: u64 = 7;

#[cfg(test)]
mod lancedb_rebuild_tests {
    use super::*;

    #[test]
    fn force_flag_always_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        assert!(lancedb_rebuild_reason(true, &missing, Some("any")).is_some());
    }

    #[test]
    fn missing_meta_triggers_first_build() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("_corpus_meta.json");
        let r = lancedb_rebuild_reason(false, &missing, Some("qwen-embedding-0.6b")).unwrap();
        assert!(r.contains("no existing index"));
    }

    #[test]
    fn matching_embed_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("_corpus_meta.json");
        std::fs::write(
            &p,
            r#"{"embedding_model":"qwen-embedding-0.6b","embedding_dimensions":1024}"#,
        )
        .unwrap();
        assert!(lancedb_rebuild_reason(false, &p, Some("qwen-embedding-0.6b")).is_none());
    }

    #[test]
    fn name_difference_alone_does_not_trigger_rebuild() {
        // corpus-engine's ingest writes `qwen3-embedding-0.6b` as
        // a recipe-default cosmetic label even when the real
        // embed model is `qwen-embedding-0.6b`. If the dims
        // match, the vectors are compatible; name drift should
        // NOT force a pointless reindex.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("_corpus_meta.json");
        std::fs::write(
            &p,
            r#"{"embedding_model":"qwen3-embedding-0.6b","embedding_dimensions":1024}"#,
        )
        .unwrap();
        assert!(lancedb_rebuild_reason(false, &p, Some("qwen-embedding-0.6b")).is_none());
    }

    #[test]
    fn legacy_768_dim_triggers_rebuild_even_with_matching_name() {
        // Historical case: code indexes wrote `qwen-embedding-0.6b`
        // as the model name but hardcoded 768-dim zero vectors.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("_corpus_meta.json");
        std::fs::write(
            &p,
            r#"{"embedding_model":"qwen-embedding-0.6b","embedding_dimensions":768}"#,
        )
        .unwrap();
        let r = lancedb_rebuild_reason(false, &p, Some("qwen-embedding-0.6b")).unwrap();
        assert!(r.contains("768-dim"));
    }

    // RED-FIRST (order code-intel-reindexer-fix, defect 3): the
    // pre-fix gate had no content-age input at all — a 20-day-old
    // index read "current". `_corpus_meta.json` is re-stamped by
    // write_meta on every index build, so its mtime IS the content
    // age. The gate must say stale for an old index and skip for a
    // fresh one.
    #[test]
    fn stale_index_age_triggers_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("_corpus_meta.json");
        std::fs::write(
            &p,
            r#"{"embedding_model":"qwen-embedding-0.6b","embedding_dimensions":1024}"#,
        )
        .unwrap();
        let twenty_days_ago =
            std::time::SystemTime::now() - std::time::Duration::from_secs(20 * 24 * 3600);
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(twenty_days_ago))
            .unwrap();
        let r = lancedb_rebuild_reason(false, &p, Some("qwen-embedding-0.6b"))
            .expect("a 20-day-old index must trigger a rebuild");
        assert!(r.contains("older than"), "reason: {r}");
    }

    #[test]
    fn fresh_index_age_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("_corpus_meta.json");
        std::fs::write(
            &p,
            r#"{"embedding_model":"qwen-embedding-0.6b","embedding_dimensions":1024}"#,
        )
        .unwrap();
        assert!(lancedb_rebuild_reason(false, &p, Some("qwen-embedding-0.6b")).is_none());
    }

    // ── nudge_verdict ────────────────────────────────────────
    //
    // The honesty surface of the nudge path (order defect 1): every
    // poll sample must map to a named verdict, never a silent
    // success. Red-first: `cmd_refresh`'s old nudge branch printed
    // "✓ Rebuild nudged" on any 2xx — these tests pin the four
    // verdicts that replace it.

    fn secs(n: u64) -> std::time::Duration {
        std::time::Duration::from_secs(n)
    }

    #[test]
    fn verdict_completed_when_graph_reaches_git_head() {
        let v = nudge_verdict(
            Some("idle"),
            None,
            None,
            0,
            Some("abc123"),
            Some("abc123"),
            secs(30),
        );
        assert_eq!(v, NudgeVerdict::Completed);
    }

    #[test]
    fn verdict_reports_new_failure_but_not_stale_ones() {
        // Failure recorded BEFORE the baseline is history, not this
        // nudge's fault — keep polling.
        let v = nudge_verdict(
            Some("idle"),
            Some("boom"),
            Some(100),
            200,
            Some("abc"),
            Some("def"),
            secs(30),
        );
        assert_eq!(v, NudgeVerdict::Pending);
        // Failure AFTER the baseline belongs to this nudge.
        let v = nudge_verdict(
            Some("idle"),
            Some("boom"),
            Some(300),
            200,
            Some("abc"),
            Some("def"),
            secs(30),
        );
        assert_eq!(
            v,
            NudgeVerdict::Failed {
                error: "boom".into()
            }
        );
    }

    #[test]
    fn verdict_never_silent_on_wedged_state() {
        let v = nudge_verdict(
            Some("idle"),
            None,
            None,
            0,
            Some("abc"),
            Some("def"),
            secs(51 * 60),
        );
        assert!(matches!(v, NudgeVerdict::Wedged { .. }));
    }

    #[test]
    fn verdict_crashed_status_is_loud() {
        let v = nudge_verdict(
            Some("crashed"),
            Some("panic in export"),
            Some(300),
            0,
            Some("abc"),
            Some("abc"),
            secs(5),
        );
        assert!(matches!(v, NudgeVerdict::Crashed { .. }));
    }

    #[test]
    fn verdict_head_mismatch_keeps_polling_then_wedges() {
        // Graph still at the OLD head mid-rebuild is the normal
        // in-flight state — must keep polling, not fail early.
        let v = nudge_verdict(
            Some("active"),
            None,
            None,
            0,
            Some("abc"),
            Some("def"),
            secs(30),
        );
        assert_eq!(v, NudgeVerdict::Pending);
    }
}

/// Remove a SCIP graph DB file and its SQLite journal sidecars so
/// a subsequent `open_with_integrity` starts from a clean slate.
/// Missing files are not errors — a partial reset is still a reset.
fn reset_scip_db(db_path: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let p = if suffix.is_empty() {
            db_path.to_path_buf()
        } else {
            let name = db_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("scip_graph.db");
            db_path.with_file_name(format!("{name}{suffix}"))
        };
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
