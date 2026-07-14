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
             NOT force a LanceDB rebuild). The LanceDB index auto-rebuilds whenever this \
             command detects an embed-model mismatch between `_corpus_meta.json` and \
             `SetupConfig.models.embed` — once the indexes are on the current model, \
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
    if !local {
        let corpus_id = explicit_name
            .clone()
            .unwrap_or_else(|| derive_corpus_id(&repo_root));
        match daemon_post(
            &format!("/v1/projects/{corpus_id}/rebuild"),
            serde_json::json!({ "reason": "cli refresh" }),
        )
        .await
        {
            Ok(_) => {
                if !quiet {
                    println!("  \u{2713} Rebuild nudged for \"{corpus_id}\".");
                    println!("    Check progress with `svrn project watch status {corpus_id}`.");
                }
                // SCIP is nudged; now gate the LanceDB corpus
                // rebuild on either the explicit `--rebuild-index`
                // flag or a detected embed-model mismatch. Common
                // case on an already-migrated installation: no
                // mismatch → no-op → `refresh` stays fast.
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
            Err(e) => {
                if !quiet {
                    eprintln!("  \u{26a0} Daemon nudge failed: {e}");
                    eprintln!("    Falling back to local in-process rebuild.");
                }
                // Fall through to legacy path below.
            }
        }
    }
    let _ = explicit_name;

    let abs_path = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    let config = load_project_config(&repo_root);
    let corpus_id = config
        .as_ref()
        .and_then(|c| c["corpus_id"].as_str())
        .map(|s| s.to_string())
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

    let tempdir = match tempfile_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot create temp dir: {e}");
            return 1;
        }
    };
    let scip_output_dir = tempdir.join("scip");

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
    match corpus_engine_scip::scip_export::export_all(
        &abs_path,
        &scip_output_dir,
        &graph,
        scip_workspace_roots.as_deref(),
        &progress_fn,
    )
    .await
    {
        Ok(summary) => {
            let elapsed = start.elapsed().as_secs();
            let sym_delta = summary.total_symbols as i64 - prev_symbols as i64;
            let ref_delta = summary.total_refs as i64 - prev_refs as i64;

            if !quiet {
                eprintln!(
                    "    \u{2713} {} symbols ({}{})",
                    summary.total_symbols,
                    if sym_delta >= 0 { "+" } else { "" },
                    sym_delta
                );
                eprintln!(
                    "    \u{2713} {} edges ({}{})",
                    summary.total_refs,
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
        Err(e) => {
            eprintln!("error: SCIP export failed: {e}");
            1
        }
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
    match crate::code_cmd::rebuild_code_corpus(abs_repo, corpus_id, data_dir).await {
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
    None
}

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
