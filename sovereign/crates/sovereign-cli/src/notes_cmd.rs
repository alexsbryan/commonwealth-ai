//! `sovereign notes` — read and write durable working notes.
//!
//! Merges:
//!
//! - `sovereign reflect` (the developer-facing read view)  → `sovereign notes`
//! - `sovereign atos promote <id> --to <scope>`            → `sovereign notes promote ...`
//! - new write surface (used by Phase 7 commit harvester)  → `sovereign notes add ...`
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
//! sources from `corpus_engine::NoteSource`) lights up here when the
//! audit assembly is rewritten — until then, this surface is a
//! lossless rename + write entry point.

use std::path::PathBuf;

use corpus_engine::{NoteScope, NoteSource, NoteStore};

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    match args.first().map(String::as_str) {
        Some("add") => cmd_add(&args[1..]).await,
        Some("promote") => crate::dev_bin::exec("atos-status-promote", &args[1..]),
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

/// `sovereign notes add` — append a new note.
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
            "notes add: could not locate notes.db. Run `sovereign init` in this \
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

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign notes",
    summary: "Read and write durable working notes (the audit's primary input).",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign notes                           30-day reflection view (default)\n\
             sovereign notes add --kind <k> -m \"...\"   Append a note\n\
             sovereign notes promote <id> --to <s>     Promote scope\n\
             sovereign notes --since 7d --tool <name>  Reflection filters",
        ),
        crate::util::help::HelpSection::Notes(
            "Replaces `sovereign reflect` and `sovereign atos promote`. Old names \
             still work and forward here. The Phase 7 audit-hardening rewrite adds \
             multi-source views (--kind, --source, --feature filters).",
        ),
    ],
};








