// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code index` — build or refresh a repository's chunk corpus.
//!
//! # One implementation, two binaries
//!
//! This verb ships in `sovereign-cli` (the dispatcher) AND runs in
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
//! The merge below keeps the correct half of each. Lives here because
//! `sovereign-cli-shared` is the only crate both binaries already depend on;
//! gated behind `code-index` so binaries that do not serve the verb pay nothing
//! for `corpus-engine` and `oicp-client` (both of which the two that DO serve it
//! already link).
//!
//! # This does not run a model
//!
//! Embeddings come from the daemon over loopback HTTP: `build_daemon_embed_fn`
//! probes `/v1/models` and refuses up front if nothing answers, then talks to
//! `/embeddings` through `oicp-client` — a pure `reqwest` client with no
//! llama.cpp. The daemon owns the one inference stack in the system and this
//! module must never acquire a second one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::{Corpus, CorpusEngine, CorpusSpec, EmbedFn, CORPUS_META_FILENAME};
use oicp_client::RemoteApiProvider;
use sovereign_core::traits::InferenceProvider;

use crate::dirs::default_data_dir;
use crate::help::{Help, HelpSection};

/// Help for `svrn code index` specifically. The workbench's `code` help
/// covers a dozen subcommands that do not ship here; advertising them from
/// this binary would be the same defect this port exists to fix.
pub const HELP: Help = Help {
    command: "svrn code index",
    summary: "Index a repository into a searchable code corpus.",
    sections: &[HelpSection::Usage(
        "svrn code index <path> [--corpus-id <id>] [--data-dir <dir>]\n\
         svrn code index <path> --full          (re-embed everything)\n\
         svrn code index <path> --incremental   (force the changed-files path)",
    )],
};

pub async fn cmd_index(args: &[String]) -> i32 {
    // Handle `--help` BEFORE the flag loop. The loop's catch-all treats any
    // unknown `-` flag as a warning and falls through, so `code index --help`
    // printed "unknown flag '--help' — ignored", then "error: missing <path>",
    // then the help text, and exited 1. Harmless while the verb was
    // workbench-only; a shipped verb whose `--help` exits non-zero is a defect.
    if crate::help::wants_help(args) {
        crate::help::print(&HELP);
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
        crate::help::print(&HELP);
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
pub async fn rebuild_code_corpus(
    root: &std::path::Path,
    corpus_id: &str,
    data_dir: &std::path::Path,
) -> std::result::Result<corpus_engine::IngestResult, String> {
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

pub fn tempfile_dir() -> std::io::Result<PathBuf> {
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
pub async fn build_daemon_embed_fn() -> std::result::Result<(EmbedFn, String), String> {
    let cfg = sovereign_core::setup_config::SetupConfig::load()
        .map_err(|e| format!("read ~/.svrnmesh/config.toml: {e}"))?;
    let port = cfg.daemon.client_port;
    let endpoint = format!("http://localhost:{port}/v1");
    // The embedding happens on the DAEMON at `endpoint`, over loopback — so
    // the question is not "does this process hold a GGUF" but "can the daemon
    // this is about to call name the space its vectors land in". A terminal's
    // daemon can: it forwards to its entry node, and `local_embed_model_id()`
    // is that node's recorded id. Gating on `models()` instead made `svrn code
    // index` refuse on a terminal for a capability the node actually has.
    //
    // Still no default. This name decides which vector space the corpus lands
    // in, so a space that cannot be named is a refusal, never a fallback —
    // that is the split `sovereign-cli-shared::models` documents, and this is
    // the side that refuses (§18.3).
    let embed_model = cfg.local_embed_model_id().ok_or_else(|| {
        "this node cannot name its embedding model — a holder needs \
         `[models] embed` in ~/.svrnmesh/config.toml, and a terminal needs its \
         entry node to declare an embed slot (re-run `svrn setup --terminal \
         <entry>` once it has one). Indexing under an unnamed space would \
         produce a corpus that cannot be searched back."
            .to_string()
    })?;

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
