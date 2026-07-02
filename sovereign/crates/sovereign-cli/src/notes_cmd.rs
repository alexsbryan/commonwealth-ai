// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn notes` — read and write durable working notes.
//!
//! Merges:
//!
//! - `svrn reflect` (the developer-facing read view)  → `svrn notes`
//! - `svrn atos promote <id> --to <scope>`            → `svrn notes promote ...`
//! - new write surface (used by Phase 7 commit harvester)  → `svrn notes add ...`
//!
//! Phase 1 (this file): scaffolds the three surfaces. The default
//! read path delegates to [`crate::reflect_cmd::run_reflect`] so the
//! 30-day reflection view that engineers rely on today keeps working
//! verbatim. `add` is the new write surface — Phase 7 hooks the
//! daemon's git-HEAD-poll harvester here when it detects a new
//! commit message worth recording. `promote` forwards to the
//! existing atos handler.
//!
//! The Phase 7 multi-source audit (extracted/inferred/observed
//! sources from `corpus_engine_notes::NoteSource`) lights up here when the
//! audit assembly is rewritten — until then, this surface is a
//! lossless rename + write entry point.

use std::path::PathBuf;

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    match args.first().map(String::as_str) {
        Some("add") => cmd_add(&args[1..]).await,
        Some("promote") => crate::dev_bin::exec("atos-status-promote", &args[1..]),
        Some("migrate-from") => cmd_migrate_from(&args[1..]).await,
        _ => {
            // Default + filter flags: forward to the canonical
            // reflection view (without the deprecation banner —
            // that only fires when the user types `sovereign
            // reflect`). The `--kind` / `--source` filters
            // described in the plan are implemented in Phase 7
            // alongside the audit assembly rewrite.
            crate::reflect_cmd::run_reflect_view(args).await
        }
    }
}

/// `svrn notes add` — append a new note.
///
/// Used by the Phase 7 commit-message harvester when the daemon's
/// reindexer notices a new commit. Also useful for ad-hoc human
/// writes ("record this decision I just made"). The MCP `note` tool
/// remains the agent-facing surface.
///
/// Required flags:
///   --kind <k>      One of: decision, attempt, invariant, todo,
///                   reflection, deviation, commitment, follow_up,
///                   goal, uncertainty, postmortem_pointer,
///                   redteam_finding (the v5+ schema kinds).
///   --message "..." The note body.
///
/// Optional:
///   --source <s>    agent | committed | extracted | inferred |
///                   observed. Defaults to `agent`.
///   --feature <id>  Tag the note to a feature (sets scope=feature).
///   --symbols a,b   Comma-separated symbol names.
///   --files a,b     Comma-separated file paths.
///   --session <id>  Session id (defaults to `cli-add`).
///   --supersedes <id> Mark this note as a reversal of an existing one.
///   --data-dir <p>  Override notes.db location.
async fn cmd_add(args: &[String]) -> i32 {
    let mut kind: Option<String> = None;
    let mut message: Option<String> = None;
    let mut source = NoteSource::Agent;
    let mut feature_id: Option<String> = None;
    let mut symbols: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut session_id = "cli-add".to_string();
    let mut supersedes: Option<String> = None;
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--kind" => {
                i += 1;
                kind = args.get(i).cloned();
            }
            "--message" | "-m" => {
                i += 1;
                message = args.get(i).cloned();
            }
            "--source" => {
                i += 1;
                let raw = match args.get(i) {
                    Some(s) => s,
                    None => {
                        eprintln!("notes add: --source requires a value");
                        return 2;
                    }
                };
                source = match NoteSource::parse(raw) {
                    Some(s) => s,
                    None => {
                        eprintln!(
                            "notes add: unknown --source {raw:?}. \
                             Valid: agent | committed | extracted | inferred | observed."
                        );
                        return 2;
                    }
                };
            }
            "--feature" => {
                i += 1;
                feature_id = args.get(i).cloned();
            }
            "--symbols" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    symbols = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            "--files" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    files = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            "--session" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    session_id = v.clone();
                }
            }
            "--supersedes" => {
                i += 1;
                supersedes = args.get(i).cloned();
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("notes add: unknown flag {other:?}");
                return 2;
            }
        }
        i += 1;
    }

    let Some(kind) = kind else {
        eprintln!("notes add: --kind is required");
        return 2;
    };
    let Some(message) = message else {
        eprintln!("notes add: --message is required");
        return 2;
    };

    let Some(notes_db) = crate::reflect_cmd::find_notes_db(data_dir.as_deref()) else {
        eprintln!(
            "notes add: could not locate notes.db. Run `svrn init` in this \
             repo (or pass --data-dir <path>)."
        );
        return 1;
    };

    let store = match NoteStore::open(&notes_db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("notes add: open {}: {e}", notes_db.display());
            return 1;
        }
    };

    // Translate feature_id presence into the scope the schema
    // requires (Feature scope mandates a non-empty feature_id).
    let scope = if feature_id.is_some() {
        NoteScope::Feature
    } else {
        NoteScope::Global
    };

    let id = match store
        .write_note_with_source(
            &kind,
            &message,
            symbols,
            files,
            &session_id,
            scope,
            feature_id.as_deref(),
            None, // related_entity — unused in CLI add
            source,
            supersedes.as_deref(),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            eprintln!("notes add: write failed: {e}");
            return 1;
        }
    };

    println!("{id}");
    0
}

/// `svrn notes migrate-from <path>` — merge a stray local
/// `notes.db` into the canonical store (`~/.sovereign/notes.db`).
///
/// Use case: pre-unification, some CLI surfaces opened a
/// project-local `<repo>/.sovereign/notes.db`. After
/// registry.rs was aligned to the canonical home path, those
/// local DBs become orphans. This command merges them in,
/// content_hash-deduplicating so re-running is idempotent.
///
/// Usage:
///   sovereign notes migrate-from /path/to/notes.db
///   sovereign notes migrate-from /path/to/notes.db --target /alt/notes.db
async fn cmd_migrate_from(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!(
            "notes migrate-from: requires a source path. \
             Example: sovereign notes migrate-from \
             /Users/<you>/dev/<repo>/.sovereign/notes.db"
        );
        return 2;
    }
    let mut source: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                target = args.get(i).map(PathBuf::from);
            }
            other if !other.starts_with("--") => {
                source = Some(PathBuf::from(other));
            }
            other => {
                eprintln!("notes migrate-from: unknown flag {other:?}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(source) = source else {
        eprintln!("notes migrate-from: source path required");
        return 2;
    };
    if !source.exists() {
        eprintln!("notes migrate-from: {} does not exist", source.display());
        return 1;
    }
    let canonical_root = crate::util::dirs::sovereign_root();
    let target = target.unwrap_or_else(|| canonical_root.join("notes.db"));
    if source == target {
        eprintln!(
            "notes migrate-from: source and target are the same path ({})",
            source.display()
        );
        return 2;
    }

    // Open the source as a read-only NoteStore so its v0→v10
    // migrations fire if needed (gives us the `content_hash`
    // column required for cross-DB dedup). Then scan its full
    // contents and ingest_remote_notes into the canonical store
    // for idempotent content-hash dedup, supersedes-chain
    // preservation, and fork detection.
    let source_store = match NoteStore::open(&source) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "notes migrate-from: cannot open source {}: {e}",
                source.display()
            );
            return 1;
        }
    };
    let target_store = match NoteStore::open(&target) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "notes migrate-from: cannot open target {}: {e}",
                target.display()
            );
            return 1;
        }
    };

    // Pull every global, non-private, non-tombstoned note from
    // source. Use `events_for_content_hashes` after gathering ids
    // — gives us full embeddings + entities so the migration also
    // ports T1/T2 artifacts in one pass.
    let source_hashes = match source_store.content_hash_digest().await {
        Ok(digest) => {
            let mut all = Vec::new();
            for (bucket, _) in digest {
                match source_store.content_hashes_in_bucket(bucket).await {
                    Ok(mut h) => all.append(&mut h),
                    Err(e) => {
                        eprintln!("notes migrate-from: scan bucket {:02x}: {e}", bucket);
                    }
                }
            }
            all
        }
        Err(e) => {
            eprintln!("notes migrate-from: digest source: {e}");
            return 1;
        }
    };
    if source_hashes.is_empty() {
        println!(
            "notes migrate-from: source has no migratable global notes (private/feature/session scopes stay local)"
        );
        return 0;
    }

    let events = match source_store.events_for_content_hashes(&source_hashes).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("notes migrate-from: events_for_content_hashes: {e}");
            return 1;
        }
    };
    let total = events.len();
    let report = match target_store.ingest_remote_notes(events).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("notes migrate-from: ingest into target: {e}");
            return 1;
        }
    };

    println!(
        "notes migrate-from: source={src} target={tgt}\n  scanned: {total}\n  inserted: {ins}\n  deduplicated: {dup}\n  tombstoned: {tomb}\n  forked: {fork}\n  rejected: {rej}",
        src = source.display(),
        tgt = target.display(),
        ins = report.inserted,
        dup = report.deduplicated,
        tomb = report.tombstoned,
        fork = report.forked,
        rej = report.rejected,
    );
    0
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn notes",
    summary: "Read and write durable working notes (the audit's primary input).",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn notes                           30-day reflection view (default)\n\
             sovereign notes add --kind <k> -m \"...\"   Append a note\n\
             sovereign notes promote <id> --to <s>     Promote scope\n\
             sovereign notes migrate-from <path>       Merge a stray local notes.db into ~/.sovereign/notes.db\n\
             sovereign notes --since 7d --tool <name>  Reflection filters",
        ),
        crate::util::help::HelpSection::Notes(
            "Replaces `svrn reflect` and `svrn atos promote`. Old names \
             still work and forward here. The Phase 7 audit-hardening rewrite adds \
             multi-source views (--kind, --source, --feature filters).",
        ),
    ],
};
