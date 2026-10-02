// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code index` — build or refresh a repository's chunk corpus.
//!
//! # One implementation, two binaries
//!
//! This verb shipped in `sovereign-cli` (the dispatcher) AND ran in
//! `sovereign-cli-dev` (the workbench). From the 2026-08-06 port until
//! 2026-08-20 each binary carried its own copy of the whole thing — `cmd_index`,
//! `run_incremental`, `rebuild_code_corpus`, `stamp_index_state`, the two
//! LanceDB cleanup helpers and `build_daemon_embed_fn` — and the copies drifted
//! in three places, each one a defect the other copy did not have:
//!
//!   - `cmd_index --help` exited 1 in the workbench (the flag loop's catch-all
//!     swallowed `--help`, printed "unknown flag", then "missing <path>").
//!   - `build_daemon_embed_fn` told dispatcher users to check
//!     `~/.sovereign/config.toml`, a path that has not been current since the
//!     rebrand; the real file is `~/.svrnmesh/config.toml`.
//!   - the workbench reached `inference_to_embed_fn` through
//!     `sovereign_tools::corpus`, a re-export, rather than its owner
//!     `sovereign_core::embed_fn`.
//!
//! The merge below keeps the correct half of each. It lived in
//! `sovereign-cli-shared` until pb-code-index moved it here, into the code
//! program; the dispatcher now execs this binary for `svrn code index`.
//!
//! # This writes no index itself
//!
//! Code decides WHAT to index — the recipe, the changed files, the stamp — and
//! execs ingest's CLI, `svrn-ingest index`, to write it (phase-b-30 Group 4).
//! Its embedder is the one decider, `corpus_index::host`: the endpoint the
//! discovery ladder finds, asked for this node's configured embed model when
//! one is named. `--fts-only` builds a keyword-only index and needs no
//! endpoint at all. Nothing here needs a daemon, and `symbols` / `callers`
//! need neither this nor ingest.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use corpus_index::{
    corpus::{Corpus, CORPUS_META_FILENAME},
    host,
    types::EmbedFn,
};

use sovereign_cli_base::code_index::tempfile_dir;
use sovereign_cli_base::dirs::default_data_dir;
use sovereign_cli_base::help::{Help, HelpSection};

/// Help for `svrn code index` specifically. The workbench's `code` help
/// covers a dozen subcommands that do not ship here; advertising them from
/// this binary would be the same defect this port exists to fix.
pub const HELP: Help = Help {
    command: "svrn code index",
    summary: "Index a repository into a searchable code corpus.",
    sections: &[HelpSection::Usage(
        "svrn code index <path> [--corpus-id <id>] [--data-dir <dir>]\n\
         svrn code index <path> --full          (re-embed everything)\n\
         svrn code index <path> --incremental   (force the changed-files path)\n\
         svrn code index <path> --fts-only      (keyword-only: no embedder, no vectors)",
    )],
};

pub async fn cmd_index(args: &[String]) -> i32 {
    // Handle `--help` BEFORE the flag loop. The loop's catch-all treats any
    // unknown `-` flag as a warning and falls through, so `code index --help`
    // printed "unknown flag '--help' — ignored", then "error: missing <path>",
    // then the help text, and exited 1. Harmless while the verb was
    // workbench-only; a shipped verb whose `--help` exits non-zero is a defect.
    if sovereign_cli_base::help::wants_help(args) {
        sovereign_cli_base::help::print(&HELP);
        return 0;
    }

    let mut path_arg: Option<PathBuf> = None;
    let mut corpus_id: Option<String> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut force_full = false;
    let mut force_incremental = false;
    let mut fts_only = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--full" => force_full = true,
            "--fts-only" => fts_only = true,
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
        sovereign_cli_base::help::print(&HELP);
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
                &abs_path, &corpus_id, &data_dir, &files, &head_now, &dirty_now, fts_only,
            )
            .await
        }
        inc::Plan::Full { reason } => {
            eprintln!("Full rebuild of '{corpus_id}' — {reason}.");
            if fts_only {
                eprintln!("Keyword-only (FTS) index (--fts-only): no embedder, no vectors.");
            } else {
                eprintln!("Every chunk will be re-embedded; this is the slow path.");
            }
            match rebuild_code_corpus(&abs_path, &corpus_id, &data_dir, fts_only).await {
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

/// Re-index the changed set through `svrn-ingest index --files-from`.
///
/// The embedder is the one a full build gets (`embedder_args`) — NOT the
/// zero-vector `EmbedFn` that `cmd_watch` used to install. Writing zero
/// vectors into a vector-searchable corpus silently destroys semantic search
/// for those chunks (cosine similarity against a zero vector is meaningless),
/// so ingest refuses a file-list run whose `--fts-only` disagrees with the
/// index's stamp rather than mixing the two.
async fn run_incremental(
    root: &Path,
    corpus_id: &str,
    data_dir: &Path,
    files: &[String],
    head: &Option<String>,
    dirty: &[String],
    fts_only: bool,
) -> i32 {
    let started = std::time::Instant::now();
    let list = match tempfile_dir() {
        Ok(d) => d.join(format!("{corpus_id}.changed")),
        Err(e) => {
            eprintln!("✗ cannot create temp dir: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(&list, files.join("\n")) {
        eprintln!(
            "✗ cannot write the changed-file list {}: {e}",
            list.display()
        );
        return 1;
    }
    let mut args = vec![
        "--index-dir".to_string(),
        data_dir.display().to_string(),
        "--files-from".to_string(),
        list.display().to_string(),
        "--corpus".to_string(),
        corpus_id.to_string(),
        "--root".to_string(),
        root.display().to_string(),
    ];
    match embedder_args(fts_only) {
        Ok(more) => args.extend(more),
        Err(e) => {
            eprintln!("✗ {e}");
            return 1;
        }
    }
    let counts = match ingest_index(&args).await {
        Ok((_, counts)) => counts,
        Err(e) => {
            eprintln!("✗ {e}");
            return 1;
        }
    };
    let count = |k: &str| counts[k].as_u64();
    let (
        Some(updated),
        Some(unchanged),
        Some(deleted),
        Some(skipped),
        Some(failed),
        Some(chunks_written),
    ) = (
        count("updated"),
        count("unchanged"),
        count("deleted"),
        count("skipped"),
        count("failed"),
        count("chunks_written"),
    )
    else {
        eprintln!("✗ svrn-ingest index returned counts this verb cannot read: {counts}");
        return 1;
    };

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
/// recipe, built by `svrn-ingest index` into `<data_dir>/<corpus_id>/`.
///
/// Bails early (error, never zero-vector fallback) when no embedder
/// answers, unless `fts_only` asks for a keyword-only index.
pub async fn rebuild_code_corpus(
    root: &std::path::Path,
    corpus_id: &str,
    data_dir: &std::path::Path,
    fts_only: bool,
) -> std::result::Result<IndexBuilt, String> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("cannot create data dir {}: {e}", data_dir.display()))?;

    // A `rebuild` rebuilds THE CHUNK TABLE. Clear the ingest's own artifacts so
    // `create_empty_table` doesn't trip with `Table 'chunks' already exists`,
    // and leave every other occupant of the directory alone — see
    // `clear_ingest_artifacts` for what that cost when it was the other way
    // round. `svrn corpus remove <id>` is the verb for "delete the corpus".
    //
    // Two targets: the canonical `<corpus>/` directory AND every
    // `<corpus>-partition-*/` sibling. The engine writes new ingests into a
    // partition directory and only renames to canonical at finalize; a stale
    // partition from a prior run would make `create_empty_table` collide on
    // the second pass.
    let target = data_dir.join(corpus_id);
    let mut preserved: Vec<String> = Vec::new();
    if target.exists() {
        let kept = clear_ingest_artifacts(&target).map_err(|e| {
            format!(
                "cannot clear existing chunk table at {}: {e}",
                target.display()
            )
        })?;
        preserved.extend(kept.names);
    }
    let kept = clear_partitions_for(data_dir, corpus_id).map_err(|e| {
        format!(
            "cannot clear partition dirs under {}: {e}",
            data_dir.display()
        )
    })?;
    preserved.extend(kept.names);

    // Say what survived. A rebuild regenerates chunk ids, so anything keyed to
    // the old ones is now stale — and the subsystem that owns it is the only
    // thing entitled to decide what that means. Reporting is the contract;
    // deleting on its behalf is what this code used to do.
    if !preserved.is_empty() {
        eprintln!(
            "Preserved {} entr{} not owned by the ingest (chunk ids are regenerated, so \
             anything keyed to the old ones may now be stale):",
            preserved.len(),
            if preserved.len() == 1 { "y" } else { "ies" }
        );
        for name in &preserved {
            eprintln!("  {name}");
        }
    }

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
# (`sovereign_code::has_code_graph`), not from this field.
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
vector = {vector}
"#,
        corpus_id = corpus_id,
        path = root.display(),
        vector = !fts_only,
    );

    let tempdir = tempfile_dir().map_err(|e| format!("cannot create temp dir: {e}"))?;
    let recipe_path = tempdir.join(format!("{corpus_id}.toml"));
    std::fs::write(&recipe_path, recipe_toml)
        .map_err(|e| format!("cannot write ephemeral recipe: {e}"))?;

    eprintln!("Indexing {} as corpus '{corpus_id}'", root.display());
    eprintln!("Index directory: {}", data_dir.display());
    eprintln!();

    let mut args = vec![
        "--recipe".to_string(),
        recipe_path.display().to_string(),
        "--index-dir".to_string(),
        data_dir.display().to_string(),
    ];
    args.extend(embedder_args(fts_only)?);
    match ingest_index(&args).await? {
        (true, result) => serde_json::from_value(result)
            .map_err(|e| format!("svrn-ingest index: unreadable result: {e}")),
        (false, _) => Err("svrn-ingest index failed (its reason is above)".to_string()),
    }
}

/// What `svrn-ingest index` reports for a full build: the fields of its JSON
/// result line this verb renders.
#[derive(Debug, serde::Deserialize)]
pub struct IndexBuilt {
    pub corpus_id: String,
    pub chunks_created: u64,
    pub index_size_bytes: u64,
    pub duration_secs: u64,
}

/// Ingest's CLI, which writes every code index (phase-b-30 Group 4).
const INGEST_BIN: &str = "svrn-ingest";

/// The embedder flags `svrn-ingest index` gets. Keyword-only, or the one
/// decider (`corpus_index::host`) asked for this node's configured embed model
/// when svrn's config names one, so code lands in the space knowledge corpora
/// use; with none named, the decider takes what the endpoint lists.
pub(crate) fn embedder_args(fts_only: bool) -> Result<Vec<String>, String> {
    if fts_only {
        return Ok(vec!["--fts-only".to_string()]);
    }
    Ok(match node_embed_model()? {
        Some(model) => vec!["--embed-model".to_string(), model],
        None => Vec::new(),
    })
}

/// This node's configured embed model id, if svrn's config names one. No
/// config file is `None` (a code-only developer); a config that exists and
/// does not load is refused, never read as "no model": that would put the
/// corpus in whatever space the endpoint lists first.
fn node_embed_model() -> Result<Option<String>, String> {
    use sovereign_contracts::setup_config::SetupConfig;
    if !SetupConfig::exists() {
        tracing::debug!("code index: no svrn config; the decider picks the embed model");
        return Ok(None);
    }
    Ok(SetupConfig::load()?.local_embed_model_id())
}

/// Where ingest's CLI is, or the refusal naming `what` needed it.
fn ingest_bin(what: &str) -> Result<PathBuf, String> {
    sovereign_turn_client::reach::locate_sibling(INGEST_BIN, "SOVEREIGN_INGEST_BIN").ok_or_else(
        || {
            format!(
                "{what} needs ingest's CLI, `{INGEST_BIN}`, which was not found beside this \
                 binary or on PATH. Build it with `cargo build -p sovereign-pipeline`, or set \
                 SOVEREIGN_INGEST_BIN to its path. (`symbols` and `callers` need only the SCIP \
                 graph, not this.)"
            )
        },
    )
}

/// Run `svrn-ingest <verb> <args>` with every stream the person's, and
/// return its exit code (`code finalize` and `code watch`, code F1 (a)).
pub(crate) async fn exec_ingest(verb: &str, args: &[String]) -> i32 {
    let bin = match ingest_bin(&format!("`code {verb}`")) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    tracing::debug!(bin = %bin.display(), verb, ?args, "code: exec svrn-ingest");
    let status = tokio::process::Command::new(&bin)
        .arg(verb)
        .args(args)
        .status()
        .await;
    tracing::debug!(verb, ?status, "code: svrn-ingest returned");
    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("error: cannot run {}: {e}", bin.display());
            1
        }
    }
}

/// Run `svrn-ingest index <args>`. Its stderr is the person's and passes
/// straight through; its last stdout line is the JSON result. Returns the exit
/// status with that result, because a file-list run that failed some files
/// still reports its counts.
async fn ingest_index(args: &[String]) -> Result<(bool, serde_json::Value), String> {
    let bin = ingest_bin("building a code index")?;
    tracing::debug!(bin = %bin.display(), ?args, "code index: exec svrn-ingest index");
    // `spawn` + `wait_with_output`, not `output()`: tokio's `output()` pipes
    // stderr unconditionally, which swallowed ingest's reasons (watched).
    let out = tokio::process::Command::new(&bin)
        .arg("index")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?
        .wait_with_output()
        .await
        .map_err(|e| format!("waiting on {}: {e}", bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let result = stdout
        .lines()
        .last()
        .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok());
    tracing::debug!(
        status = ?out.status,
        has_result = result.is_some(),
        "code index: svrn-ingest index returned"
    );
    match result {
        Some(v) => Ok((out.status.success(), v)),
        None if out.status.success() => Err(format!(
            "{} index exited 0 but printed no JSON result",
            bin.display()
        )),
        None => Err(format!(
            "{} index failed (its reason is above)",
            bin.display()
        )),
    }
}

/// The entries a code-corpus ingest creates. Everything else in a corpus
/// directory belongs to some other subsystem.
///
/// This list is the whole point of the module's clearing logic, and it is an
/// ALLOWLIST on purpose. It used to be a denylist — "delete everything that is
/// not `scip_graph.db*`" — which is not a property anyone can hold in their
/// head as the directory gains occupants. Measured on this host 2026-08-24, a
/// mature corpus directory also holds `_enrichment_state.json`,
/// `_raptor_checkpoint`, `raptor_summaries.lance`, `atlas/`,
/// `field_skeleton.json`, `triage-candidates.json`, `_doc_freshness.json`,
/// `code_intel_cache.json`, a whole second graph db (`wikipedia_graph.db`),
/// and in one case a hand-made `_corpus_meta.json.bak-predeup`. A rebuild
/// deleted all of it.
const INGEST_ARTIFACTS: &[&str] = &[
    // The corpus descriptor the ingest writes on finalise.
    CORPUS_META_FILENAME,
    // The table `create_empty_table` collides on. Removing the directory takes
    // its `_indices` / FTS / vector build scratch with it.
    "chunks.lance",
    // A top-level index dir from older layouts; harmless when absent.
    "_indices",
];

/// What a clear left behind. Reported, never silent.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Preserved {
    /// Entry names that were not the ingest's to delete, sorted.
    pub names: Vec<String>,
    /// True when the directory itself was removed because nothing survived.
    pub dir_removed: bool,
}

/// Remove the ingest's own artifacts from `dir`, preserving everything else.
///
/// # Why this is an allowlist
///
/// A code-index rebuild is a rebuild OF THE CHUNK TABLE. It is not a request
/// to empty the corpus directory, and the two were the same function until
/// 2026-08-24, when a branch switch changed 570 files, tripped the
/// "past the 500-file mark a rebuild is usually faster" heuristic in
/// [`run_incremental`], and destroyed a 7.8-hour code-intel enrichment pass
/// that had finished three hours earlier. Nothing warned, because deleting
/// data the caller never mentioned was the implementation's normal behaviour.
///
/// A speed heuristic must never be able to choose a destructive path. After
/// this change the heuristic is free to pick whichever route is faster,
/// because both routes cost the same thing: re-embedding chunks.
///
/// If the caller genuinely wants the directory gone, that verb already exists
/// and is explicit — `svrn corpus remove <id>` (ARCH §19: the inventory
/// outranks the plan).
///
/// # The empty-directory rule, and why it is computed rather than tracked
///
/// `finalise_solo_ingest` promotes `<corpus>-partition-<node>/` to canonical
/// `<corpus>/` by rename, and that rename is SKIPPED when the canonical path
/// already exists — even if empty. So a directory with nothing left in it must
/// go, or the fresh ingest is stranded in the partition path. That is decided
/// by re-reading the directory afterwards, not by a flag set during the walk:
/// a flag has to be updated every time the allowlist changes, and this does
/// not.
pub(crate) fn clear_ingest_artifacts(dir: &std::path::Path) -> std::io::Result<Preserved> {
    let mut preserved = Preserved::default();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if INGEST_ARTIFACTS.contains(&name_str.as_str()) {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        } else {
            preserved.names.push(name_str);
        }
    }
    preserved.names.sort();
    if std::fs::read_dir(dir)?.next().is_none() {
        // Swallow the error — a racing observer could have created a file
        // between the read and this rmdir. The next ingest step creates a
        // fresh partition either way.
        preserved.dir_removed = std::fs::remove_dir(dir).is_ok();
    }
    Ok(preserved)
}

/// Clear the ingest's artifacts from every `<corpus_id>-partition-*` directory
/// under `root`, so a stale partition-of-self / partition-of-peer does not
/// collide with the fresh ingest's `create_empty_table` call.
///
/// This used to `remove_dir_all` the whole partition. That is what actually
/// destroyed the code-intel enrichment on 2026-08-24: for a SCIP-indexed code
/// corpus the canonical directory holds `scip_graph.db`, which kept it alive,
/// which meant `finalise_solo_ingest` never promoted — so the corpus's chunks,
/// and the enrichment rows written alongside them, lived in
/// `<corpus>-partition-local/` permanently. The "transient shard" the old code
/// believed it was deleting was the corpus.
///
/// Non-partition siblings (other corpora, arbitrary files) are untouched. A
/// missing `root` is not an error — a first-ever rebuild on a machine with no
/// indexes is a normal state.
pub(crate) fn clear_partitions_for(
    root: &std::path::Path,
    corpus_id: &str,
) -> std::io::Result<Preserved> {
    // An empty or whitespace-only id names no corpus, so it sweeps nothing —
    // refused rather than normalised into a prefix that would match every
    // partition under `root` (ARCH §18.3).
    let Some(corpus) = Corpus::named(root, corpus_id) else {
        return Ok(Preserved::default());
    };
    let prefix = corpus.partition_prefix();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Preserved::default()),
        Err(e) => return Err(e),
    };
    let mut all = Preserved::default();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with(&prefix) || !entry.path().is_dir() {
            continue;
        }
        let kept = clear_ingest_artifacts(&entry.path())?;
        for n in kept.names {
            all.names.push(format!("{name_str}/{n}"));
        }
    }
    all.names.sort();
    Ok(all)
}

/// The in-process embedder `code watch` and `check-spec` use: the one
/// decider, `corpus_index::host` (the discovery ladder, then a probe
/// embedding), asked for this node's configured embed model. `build_daemon_
/// embed_fn`, which probed only the daemon, went with pb-code-index.
pub async fn node_embedder() -> std::result::Result<(EmbedFn, String), String> {
    let profile = host::discover_and_probe(None, node_embed_model()?)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok((profile.embed_document_fn(), profile.embed_model))
}

#[cfg(test)]
mod clearing_tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// Build a corpus directory holding what a real one holds. The occupant
    /// list is not invented — it is `ls -A` over this host's `sep`,
    /// `wikipedia`, `conversations-anthropic` and `commonwealth-ai` corpora
    /// on 2026-08-24.
    fn populated_corpus(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        // The ingest's own.
        fs::write(Corpus::meta_in(dir), "{}").unwrap();
        fs::create_dir_all(dir.join("chunks.lance/data")).unwrap();
        fs::write(dir.join("chunks.lance/data/0.lance"), "x").unwrap();
        // Everybody else's.
        fs::write(dir.join("code_intel_cache.json"), "{}").unwrap();
        fs::write(dir.join("_enrichment_state.json"), "{}").unwrap();
        fs::write(dir.join("raptor_summaries.meta.json"), "{}").unwrap();
        fs::create_dir_all(dir.join("raptor_summaries.lance")).unwrap();
        fs::create_dir_all(dir.join("atlas")).unwrap();
        fs::write(dir.join("atlas/atoms.jsonl"), "{}").unwrap();
        fs::write(dir.join("field_skeleton.json"), "{}").unwrap();
        fs::write(dir.join("triage-candidates.json"), "[]").unwrap();
        fs::write(dir.join("_corpus_meta.json.bak-predeup"), "{}").unwrap();
        fs::write(dir.join("scip_graph.db"), "x").unwrap();
        fs::write(dir.join(".rebuild.lock"), "").unwrap();
    }

    /// THE REGRESSION. On 2026-08-24 a branch switch changed 570 files, tripped
    /// the "past the 500-file mark a rebuild is usually faster" heuristic, and
    /// the resulting rebuild deleted `code_intel_cache.json` — 19,855 symbol
    /// summaries, 7.8 hours of local inference, finished three hours earlier.
    /// The pass was regenerable; nothing warned, and that is the part this
    /// pins. Switching branches must cost a re-index, never the enrichment.
    #[test]
    fn a_rebuild_does_not_delete_the_code_intel_enrichment() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("commonwealth-ai");
        populated_corpus(&dir);

        let kept = clear_ingest_artifacts(&dir).unwrap();

        assert!(
            dir.join("code_intel_cache.json").exists(),
            "the 7.8-hour cache must survive a chunk-table rebuild"
        );
        assert!(kept.names.contains(&"code_intel_cache.json".to_string()));
        assert!(!kept.dir_removed);
    }

    /// The ingest's artifacts go, and nothing else does. Stated as the full
    /// partition of the directory so a new occupant cannot be quietly added to
    /// the wrong side.
    #[test]
    fn only_the_ingests_own_artifacts_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("corpus");
        populated_corpus(&dir);

        let kept = clear_ingest_artifacts(&dir).unwrap();

        for gone in [CORPUS_META_FILENAME, "chunks.lance"] {
            assert!(!dir.join(gone).exists(), "{gone} must be cleared");
        }
        let expected = [
            ".rebuild.lock",
            "_corpus_meta.json.bak-predeup",
            "_enrichment_state.json",
            "atlas",
            "code_intel_cache.json",
            "field_skeleton.json",
            "raptor_summaries.lance",
            "raptor_summaries.meta.json",
            "scip_graph.db",
            "triage-candidates.json",
        ];
        assert_eq!(
            kept.names,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        for survivor in expected {
            assert!(dir.join(survivor).exists(), "{survivor} must survive");
        }
        // The nested file proves the directory was preserved whole, not
        // emptied and left as a shell.
        assert!(dir.join("atlas/atoms.jsonl").exists());
    }

    /// A second graph db in the same directory used to be deleted because the
    /// exemption was spelled `scip_graph.db` and nothing else. `wikipedia`
    /// carries `wikipedia_graph.db` beside its chunks.
    #[test]
    fn a_sibling_graph_db_that_is_not_scip_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wikipedia");
        fs::create_dir_all(&dir).unwrap();
        fs::write(Corpus::meta_in(&dir), "{}").unwrap();
        fs::create_dir_all(dir.join("chunks.lance")).unwrap();
        for f in ["wikipedia_graph.db", "wikipedia_graph.db-wal"] {
            fs::write(dir.join(f), "x").unwrap();
        }

        clear_ingest_artifacts(&dir).unwrap();

        for f in ["wikipedia_graph.db", "wikipedia_graph.db-wal"] {
            assert!(dir.join(f).exists(), "{f} must survive");
        }
    }

    /// The promotion rule, both ways. `finalise_solo_ingest` renames
    /// `<corpus>-partition-<node>/` to canonical `<corpus>/` and SKIPS the
    /// rename when the canonical path exists — even empty. So a directory with
    /// nothing left must go, or the fresh ingest is stranded in the partition.
    #[test]
    fn the_directory_goes_only_when_nothing_survived() {
        let tmp = tempfile::tempdir().unwrap();

        let bare = tmp.path().join("bare");
        fs::create_dir_all(bare.join("chunks.lance")).unwrap();
        fs::write(Corpus::meta_in(&bare), "{}").unwrap();
        let kept = clear_ingest_artifacts(&bare).unwrap();
        assert!(kept.names.is_empty());
        assert!(
            kept.dir_removed,
            "an emptied directory must not block the rename"
        );
        assert!(!bare.exists());

        let occupied = tmp.path().join("occupied");
        fs::create_dir_all(occupied.join("chunks.lance")).unwrap();
        fs::write(occupied.join("code_intel_cache.json"), "{}").unwrap();
        let kept = clear_ingest_artifacts(&occupied).unwrap();
        assert!(!kept.dir_removed);
        assert!(occupied.exists());
    }

    /// The partition path is the one that actually did the damage. For a
    /// SCIP-indexed code corpus the canonical directory holds `scip_graph.db`,
    /// so it never empties, so `finalise_solo_ingest` never promotes — and the
    /// corpus lives in `<id>-partition-local/` permanently. `remove_dir_all` on
    /// that is not "clearing a stale shard", it is deleting the corpus.
    #[test]
    fn a_partition_holding_the_corpus_is_cleared_not_nuked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let part = root.join("commonwealth-ai-partition-local");
        populated_corpus(&part);
        // An unrelated corpus and a same-prefix non-partition must be untouched.
        fs::create_dir_all(root.join("commonwealth")).unwrap();
        fs::write(root.join("commonwealth/_corpus_meta.json"), "{}").unwrap();
        fs::create_dir_all(root.join("commonwealth-ai")).unwrap();
        fs::write(root.join("commonwealth-ai/scip_graph.db"), "x").unwrap();

        let kept = clear_partitions_for(root, "commonwealth-ai").unwrap();

        assert!(part.exists(), "the partition directory must survive");
        assert!(
            !part.join("chunks.lance").exists(),
            "its chunk table is cleared"
        );
        assert!(part.join("code_intel_cache.json").exists());
        assert!(part.join("atlas/atoms.jsonl").exists());
        assert!(
            kept.names
                .iter()
                .any(|n| n.ends_with("/code_intel_cache.json")),
            "preserved names are qualified by partition: {:?}",
            kept.names
        );
        // Blast radius: neither sibling was in scope.
        assert!(root.join("commonwealth/_corpus_meta.json").exists());
        assert!(root.join("commonwealth-ai/scip_graph.db").exists());
    }

    /// A partition that held only ingest output is removed outright — that is
    /// the case the old `remove_dir_all` was written for, and it still works.
    #[test]
    fn a_partition_holding_only_ingest_output_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let part = root.join("c-partition-peer");
        fs::create_dir_all(part.join("chunks.lance")).unwrap();
        fs::write(Corpus::meta_in(&part), "{}").unwrap();

        let kept = clear_partitions_for(root, "c").unwrap();

        assert!(!part.exists(), "a stale peer shard must not linger");
        assert!(kept.names.is_empty());
    }

    /// A missing indexes root is a normal first-run state, not an error.
    #[test]
    fn a_missing_root_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let kept = clear_partitions_for(&tmp.path().join("nope"), "c").unwrap();
        assert_eq!(kept, Preserved::default());
    }
}
