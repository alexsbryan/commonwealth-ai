// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code index` — build or refresh a repository's chunk corpus.
//!
//! Ported from `sovereign-cli-dev/src/code_cmd.rs` (2026-08-06) so the verb
//! ships in the release binary. It was a `DEV_VERB` served by a sibling that
//! the tarball never contained, while `svrn doctor` told users to run it.
//!
//! # This does not run a model
//!
//! Embeddings come from the daemon over loopback HTTP: `build_daemon_embed_fn`
//! probes `/v1/models` and refuses up front if nothing answers, then talks to
//! `/embeddings` through `oicp-client` — a pure `reqwest` client with no
//! llama.cpp. The daemon owns the one inference stack in the system and this
//! module must never acquire a second one.
//!
//! The heavier `code` subcommands (`brief`, `fieldglass`, `arch-report`,
//! `dry-report`, `suggest-seams`, `check-spec`, `watch`) stay in the workbench:
//! they pull `sovereign-tools`, `sovereign-mesh` and `sovereign-work-atlas`,
//! none of which the index path needs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn};
use oicp_client::RemoteApiProvider;
use sovereign_cli_shared::help::{Help, HelpSection};
use sovereign_core::traits::InferenceProvider;

/// Help for `svrn code index` specifically. The workbench's `code` help
/// covers a dozen subcommands that do not ship here; advertising them from
/// this binary would be the same defect this port exists to fix.
const HELP: Help = Help {
    command: "svrn code index",
    summary: "Index a repository into a searchable code corpus.",
    sections: &[HelpSection::Usage(
        "svrn code index <path> [--corpus-id <id>] [--data-dir <dir>]\n\
         svrn code index <path> --full          (re-embed everything)\n\
         svrn code index <path> --incremental   (force the changed-files path)",
    )],
};

/// Subcommands of `svrn code` this binary serves. Everything else under
/// `code` is workbench-only; see `refuse_workbench_subcommand`.
const IN_PROCESS: &[&str] = &["index"];

/// Returns `Some(exit_code)` when this module owns the subcommand, `None` when
/// the caller should fall through to the `sovereign-cli-dev` sibling.
pub async fn try_run(args: &[String]) -> Option<i32> {
    let sub = args.first()?;
    if !IN_PROCESS.contains(&sub.as_str()) {
        return None;
    }
    Some(cmd_index(&args[1..]).await)
}

/// Refuse a `code` subcommand that still lives in the workbench, naming what
/// this build can do instead of pointing at a `cargo build` the user has no
/// checkout for. Same contract as `project_registry::refuse_workbench_subcommand`.
pub fn refuse_workbench_subcommand(sub: Option<&str>) -> i32 {
    match sub {
        Some(s) => eprintln!("svrn code {s}: not available in this build."),
        None => eprintln!("svrn code: missing subcommand."),
    }
    eprintln!();
    eprintln!("  Available here:");
    eprintln!("    svrn code index <path> [--corpus-id <id>] [--full|--incremental]");
    eprintln!();
    eprintln!("  The analysis subcommands (brief, fieldglass, arch-report, dry-report,");
    eprintln!("  suggest-seams, check-spec) are developer tooling and ship separately.");
    2
}

async fn cmd_index(args: &[String]) -> i32 {
    // Handle `--help` BEFORE the flag loop. The loop's catch-all treats any
    // unknown `-` flag as a warning and falls through, so `code index --help`
    // printed "unknown flag '--help' — ignored", then "error: missing <path>",
    // then the help text, and exited 1. Harmless while the verb was
    // workbench-only; a shipped verb whose `--help` exits non-zero is a defect.
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }

    let mut path_arg: Option<PathBuf> = None;
    let mut corpus_id: Option<String> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut force_full = false;
    let mut force_incremental = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--full" => force_full = true,
            "--incremental" => force_incremental = true,
            "--corpus-id" => {
                i += 1;
                corpus_id = args.get(i).cloned();
                if corpus_id.is_none() {
                    eprintln!("error: --corpus-id requires a value");
                    return 1;
                }
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
                if data_dir.is_none() {
                    eprintln!("error: --data-dir requires a value");
                    return 1;
                }
            }
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            p => {
                path_arg = Some(PathBuf::from(p));
            }
        }
        i += 1;
    }

    let Some(path) = path_arg else {
        eprintln!("error: missing <path>");
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    };

    let abs_path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve path {}: {e}", path.display());
            return 1;
        }
    };

    let corpus_id = corpus_id.unwrap_or_else(|| {
        abs_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "codebase".to_string())
    });

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    if force_full && force_incremental {
        eprintln!("error: --full and --incremental are mutually exclusive");
        return 1;
    }

    // ── Choose the mode, out loud ─────────────────────────────
    // Before this existed, `code index` had exactly one behaviour — clear the
    // LanceDB artifacts and re-embed the whole repository — and no way to tell
    // from the invocation that that is what you were about to pay for. The
    // decision is now explicit, printed, and overridable in both directions.
    use crate::code_index_incremental as inc;
    let index_dir = data_dir.join(&corpus_id);
    let head_now = inc::git_head(&abs_path);
    let is_git = head_now.is_some();
    let dirty_now = inc::git_dirty_paths(&abs_path);

    // Prefer the stamp; fall back to the corpus's own last_updated + file
    // mtimes. The fallback is what makes this useful on day one: no corpus
    // anywhere has a stamp yet, and without it every one of them would owe a
    // full rebuild before incremental could ever engage.
    let resolved = match inc::IndexState::load(&index_dir) {
        Some(state) => inc::resolve_from_stamp(&state, &abs_path, is_git),
        None => inc::resolve_from_mtime(
            inc::corpus_last_updated(&index_dir),
            inc::source_files_with_mtime(&abs_path),
        ),
    };

    let plan = inc::decide(
        index_dir.exists(),
        resolved,
        &dirty_now,
        force_full,
        force_incremental,
    );

    match plan {
        inc::Plan::UpToDate { base } => {
            // `base` is already a partner-facing label ("commit a1b2c3d4" /
            // "the last index run") — re-truncating it here printed "commit 2".
            eprintln!(
                "✓ Corpus '{corpus_id}' is already current as of {base} — nothing changed since \
                 the last index."
            );
            eprintln!("  Pass --full to rebuild from scratch anyway.");
            0
        }
        inc::Plan::Incremental { files, base } => {
            eprintln!(
                "Incremental refresh of '{corpus_id}': {} changed file(s) since {base}",
                files.len(),
            );
            run_incremental(
                &abs_path, &corpus_id, &data_dir, &files, &head_now, &dirty_now,
            )
            .await
        }
        inc::Plan::Full { reason } => {
            eprintln!("Full rebuild of '{corpus_id}' — {reason}.");
            eprintln!("Every chunk will be re-embedded; this is the slow path.");
            match rebuild_code_corpus(&abs_path, &corpus_id, &data_dir).await {
                Ok(stats) => {
                    eprintln!();
                    eprintln!(
                        "✓ Indexed {} chunks in {}s",
                        stats.chunks_created, stats.duration_secs
                    );
                    eprintln!(
                        "  Corpus: {}  ({} KB on disk)",
                        stats.corpus_id,
                        stats.index_size_bytes / 1024,
                    );
                    eprintln!("  Location: {}/{}", data_dir.display(), stats.corpus_id);
                    stamp_index_state(&index_dir, &abs_path, &head_now, &dirty_now);
                    0
                }
                Err(e) => {
                    eprintln!();
                    eprintln!("✗ Indexing failed: {e}");
                    1
                }
            }
        }
    }
}

/// Write the stamp that makes the NEXT run incremental. Called after both
/// modes — a full rebuild that forgets to stamp condemns the following run to
/// another full rebuild, which is how the corpus got 28 days stale in the
/// first place.
fn stamp_index_state(index_dir: &Path, root: &Path, head: &Option<String>, dirty: &[String]) {
    let Some(head) = head else {
        // Not a git repo: no baseline to diff against, so deliberately leave
        // no stamp rather than one that would be treated as usable.
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::code_index_incremental::IndexState::new(
        head.clone(),
        dirty.to_vec(),
        root.display().to_string(),
        now,
    )
    .save(index_dir);
}

/// Drive `CorpusEngine::reindex_file` over the changed set.
///
/// Embeds through the daemon exactly as `rebuild_code_corpus` does — NOT the
/// zero-vector `EmbedFn` that `cmd_watch` installs. Writing zero vectors into a
/// vector-searchable corpus silently destroys semantic search for those chunks
/// (cosine similarity against a zero vector is meaningless), so this path
/// refuses to run rather than fall back to it.
async fn run_incremental(
    root: &Path,
    corpus_id: &str,
    data_dir: &Path,
    files: &[String],
    head: &Option<String>,
    dirty: &[String],
) -> i32 {
    let (embed, embed_model_name) = match build_daemon_embed_fn().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("✗ {e}");
            eprintln!(
                "\nIncremental indexing embeds through the daemon so the changed chunks land in \
                 the same embedding space as the rest of the corpus. Start it with \
                 `svrn daemon run` and re-run."
            );
            return 1;
        }
    };
    let engine = CorpusEngine::new(data_dir.to_path_buf(), data_dir.to_path_buf(), embed)
        .with_embedding_model(&embed_model_name);

    let started = std::time::Instant::now();
    let (mut updated, mut unchanged, mut deleted, mut skipped, mut failed) = (0, 0, 0, 0, 0);
    let mut chunks_written = 0usize;

    for (n, rel) in files.iter().enumerate() {
        let abs = root.join(rel);
        match engine.reindex_file(corpus_id, &abs, root).await {
            Ok(corpus_engine::engine::reindex::ReindexResult::Updated {
                chunks_written: w,
                ..
            }) => {
                // `reindex_file` reports 0 written when every chunk hash-matched
                // a committed row — the whole point of the delta path. Counting
                // that as "updated" would overstate the work done.
                if w == 0 {
                    unchanged += 1;
                } else {
                    updated += 1;
                    chunks_written += w;
                }
            }
            Ok(corpus_engine::engine::reindex::ReindexResult::Deleted { .. }) => deleted += 1,
            Ok(corpus_engine::engine::reindex::ReindexResult::Skipped) => skipped += 1,
            Err(e) => {
                failed += 1;
                eprintln!("  ! {rel}: {e}");
            }
        }
        if files.len() > 20 && (n + 1) % 20 == 0 {
            eprintln!("  … {}/{} files", n + 1, files.len());
        }
    }

    eprintln!();
    if failed > 0 {
        // A partial refresh must not stamp: the next run has to revisit the
        // files that failed, and a stamp would move the baseline past them.
        eprintln!(
            "✗ {failed} file(s) failed to re-index — leaving the index stamp untouched so the \
             next run retries them."
        );
        eprintln!(
            "  {updated} updated ({chunks_written} chunks), {unchanged} unchanged, {deleted} \
             deleted, {skipped} skipped"
        );
        return 1;
    }

    eprintln!(
        "✓ Incremental refresh complete in {}s",
        started.elapsed().as_secs()
    );
    // Both counters are FILE counts. Within an updated file the engine's
    // chunk-level hash gate embeds only the chunks that actually differ, so
    // `chunks_written` is routinely far below the file's total chunk count —
    // don't read it as "the file was re-embedded whole".
    eprintln!("  {updated} file(s) changed — {chunks_written} chunk(s) embedded");
    eprintln!("  {unchanged} file(s) already current (every chunk hash-matched)");
    if deleted > 0 {
        eprintln!("  {deleted} removed from the index");
    }
    if skipped > 0 {
        eprintln!("  {skipped} skipped (not a recognised source language)");
    }
    eprintln!("  Location: {}/{}", data_dir.display(), corpus_id);
    stamp_index_state(&data_dir.join(corpus_id), root, head, dirty);
    0
}

/// Full rebuild of a code corpus's LanceDB index. Shared between
/// `svrn code index` and `svrn project refresh` so both
/// surfaces write exactly the same thing: an ephemeral code-extract
/// recipe, embedded through the running daemon, ingested to
/// `<data_dir>/<corpus_id>/`.
///
/// Bails early (error, never zero-vector fallback) when the daemon
/// is unreachable — see the `build_daemon_embed_fn` docstring for
/// rationale.
pub(crate) async fn rebuild_code_corpus(
    root: &std::path::Path,
    corpus_id: &str,
    data_dir: &std::path::Path,
) -> std::result::Result<corpus_engine::IngestResult, String> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("cannot create data dir {}: {e}", data_dir.display()))?;

    // A `rebuild` is a rebuild. Clear prior LanceDB state so
    // `create_empty_table` doesn't trip with `Table 'chunks' already
    // exists`. Keep the SCIP graph DB (`scip_graph.db*`) intact —
    // it's owned by the daemon's Reindexer on a parallel cadence,
    // and wiping it here would race with a just-nudged rebuild.
    //
    // Two targets: the canonical `<corpus>/` directory AND every
    // `<corpus>-partition-*/` sibling. The engine writes new ingests
    // into a partition directory and only renames to canonical at
    // finalize; a stale partition from a prior run would make
    // `create_empty_table` collide on the second pass.
    let target = data_dir.join(corpus_id);
    if target.exists() {
        clear_lancedb_artifacts(&target).map_err(|e| {
            format!(
                "cannot clear existing LanceDB index at {}: {e}",
                target.display()
            )
        })?;
    }
    clear_partitions_for(data_dir, corpus_id).map_err(|e| {
        format!(
            "cannot clear partition dirs under {}: {e}",
            data_dir.display()
        )
    })?;

    // Vector ANN enabled — every corpus on this node shares one
    // embedding model so the `embedding_dimensions` is consistent
    // across knowledge + code indexes. Symbol lookup still uses
    // metadata filter pushdown; vector search is additive.
    let recipe_toml = format!(
        r#"[corpus]
id = "{corpus_id}"
name = "{corpus_id}"
description = "Local code corpus generated by `svrn code index`"
# NOTE: deliberately NOT `kind = "code"`. Retrieval admits only
# `Knowledge | Catalog`, and CODE_INTEL_CHAT.md routes code questions
# through the knowledge path — so tagging this `code` would remove the
# repo from chat. Code-ness is detected from the on-disk `scip_graph.db`
# (`sovereign_tools::code::has_code_graph`), not from this field.
license = "private"
mesh_sharing = false
size_compressed_gb = 0
size_indexed_gb = 0

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "code"
context_lines = 3
max_lines_per_chunk = 150

[chunk]
type = "passthrough"

[index]
fts = true
vector = true
"#,
        corpus_id = corpus_id,
        path = root.display(),
    );

    let tempdir = tempfile_dir().map_err(|e| format!("cannot create temp dir: {e}"))?;
    let recipe_path = tempdir.join(format!("{corpus_id}.toml"));
    std::fs::write(&recipe_path, recipe_toml)
        .map_err(|e| format!("cannot write ephemeral recipe: {e}"))?;

    let (embed, embed_model_name) = build_daemon_embed_fn().await.map_err(|e| {
        format!(
            "{e}\n\n`svrn code index` / `svrn project refresh` now embed via the daemon \
             so code corpora share the standard embedding model. Start the daemon with \
             `svrn daemon run` and re-run this command."
        )
    })?;
    // Pass the embed model stem through so `_corpus_meta.json`
    // records exactly what produced the vectors (not the engine's
    // legacy default). See `corpus-engine::with_embedding_model`
    // rationale.
    let engine = CorpusEngine::new(tempdir.clone(), data_dir.to_path_buf(), embed)
        .with_embedding_model(&embed_model_name);

    eprintln!("Indexing {} as corpus '{corpus_id}'", root.display());
    eprintln!("Index directory: {}", data_dir.display());
    eprintln!();

    let spec = CorpusSpec::RecipePath(recipe_path);
    engine
        .ingest(&spec, None)
        .await
        .map_err(|e| format!("ingest failed: {e}"))
}

pub(crate) fn default_data_dir() -> Option<PathBuf> {
    // Mirrors project_cmd::default_data_dir; both just wrap
    // `util::dirs::sovereign_indexes()` but keep the Option return so
    // existing `.or_else(default_data_dir)` callers stay stable.
    let p = sovereign_cli_shared::dirs::sovereign_indexes();
    if p == std::path::Path::new(".") {
        None
    } else {
        Some(p)
    }
}

/// Remove every entry in `dir` that belongs to the LanceDB index
/// (the `_corpus_meta.json`, the `.lance` table dirs, the `_indices`
/// directory, any FTS/vector build scratch). Preserve anything named
/// `scip_graph.db*` — the daemon's Reindexer owns those.
///
/// If the directory ends up empty after clearing, remove the directory
/// itself. Reason: `finalise_solo_ingest` promotes
/// `<corpus>-partition-<node>/` to canonical `<corpus>/` via rename,
/// and that rename is skipped when the canonical path already
/// exists — even if empty. An empty leftover would silently leave
/// the fresh ingest stranded in the partition path.
///
/// Returns the first IO error encountered; partial cleanup is fine
/// since the next `create_empty_table` will still succeed against
/// whatever's left as long as the `chunks` table itself is gone.
fn clear_lancedb_artifacts(dir: &std::path::Path) -> std::io::Result<()> {
    let mut any_kept = false;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("scip_graph.db") {
            any_kept = true;
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    if !any_kept {
        // Swallow the error — a racing observer could have created
        // a file between our last read and this rmdir. The next
        // ingest step will create a fresh partition either way.
        let _ = std::fs::remove_dir(dir);
    }
    Ok(())
}

/// Remove every `<corpus_id>-partition-*` directory under `root`.
/// Called before a full rebuild so stale partition-of-self /
/// partition-of-peer dirs don't collide with the fresh ingest's
/// `create_empty_table` call.
///
/// Non-partition siblings (other corpora, arbitrary files) are
/// untouched. A missing `root` is not an error — first-ever
/// rebuild on a machine with no indexes yet is a normal state.
fn clear_partitions_for(root: &std::path::Path, corpus_id: &str) -> std::io::Result<()> {
    let prefix = format!("{corpus_id}-partition-");
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

pub(crate) fn tempfile_dir() -> std::io::Result<PathBuf> {
    // Avoid pulling in the `tempfile` crate — sovereign-cli doesn't
    // already use it, and a one-shot per-run dir is enough. Use the
    // system temp dir plus a pid-derived suffix for uniqueness.
    let base = std::env::temp_dir();
    let suffix = format!("sovereign-code-{}", std::process::id());
    let path = base.join(suffix);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Build an `EmbedFn` that POSTs to the running daemon's
/// `/v1/embeddings` endpoint with the daemon's configured embed
/// model. Returns `(EmbedFn, embed_model_stem)` — the stem is the
/// filename of the GGUF without the `.gguf` suffix, matching what
/// the daemon advertises on `/v1/models`.
///
/// Returns `Err(message)` when the daemon is unreachable or the
/// embed model can't be resolved.
///
/// Using the daemon (rather than loading a model in-process) keeps
/// `svrn code index` lightweight — no GPU/RAM for llama.cpp
/// — and guarantees code corpora land in the same embedding space
/// as knowledge corpora.
async fn build_daemon_embed_fn() -> std::result::Result<(EmbedFn, String), String> {
    let cfg = sovereign_core::setup_config::SetupConfig::load()
        .map_err(|e| format!("read ~/.sovereign/config.toml: {e}"))?;
    let port = cfg.daemon.client_port;
    let endpoint = format!("http://localhost:{port}/v1");
    let embed_model = cfg
        .models
        .embed
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "SetupConfig.models.embed has no filename stem".to_string())?
        .to_string();

    // Probe before we return — a daemon-down failure 40 minutes
    // into a 10k-file reindex is much worse than an up-front bail.
    let probe = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| format!("http client build: {e}"))?;
    let probe_url = format!("{endpoint}/models");
    match probe.get(&probe_url).send().await {
        Ok(r) if r.status().is_success() => {}
        Ok(r) => {
            return Err(format!(
                "daemon at :{port} returned {} from /v1/models",
                r.status()
            ));
        }
        Err(_) => {
            return Err(format!("daemon unreachable at localhost:{port}"));
        }
    }

    // `RemoteApiProvider` is constructed with the embed model as
    // its single `model_id`. Its `InferenceProvider::embed` sends
    // `{"model": "<embed_model>", "input": "<text>"}` to
    // `/embeddings`, which is the exact contract we want.
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&endpoint, None, &embed_model, 8192));
    let f = sovereign_core::embed_fn::inference_to_embed_fn(provider);
    Ok((f, embed_model))
}
