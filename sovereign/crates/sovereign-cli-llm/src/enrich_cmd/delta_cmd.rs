// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich delta <corpus-id> --chapters <ids>` — incremental
//! atoms-delta merge into an EXISTING referential atlas.
//!
//! The referential/LLM analogue of the structural `apply_incremental`
//! (`sovereign-mesh/src/newsworthy_host.rs`). It enriches ONLY the
//! `--chapters` subset, resolves the result into a throwaway staging
//! dir, content-hashes the staged atoms with the real corpus_id, and
//! merges them into the live atlas via the additive `apply_atom_delta`
//! primitive. The 24h-to-build atlas is never rebuilt or overwritten —
//! it is mutated exactly once, additively, after a backup.
//!
//! ## Flow (see `~/.claude/plans/iterative-giggling-hopcroft.md`)
//!
//!   1. Pre-flight the live atlas. If its atoms carry sequential ids
//!      (`entity-0001`), `apply_atom_delta` would orphan them against
//!      the content-hash staged atoms — so migrate first (`--yes`) or
//!      abort with a pointer to `svrn atlas migrate-ids`.
//!   2. Back up `atoms.json` / `edges.json` / `doc_to_atoms.json`
//!      (+ `cross_corpus_edges.json` when present) into
//!      `<atlas>/.delta-backup-<suffix>/`.
//!   3. Subset extract → cluster → name on the `--chapters`, reusing
//!      `build::build_with_progress` with seed/resolve/tensions/gaps/
//!      configure/report skipped. This promotes the subset Phase-1
//!      into `cache/questions.json`.
//!   4. Resolve the subset sketches into a STAGING tempdir via the
//!      extracted `atlas_resolve::resolve_into_dir`.
//!   5. `migrate_atlas_ids(staging, REAL corpus_id)` so staged atoms
//!      get byte-identical content-hash ids a full migration would.
//!   6. Read staging atoms + edges, build an additive `AtomsDelta`,
//!      `apply_atom_delta(live_atlas, delta)`.
//!   7. Partial meta-atlas rebuild (`rebuild_for_corpus`),
//!      best-effort.
//!   8. Clean up the staging tempdir unless `--keep-staging`.
//!
//! Sibling: `svrn enrich delta-manifest <corpus-id>
//! --source-prefix <p>` mints `sec_NNNNN` ids for newly-appended
//! chunks (whose `source_doc_id` starts with `<p>` and aren't yet in
//! `chapters.json`) and appends them to the manifest, so a subsequent
//! `enrich delta --chapters <new ids>` can enrich exactly those.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms_delta::{apply_atom_delta, AtomsDelta};
use corpus_engine::enrichment::atlas::migrate_ids::migrate_atlas_ids;
use corpus_engine::enrichment::atlas::{read_atlas_atoms, read_atlas_edges, ATLAS_DIRNAME};
use corpus_engine::enrichment::pipeline::{Phase1Output, PipelinePhase};
use corpus_engine::{CorpusEngine, EmbedFn};

use super::atlas_resolve::{collect_section_extractions, resolve_into_dir, ResolvePhase};
use super::build::{self, ParsedBuild};
use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich delta",
    summary: "Incrementally enrich a chapter subset and merge the atoms into the existing atlas.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich delta <corpus-id> --chapters <sec_ids> \\\n  [--phase 3a|all] [--yes] [--dry-run] [--keep-staging]",
        ),
        HelpSection::Flags(&[
            (
                "--chapters <ids>",
                "REQUIRED. Comma-separated chapter ids to enrich \
                 (e.g. sec_00321,sec_00322). Typically the new ids \
                 printed by `enrich delta-manifest`.",
            ),
            (
                "--phase 3a|all",
                "Resolution depth for the subset. `3a` = entities + \
                 events + Involves edges only; `all` (default) = the \
                 full structural pass (states / relations / claims / \
                 questions / positions + typed extensions).",
            ),
            (
                "--yes",
                "If the live atlas still has sequential ids, migrate it \
                 to content-hash ids in place (idempotent) before \
                 merging. Without this the command aborts and points \
                 you at `svrn atlas migrate-ids`.",
            ),
            (
                "--dry-run",
                "Run the subset extract → cluster → name (promoting the \
                 subset Phase-1 to cache) and stop BEFORE resolving / \
                 merging. Use to inspect the subset sketches first.",
            ),
            (
                "--keep-staging",
                "Leave the staging atlas tempdir on disk for inspection \
                 instead of deleting it after the merge.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich delta enron-sample-multi-wide --chapters sec_00321,sec_00322",
                "Enrich two newly-appended chapters and merge their atoms into the live atlas.",
            ),
            (
                "svrn enrich delta bk --chapters sec_00010 --phase 3a --keep-staging",
                "3a-only delta, keeping the staging dir to inspect the resolved atoms.",
            ),
        ]),
        HelpSection::Notes(
            "Additive only: the live atlas is backed up to \
             <atlas>/.delta-backup-<suffix>/ and mutated once via \
             apply_atom_delta — never rebuilt. Requires a prior \
             `svrn enrich init` + that the `--chapters` exist in \
             chapters.json (see `enrich delta-manifest` to mint ids \
             for freshly-appended chunks). Requires the daemon for the \
             extract/name LLM phases.",
        ),
    ],
};

pub async fn cmd_delta(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    let cfg = match EnrichConfig::require(&parsed.corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading enrichment config: {e}");
            return 1;
        }
    };

    // Atlas-shaped pipeline gate — identical to `atlas_resolve`. A
    // legacy `literary` cache carries no section_extraction payloads,
    // so the subset resolve would produce an empty staging atlas.
    if !cfg.pipeline_id.ends_with("_atlas") {
        eprintln!(
            "error: pipeline `{}` does not produce atlas sketches. Re-init with \
             --pipeline literary_atlas (or another *_atlas pipeline) before running \
             a delta.",
            cfg.pipeline_id
        );
        return 1;
    }

    let real_atlas_dir = paths::index_root(&cfg.corpus_id).join(ATLAS_DIRNAME);

    // ── Step 1: pre-flight the live atlas ──────────────────────
    // A live atlas with sequential ids mixes badly with the
    // content-hash staged atoms (apply_atom_delta would leave the
    // legacy atoms orphaned). Mirror the newsworthy_host check.
    match read_atlas_atoms(&real_atlas_dir) {
        Ok(atoms_file) => {
            let needs_migration = !atoms_file.atoms.is_empty()
                && !atoms_file
                    .atoms
                    .iter()
                    .all(|env| env.id().is_content_hash());
            if needs_migration {
                if parsed.yes {
                    println!(
                        "  · live atlas has sequential ids — migrating to content-hash \
                         (idempotent) before merge ..."
                    );
                    match migrate_atlas_ids(&real_atlas_dir, &cfg.corpus_id, false) {
                        Ok(s) => {
                            println!(
                                "  ✓ migrated {} atom(s) ({} already content-hash, {} deduped)",
                                s.atoms_migrated, s.atoms_already_content_hash, s.atoms_deduped
                            );
                            if !s.collisions_detected.is_empty() {
                                println!(
                                    "  ! {} id collision(s) collapsed during migration",
                                    s.collisions_detected.len()
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("error: migrating live atlas ids: {e}");
                            return 1;
                        }
                    }
                } else {
                    eprintln!(
                        "error: live atlas at {} still has sequential ids. Merging \
                         content-hash delta atoms into it would orphan the legacy \
                         atoms.\n  Run `svrn atlas migrate-ids --corpus {}` first, \
                         or re-run this command with --yes to migrate in place.",
                        real_atlas_dir.display(),
                        cfg.corpus_id
                    );
                    return 1;
                }
            }
        }
        Err(e) => {
            // No live atlas to merge into. A delta presupposes an
            // existing atlas (that's the whole point — don't rebuild).
            eprintln!(
                "error: reading live atlas atoms.json at {}: {e}\n  A delta merges into \
                 an EXISTING atlas. Run `svrn enrich build {}` to create one first.",
                real_atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    }

    // Stable suffix derived from the (sorted) chapter ids — NOT a
    // timestamp (Date::now is unavailable in this crate, and a stable
    // suffix makes re-running the same delta reuse one backup slot
    // rather than littering the atlas dir).
    let suffix = chapters_suffix(&parsed.chapters);

    // ── Step 2: back up the mutated files ──────────────────────
    let backup_dir = real_atlas_dir.join(format!(".delta-backup-{suffix}"));
    if let Err(e) = backup_atlas_files(&real_atlas_dir, &backup_dir) {
        eprintln!(
            "error: backing up atlas files to {}: {e}",
            backup_dir.display()
        );
        return 1;
    }
    println!("  ✓ backed up atlas files → {}", backup_dir.display());

    // ── Step 3: subset extract → cluster → name ────────────────
    // Skip everything except extract/cluster/name. A subset run is
    // promoted into cache/questions.json by `build` so we can read it
    // back. `Selection::Chapters` bypasses build's idempotency gate.
    if parsed.skip_build {
        println!(
            "  · --skip-build: skipping subset extract; resolving existing \
             cache/questions.json (promote a run file there first)."
        );
    } else {
        let skip_list: Vec<String> = ["seed", "resolve", "tensions", "gaps", "configure", "report"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed_build = match ParsedBuild::from_inputs(
            cfg.corpus_id.clone(),
            Some(parsed.chapters.clone()),
            &skip_list,
            parsed.dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: constructing subset build: {e}");
                return 1;
            }
        };
        println!(
            "  · subset extract → cluster → name on {} chapter(s) ...",
            parsed.chapters.len()
        );
        let build_code = build::build_with_progress(&parsed_build, None).await;
        if build_code != 0 {
            eprintln!("error: subset build failed (exit {build_code}); not merging.");
            return build_code;
        }
    }

    if parsed.dry_run {
        println!();
        println!(
            "  dry-run: subset Phase 1 promoted to {}. Stopping before resolve/merge.",
            paths::cache_dir(&cfg.corpus_id)
                .join("questions.json")
                .display()
        );
        return 0;
    }

    // ── Step 4: load the subset sketches ───────────────────────
    let cache = cfg.phase_cache();
    let phase1: Phase1Output = match cache.read(PipelinePhase::Questions) {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!(
                "error: no Phase 1 cache at {} after subset build — the subset extract \
                 produced no promotable output.",
                paths::cache_dir(&cfg.corpus_id).display()
            );
            return 1;
        }
        Err(e) => {
            eprintln!("error: reading subset Phase 1 cache: {e}");
            return 1;
        }
    };
    let sections = collect_section_extractions(&phase1.questions_by_chapter);
    if sections.is_empty() {
        eprintln!(
            "error: subset Phase 1 cache contains no `section_extraction` payloads — \
             nothing to resolve into a delta."
        );
        return 1;
    }
    println!(
        "  ✓ loaded {} section sketch(es) from the subset cache",
        sections.len()
    );

    // ── Step 4 (cont.): build the embed closure ────────────────
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, _chat, _chat_with_tokens) = client.into_closures_with_tokens();

    // ── Step 4 (cont.): resolve into a STAGING dir ─────────────
    let staging_root = std::env::temp_dir().join(format!("sov-delta-{}-{suffix}", cfg.corpus_id));
    let staging_atlas_dir = staging_root.join("atlas");
    if let Err(e) = std::fs::create_dir_all(&staging_atlas_dir) {
        eprintln!(
            "error: creating staging dir {}: {e}",
            staging_atlas_dir.display()
        );
        return 1;
    }
    println!(
        "  · resolving subset into staging {} ...",
        staging_atlas_dir.display()
    );
    if let Err(e) =
        resolve_into_dir(&cfg, &sections, &embed, &staging_atlas_dir, parsed.phase).await
    {
        eprintln!("error: resolving subset into staging atlas: {e}");
        cleanup_staging(&staging_root, parsed.keep_staging);
        return 1;
    }

    // ── Step 5: content-hash the staging atoms ─────────────────
    // Uses the REAL corpus_id so staged atoms get the canonical
    // content-hash ids a full migration would assign — that's what
    // makes apply_atom_delta's content-hash dedup line up against the
    // live atlas.
    match migrate_atlas_ids(&staging_atlas_dir, &cfg.corpus_id, false) {
        Ok(s) => {
            println!(
                "  ✓ content-hashed staging atoms ({} migrated, {} already content-hash)",
                s.atoms_migrated, s.atoms_already_content_hash
            );
        }
        Err(e) => {
            eprintln!("error: content-hashing staging atoms: {e}");
            cleanup_staging(&staging_root, parsed.keep_staging);
            return 1;
        }
    }

    // ── Step 6: build + apply the delta ────────────────────────
    let staged_atoms = match read_atlas_atoms(&staging_atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: reading staging atoms.json: {e}");
            cleanup_staging(&staging_root, parsed.keep_staging);
            return 1;
        }
    };
    // edges.json may legitimately be absent on a 3a-only run with no
    // Involves edges; treat a missing file as "no edges".
    let staged_edges = match read_atlas_edges(&staging_atlas_dir) {
        Ok(e) => e.edges,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!("error: reading staging edges.json: {e}");
            cleanup_staging(&staging_root, parsed.keep_staging);
            return 1;
        }
    };

    // Capture the staged atom count before the vec is moved into the
    // delta — `atoms_added` (newly-appended) plus this minus that
    // gives the replaced-in-place count for the summary line.
    let staged_atom_count = staged_atoms.atoms.len();
    let delta = AtomsDelta {
        added: staged_atoms.atoms,
        added_edges: staged_edges,
        // Net-new chapters have no prior atoms to remove/upsert.
        removed_doc_ids: vec![],
        upserted_docs: vec![],
    };
    let summary = match apply_atom_delta(&real_atlas_dir, delta) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: applying delta to {}: {e}\n  The live atlas is unchanged \
                 except as reported above; backups are at {}.",
                real_atlas_dir.display(),
                backup_dir.display()
            );
            cleanup_staging(&staging_root, parsed.keep_staging);
            return 1;
        }
    };
    println!();
    println!("  ✓ delta applied to {}", real_atlas_dir.display());
    // replaced-in-place = staged atoms that collided with an existing
    // live content-hash id (apply_atom_delta overwrites those rather
    // than appending, so they're in `staged_atom_count` but not in
    // `atoms_added`).
    let replaced_in_place = staged_atom_count.saturating_sub(summary.atoms_added);
    println!(
        "      atoms: {} → {} (+{} new, {} replaced-in-place of {} staged)",
        summary.atoms_before,
        summary.atoms_after,
        summary.atoms_added,
        replaced_in_place,
        staged_atom_count,
    );
    println!(
        "      edges dropped: {} · cross-corpus edges dropped: {}",
        summary.edges_dropped, summary.cross_corpus_edges_dropped
    );
    println!("      files touched: {}", summary.files_touched.join(", "));

    // ── Step 7: partial meta-atlas rebuild (best-effort) ───────
    let indexes_dir = resolve_indexes_dir();
    match corpus_engine::meta_atlas::rebuild_for_corpus(&indexes_dir, &cfg.corpus_id, None) {
        Ok(_) => println!("  ✓ meta-atlas anchors refreshed for {}", cfg.corpus_id),
        Err(e) => eprintln!(
            "  warning: meta-atlas partial rebuild failed: {e} — anchors may lag until \
             the next full build."
        ),
    }

    // ── Step 8: clean up staging ───────────────────────────────
    cleanup_staging(&staging_root, parsed.keep_staging);
    if parsed.keep_staging {
        println!("  · kept staging dir {}", staging_root.display());
    }

    0
}

/// Copy `atoms.json` / `edges.json` / `doc_to_atoms.json`
/// (+ `cross_corpus_edges.json` when present) from `atlas_dir` into
/// `backup_dir`. Files that don't exist are skipped silently — a
/// 3a-only atlas has no trajectories, a never-grounded atlas has no
/// cross_corpus_edges.
fn backup_atlas_files(atlas_dir: &Path, backup_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(backup_dir)?;
    for name in [
        "atoms.json",
        "edges.json",
        "doc_to_atoms.json",
        "cross_corpus_edges.json",
    ] {
        let src = atlas_dir.join(name);
        if src.exists() {
            std::fs::copy(&src, backup_dir.join(name))?;
        }
    }
    Ok(())
}

/// Delete the staging tempdir unless the operator asked to keep it.
/// Best-effort: a failed cleanup is a warning, never fatal — the
/// merge already succeeded by the time this runs.
fn cleanup_staging(staging_root: &Path, keep: bool) {
    if keep {
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(staging_root) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "  warning: could not remove staging dir {}: {e}",
                staging_root.display()
            );
        }
    }
}

/// Short, stable hex suffix derived from the sorted chapter id set.
/// Re-running the same delta reuses one backup/staging slot rather
/// than accreting timestamped debris.
fn chapters_suffix(chapters: &[String]) -> String {
    let mut sorted: Vec<&String> = chapters.iter().collect();
    sorted.sort();
    let mut hasher = DefaultHasher::new();
    for c in sorted {
        c.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Resolve `~/.sovereign/indexes` (or the configured data dir) the
/// same way `enrich init`'s from-corpus path does.
fn resolve_indexes_dir() -> PathBuf {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
        .join("indexes")
}

// ── delta-manifest sibling ───────────────────────────────────────

const MANIFEST_HELP: Help = Help {
    command: "svrn enrich delta-manifest",
    summary: "Mint sec_NNNNN ids for freshly-appended chunks and append them to chapters.json.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich delta-manifest <corpus-id> [--source-prefix <prefix>] [--dry-run]",
        ),
        HelpSection::Flags(&[
            (
                "--source-prefix <p>",
                "OPTIONAL. New chunks are identified as those not covered \
                 by any existing chapter (an `expand_corpus` appends slices \
                 at higher ids). When given, additionally keep only chunks \
                 whose `metadata_raw` contains this string (e.g. `symes-k` \
                 to scope to one trader's staging slice; source_doc_id is \
                 the bare javamail message-id and carries no slice info).",
            ),
            (
                "--dry-run",
                "Report how many new chapters WOULD be appended (and \
                 their ids) without writing chapters.json.",
            ),
        ]),
        HelpSection::Examples(&[(
            "svrn enrich delta-manifest enron-sample-multi-wide --source-prefix symes-k",
            "After `corpus expand` appended symes-k mailbox chunks, mint chapter ids for them.",
        )]),
        HelpSection::Notes(
            "New chapters continue the `sec_NNNNN` numbering past the \
             existing manifest length. Feed the printed ids into \
             `svrn enrich delta <corpus> --chapters <ids>`. The \
             corpus must already be indexed (this reads its LanceDB \
             chunks).",
        ),
    ],
};

pub async fn cmd_delta_manifest(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&MANIFEST_HELP);
        return 0;
    }

    let parsed = match parse_manifest_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&MANIFEST_HELP);
            return 2;
        }
    };

    // The manifest helper drives off the already-indexed corpus
    // (identity is the dir under ~/.sovereign/indexes/<id>), so it
    // doesn't require an enrichment config — but it does require the
    // index + an existing chapters.json to extend.
    let manifest_path = paths::chapters_manifest_path(&parsed.corpus_id);
    let existing = match corpus_engine::enrichment::pipeline::ChapterManifest::load(&manifest_path)
    {
        Ok(Some(m)) => m,
        Ok(None) => {
            eprintln!(
                "error: no chapter manifest at {} — run `svrn enrich init {} \
                 --from-corpus <id>` first to create one.",
                manifest_path.display(),
                parsed.corpus_id
            );
            return 1;
        }
        Err(e) => {
            eprintln!("error: loading chapter manifest: {e}");
            return 1;
        }
    };

    // Set of chunk_ids already covered by the manifest — the new-row
    // filter excludes anything already mapped to a chapter.
    let covered_ids: std::collections::HashSet<u64> = existing
        .chapters
        .iter()
        .flat_map(|c| c.chunk_ids.iter().copied())
        .collect();

    // Stream every chunk row from the corpus index (same path as
    // `enrich init --from-corpus`).
    let indexes_dir = resolve_indexes_dir();
    let data_dir = indexes_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| indexes_dir.clone());
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = CorpusEngine::new(data_dir.join("recipes"), indexes_dir.clone(), noop_embed);
    let index = match engine.open_index_for_corpus(&parsed.corpus_id).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "error: could not open index for corpus `{}`: {e}",
                parsed.corpus_id
            );
            return 1;
        }
    };
    let rows = match index.all_chunks_full().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: streaming chunk rows: {e}");
            return 1;
        }
    };

    // Keep only rows that (a) match the source prefix and (b) are not
    // already covered by an existing chapter.
    // New chunks are exactly the rows no existing chapter covers: the
    // live atlas was chaptered over the original chunk set, and an
    // `expand_corpus` appends new slices at higher ids that no chapter
    // references yet. An optional `--source-prefix` further narrows by
    // matching the chunk's `metadata_raw` (the email JSON carries the
    // staging-slice path, e.g. `symes-k_..._link`) — NOT source_doc_id,
    // which is the bare javamail message-id with no slice info.
    let new_rows: Vec<corpus_engine::EnrichmentChunkRow> = rows
        .into_iter()
        .filter(|r| {
            if covered_ids.contains(&r.id) {
                return false;
            }
            match parsed.source_prefix.as_deref() {
                Some(p) => r
                    .metadata_raw
                    .as_deref()
                    .map(|m| m.contains(p))
                    .unwrap_or(false),
                None => true,
            }
        })
        .collect();

    let filter_desc = match parsed.source_prefix.as_deref() {
        Some(p) => format!("uncovered + metadata~`{p}`"),
        None => "uncovered by any chapter".to_string(),
    };
    if new_rows.is_empty() {
        println!(
            "  · no new chunks ({filter_desc}) — all matching rows are already in chapters.json, \
             or none matched."
        );
        return 0;
    }
    println!(
        "  · {} new chunk row(s) ({filter_desc}) not yet in the manifest.",
        new_rows.len()
    );

    // Bucket the new rows into chapters, continuing the ordinal past
    // the existing manifest length.
    let start_ordinal = (existing.len() as u32).saturating_add(1);
    let new_manifest = match super::init::build_manifest_from_corpus_rows(
        &parsed.corpus_id,
        new_rows,
        None,
        None,
        start_ordinal,
    ) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: building chapter manifest for new chunks: {e}");
            return 1;
        }
    };

    if new_manifest.is_empty() {
        println!("  · the new chunks carried no section metadata — no chapters minted.");
        return 0;
    }

    let new_ids: Vec<String> = new_manifest.chapters.iter().map(|c| c.id.clone()).collect();

    if parsed.dry_run {
        println!();
        println!(
            "  dry-run: would append {} chapter(s): {}",
            new_ids.len(),
            new_ids.join(",")
        );
        return 0;
    }

    // Append the new chapters and save.
    let mut merged = existing;
    merged.chapters.extend(new_manifest.chapters);
    if let Err(e) = merged.save(&manifest_path) {
        eprintln!("error: saving updated chapter manifest: {e}");
        return 1;
    }
    println!();
    println!(
        "  ✓ appended {} chapter(s) to {} (now {} total)",
        new_ids.len(),
        manifest_path.display(),
        merged.len()
    );
    println!("  new ids: {}", new_ids.join(","));
    println!();
    println!(
        "  Next: svrn enrich delta {} --chapters {}",
        parsed.corpus_id,
        new_ids.join(",")
    );

    0
}

// ── Arg parsing ──────────────────────────────────────────────────

#[derive(Debug)]
struct ParsedDelta {
    corpus_id: String,
    chapters: Vec<String>,
    phase: ResolvePhase,
    yes: bool,
    dry_run: bool,
    keep_staging: bool,
    /// Skip the subset extract/cluster/name (build) step and resolve
    /// whatever is already in `cache/questions.json`. Salvage path: when
    /// a long subset extract succeeded for most chapters but the build
    /// halted on one unparseable chapter, promote the run file
    /// (`runs/questions-subset-NNN.json`) to `cache/questions.json` and
    /// re-run with `--skip-build` to resolve + merge the recovered
    /// sketches without re-extracting. `--chapters` is optional here.
    skip_build: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedDelta, String> {
    let mut corpus_id: Option<String> = None;
    let mut chapters: Option<Vec<String>> = None;
    let mut phase = ResolvePhase::All;
    let mut yes = false;
    let mut dry_run = false;
    let mut keep_staging = false;
    let mut skip_build = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--chapters" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--chapters requires a comma-separated id list".to_string())?;
                chapters = Some(
                    raw.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
                i += 2;
            }
            "--phase" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| "--phase requires a value (3a|all)".to_string())?;
                phase = match val.as_str() {
                    "3a" => ResolvePhase::P3a,
                    "all" => ResolvePhase::All,
                    // `3b` is accepted as a synonym for `all` to match
                    // atlas-resolve's vocabulary, but the documented
                    // surface here is just 3a|all.
                    "3b" => ResolvePhase::P3b,
                    other => {
                        return Err(format!("unknown phase `{other}`; expected 3a or all"));
                    }
                };
                i += 2;
            }
            "--yes" => {
                yes = true;
                i += 1;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--keep-staging" => {
                keep_staging = true;
                i += 1;
            }
            "--skip-build" => {
                skip_build = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }

    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    // --chapters drives the subset extract; with --skip-build there is no
    // extract (we resolve whatever is already in cache), so it's optional.
    let chapters = match chapters {
        Some(c) if !c.is_empty() => c,
        Some(_) => return Err("--chapters list is empty".to_string()),
        None if skip_build => Vec::new(),
        None => {
            return Err(
                "missing --chapters <ids> (the chapter subset to enrich; required \
                 unless --skip-build)"
                    .to_string(),
            )
        }
    };
    Ok(ParsedDelta {
        corpus_id,
        chapters,
        phase,
        yes,
        dry_run,
        keep_staging,
        skip_build,
    })
}

#[derive(Debug)]
struct ParsedManifest {
    corpus_id: String,
    /// Optional narrowing filter, matched against each chunk's
    /// `metadata_raw` (the email JSON carries the staging-slice path,
    /// e.g. `symes-k_..._link`). `source_doc_id` is the bare javamail
    /// message-id and carries no slice info, so the prefix is matched
    /// against metadata, not source_doc_id. When omitted, every chunk
    /// not already covered by a chapter is treated as new.
    source_prefix: Option<String>,
    dry_run: bool,
}

fn parse_manifest_args(args: &[String]) -> Result<ParsedManifest, String> {
    let mut corpus_id: Option<String> = None;
    let mut source_prefix: Option<String> = None;
    let mut dry_run = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source-prefix" => {
                source_prefix = Some(
                    args.get(i + 1)
                        .ok_or_else(|| "--source-prefix requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }

    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    // --source-prefix is optional: new chunks are identified primarily
    // by being uncovered by any existing chapter. An empty prefix is
    // treated as "no filter".
    let source_prefix = source_prefix.filter(|s| !s.is_empty());
    Ok(ParsedManifest {
        corpus_id,
        source_prefix,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── delta parser ──────────────────────────────────────────

    #[test]
    fn parse_delta_requires_corpus_and_chapters() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"), "got: {err}");

        let err = parse_args(&["enron".into()]).unwrap_err();
        assert!(err.contains("--chapters"), "got: {err}");
    }

    #[test]
    fn parse_delta_defaults_phase_all_and_flags_off() {
        let p = parse_args(&[
            "enron".into(),
            "--chapters".into(),
            "sec_00321,sec_00322".into(),
        ])
        .unwrap();
        assert_eq!(p.corpus_id, "enron");
        assert_eq!(p.chapters, vec!["sec_00321", "sec_00322"]);
        assert_eq!(p.phase, ResolvePhase::All);
        assert!(!p.yes);
        assert!(!p.dry_run);
        assert!(!p.keep_staging);
    }

    #[test]
    fn parse_delta_trims_and_drops_empty_chapter_tokens() {
        let p = parse_args(&["c".into(), "--chapters".into(), " sec_1 , ,sec_2 ,".into()]).unwrap();
        assert_eq!(p.chapters, vec!["sec_1", "sec_2"]);
    }

    #[test]
    fn parse_delta_rejects_empty_chapters_list() {
        let err = parse_args(&["c".into(), "--chapters".into(), " , ".into()]).unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn parse_delta_accepts_phase_and_toggles() {
        let p = parse_args(&[
            "c".into(),
            "--chapters".into(),
            "sec_1".into(),
            "--phase".into(),
            "3a".into(),
            "--yes".into(),
            "--keep-staging".into(),
            "--dry-run".into(),
        ])
        .unwrap();
        assert_eq!(p.phase, ResolvePhase::P3a);
        assert!(p.yes);
        assert!(p.keep_staging);
        assert!(p.dry_run);
    }

    #[test]
    fn parse_delta_phase_all_maps_to_all() {
        let p = parse_args(&[
            "c".into(),
            "--chapters".into(),
            "sec_1".into(),
            "--phase".into(),
            "all".into(),
        ])
        .unwrap();
        assert_eq!(p.phase, ResolvePhase::All);
    }

    #[test]
    fn parse_delta_rejects_unknown_phase() {
        let err = parse_args(&[
            "c".into(),
            "--chapters".into(),
            "sec_1".into(),
            "--phase".into(),
            "42".into(),
        ])
        .unwrap_err();
        assert!(err.contains("unknown phase"), "got: {err}");
    }

    #[test]
    fn parse_delta_rejects_unknown_flag_and_extra_positional() {
        let err = parse_args(&[
            "c".into(),
            "--chapters".into(),
            "sec_1".into(),
            "--nope".into(),
        ])
        .unwrap_err();
        assert!(err.contains("unknown flag"), "got: {err}");

        let err = parse_args(&[
            "c".into(),
            "extra".into(),
            "--chapters".into(),
            "sec_1".into(),
        ])
        .unwrap_err();
        assert!(err.contains("unexpected positional"), "got: {err}");
    }

    // ── delta-manifest parser ─────────────────────────────────

    #[test]
    fn parse_manifest_requires_corpus_but_prefix_is_optional() {
        let err = parse_manifest_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"), "got: {err}");

        // --source-prefix is optional now: corpus-id alone is valid
        // (new chunks are identified by being uncovered by any chapter).
        let p = parse_manifest_args(&["enron".into()]).unwrap();
        assert_eq!(p.corpus_id, "enron");
        assert_eq!(p.source_prefix, None);
    }

    #[test]
    fn parse_manifest_happy_path() {
        let p = parse_manifest_args(&["enron".into(), "--source-prefix".into(), "symes-k".into()])
            .unwrap();
        assert_eq!(p.corpus_id, "enron");
        assert_eq!(p.source_prefix.as_deref(), Some("symes-k"));
        assert!(!p.dry_run);
    }

    #[test]
    fn parse_manifest_empty_prefix_is_none() {
        // An empty --source-prefix is treated as "no filter", not an error.
        let p = parse_manifest_args(&["c".into(), "--source-prefix".into(), "".into()]).unwrap();
        assert_eq!(p.source_prefix, None);
    }

    // ── suffix derivation ─────────────────────────────────────

    #[test]
    fn chapters_suffix_is_order_independent_and_stable() {
        let a = chapters_suffix(&["sec_2".into(), "sec_1".into()]);
        let b = chapters_suffix(&["sec_1".into(), "sec_2".into()]);
        assert_eq!(a, b, "suffix must not depend on input order");
        assert_eq!(a.len(), 16, "16 hex chars");
        // Different sets → (almost surely) different suffixes.
        let c = chapters_suffix(&["sec_1".into(), "sec_3".into()]);
        assert_ne!(a, c);
    }

    // ── backup helper ─────────────────────────────────────────

    #[test]
    fn backup_atlas_files_copies_present_skips_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(atlas.join("atoms.json"), b"{\"atoms\":[]}").unwrap();
        std::fs::write(atlas.join("edges.json"), b"{\"edges\":[]}").unwrap();
        // doc_to_atoms.json + cross_corpus_edges.json intentionally absent.

        let backup = atlas.join(".delta-backup-test");
        backup_atlas_files(&atlas, &backup).unwrap();

        assert!(backup.join("atoms.json").exists());
        assert!(backup.join("edges.json").exists());
        assert!(!backup.join("doc_to_atoms.json").exists());
        assert!(!backup.join("cross_corpus_edges.json").exists());
        // Content preserved byte-for-byte.
        assert_eq!(
            std::fs::read(backup.join("atoms.json")).unwrap(),
            b"{\"atoms\":[]}"
        );
    }

    #[test]
    fn cleanup_staging_respects_keep_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("sov-delta-x");
        std::fs::create_dir_all(&staging).unwrap();

        // keep = true → dir survives.
        cleanup_staging(&staging, true);
        assert!(staging.exists(), "keep=true must leave the dir");

        // keep = false → dir removed.
        cleanup_staging(&staging, false);
        assert!(!staging.exists(), "keep=false must delete the dir");

        // Idempotent: removing an already-gone dir is a no-op (no panic).
        cleanup_staging(&staging, false);
    }

    // ── staging → migrate → apply round-trip ──────────────────
    //
    // Exercises the new orchestration's core data-flow (delta steps
    // 5→6) against the real engine primitives, WITHOUT the daemon /
    // LLM phases: seed a live (already content-hashed) atlas + a
    // staging atlas with sequential ids; content-hash the staging
    // with the SAME corpus_id; read it back; build the additive
    // `AtomsDelta`; `apply_atom_delta` into the live atlas. Asserts
    // net-new atoms append and a name-collision replaces in place
    // rather than duplicating — the contract `cmd_delta` relies on.
    //
    // Modeled on the fixture style in
    // `corpus-engine/.../atoms_delta.rs` + `migrate_ids.rs`.
    mod roundtrip {
        use super::super::*;
        use corpus_engine::enrichment::atlas::atoms::{
            AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity,
        };
        use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
        use std::path::Path;

        /// One sequential-id Person entity. `idx` seeds the legacy
        /// `entity-NNNN` id; `migrate_atlas_ids` rewrites it to the
        /// content-hash derived from (name, type, corpus).
        fn seq_entity(idx: usize, name: &str, chunk: &str) -> AtomEnvelope {
            AtomEnvelope::Entity(Entity {
                id: AtomId::entity(idx),
                canonical_name: name.into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new(chunk, None),
                description: format!("desc of {name}"),
                defining_quote: None,
                salience: 0.5,
                enrichment_depth: EnrichmentDepth::Extracted,
                affiliation: None,
                role: None,
                participants: vec![],
                provenance: Default::default(),
                attributes: serde_json::Map::new(),
                concept_kind: None,
            })
        }

        fn write_atoms(atlas_dir: &Path, atoms: Vec<AtomEnvelope>) {
            std::fs::create_dir_all(atlas_dir).unwrap();
            let af = AtomsFile::new(atoms);
            std::fs::write(
                atlas_dir.join("atoms.json"),
                serde_json::to_string_pretty(&af).unwrap(),
            )
            .unwrap();
        }

        #[test]
        fn staging_migrate_apply_appends_new_and_replaces_collision() {
            const CORPUS: &str = "delta-roundtrip-corpus";
            let tmp = tempfile::tempdir().unwrap();
            let live = tmp.path().join("live").join("atlas");
            let staging = tmp.path().join("staging").join("atlas");

            // Live atlas: one entity "Alice", already content-hashed
            // under CORPUS (mirrors a real already-migrated atlas).
            write_atoms(&live, vec![seq_entity(1, "Alice", "doc_live")]);
            migrate_atlas_ids(&live, CORPUS, false).unwrap();
            let alice_id = AtomId::entity_content_hash("Alice", &EntityType::Person, CORPUS);
            // Sanity: the live atom is the content-hash Alice.
            {
                let live_atoms = read_atlas_atoms(&live).unwrap();
                assert_eq!(live_atoms.atoms.len(), 1);
                assert_eq!(live_atoms.atoms[0].id(), &alice_id);
                assert!(live_atoms.atoms[0].id().is_content_hash());
            }

            // Staging atlas: sequential ids, one re-mentioning "Alice"
            // (collision) and one net-new "Bob".
            write_atoms(
                &staging,
                vec![
                    seq_entity(1, "Alice", "doc_staging"),
                    seq_entity(2, "Bob", "doc_staging"),
                ],
            );

            // Step 5: content-hash the staging atoms with the REAL
            // corpus_id so collisions line up with the live atlas.
            migrate_atlas_ids(&staging, CORPUS, false).unwrap();

            // Step 6: read staging back, build the additive delta,
            // apply it to the live atlas.
            let staged = read_atlas_atoms(&staging).unwrap();
            assert_eq!(staged.atoms.len(), 2);
            let staged_count = staged.atoms.len();
            let delta = AtomsDelta {
                added: staged.atoms,
                added_edges: vec![],
                removed_doc_ids: vec![],
                upserted_docs: vec![],
            };
            let summary = apply_atom_delta(&live, delta).unwrap();

            // Alice collided (replaced in place) → only Bob is net-new.
            assert_eq!(summary.atoms_before, 1, "live started with Alice");
            assert_eq!(summary.atoms_added, 1, "only Bob is appended");
            assert_eq!(summary.atoms_after, 2, "Alice (replaced) + Bob");
            // The replaced-in-place math the CLI prints.
            assert_eq!(staged_count.saturating_sub(summary.atoms_added), 1);

            // Final atlas carries exactly Alice + Bob, ids unique +
            // content-hash, Alice's id stable across the merge.
            let after = read_atlas_atoms(&live).unwrap();
            assert_eq!(after.atoms.len(), 2);
            let bob_id = AtomId::entity_content_hash("Bob", &EntityType::Person, CORPUS);
            let ids: std::collections::HashSet<String> = after
                .atoms
                .iter()
                .map(|a| a.id().as_str().to_string())
                .collect();
            assert!(ids.contains(alice_id.as_str()), "Alice id preserved");
            assert!(ids.contains(bob_id.as_str()), "Bob appended");
            assert!(after.atoms.iter().all(|a| a.id().is_content_hash()));
        }

        #[test]
        fn empty_staging_yields_noop_delta_against_live() {
            // A subset that resolves to zero atoms must not disturb the
            // live atlas. (cmd_delta guards this earlier via the
            // empty-sections check, but the apply path is a no-op too.)
            const CORPUS: &str = "delta-empty-corpus";
            let tmp = tempfile::tempdir().unwrap();
            let live = tmp.path().join("live").join("atlas");
            write_atoms(&live, vec![seq_entity(1, "Alice", "doc_live")]);
            migrate_atlas_ids(&live, CORPUS, false).unwrap();

            let delta = AtomsDelta {
                added: vec![],
                added_edges: vec![],
                removed_doc_ids: vec![],
                upserted_docs: vec![],
            };
            let summary = apply_atom_delta(&live, delta).unwrap();
            assert_eq!(summary.atoms_added, 0);
            assert!(
                summary.files_touched.is_empty(),
                "empty delta touches nothing"
            );
            assert_eq!(read_atlas_atoms(&live).unwrap().atoms.len(), 1);
        }
    }
}
