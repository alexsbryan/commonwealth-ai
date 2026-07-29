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

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use corpus_engine_notes::{is_ephemeral_kind, NoteRow, NoteScope, NoteSource, NoteStore};

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    match args.first().map(String::as_str) {
        Some("add") => cmd_add(&args[1..]).await,
        Some("list") => cmd_list(&args[1..]).await,
        Some("retrieval-audit") => crate::notes_retrieval_cmd::run(&args[1..]).await,
        Some("promote") => crate::dev_bin::exec("atos-status-promote", &args[1..]),
        Some("migrate-from") => cmd_migrate_from(&args[1..]).await,
        Some("rationalize") => cmd_rationalize(&args[1..]).await,
        Some("gc") => cmd_gc(&args[1..]).await,
        // A search/filter request is a request to SEE NOTES, so it routes to
        // the list view rather than the reflection summary.
        //
        // Before this, `svrn notes` had no read-back for the notes
        // `svrn notes add` writes: the default path forwarded everything to
        // the 30-day reflection view, and `--query` was not a flag at all —
        // it printed "Unknown flag: --query" and dumped `svrn reflect` help.
        // `.claude/hooks/session-boot.sh` has been telling operators to run
        // `sovereign notes --query "<id>"` to read a truncated note the whole
        // time, so the repo shipped a hook teaching a flag the binary
        // rejected, and the only working search surface was the MCP `notes`
        // tool. Caught 2026-07-28 by the `agent-notes` journey, whose title
        // is literally "Leave durable decisions and read them back".
        _ if args.iter().any(|a| is_list_flag(a)) => cmd_list(args).await,
        _ => {
            // Bare `svrn notes` keeps forwarding to the canonical reflection
            // view (without the deprecation banner — that only fires when the
            // user types `sovereign reflect`). Engineers rely on that view;
            // only an explicit filter switches to the list.
            crate::reflect_cmd::run_reflect_view(args).await
        }
    }
}

/// Flags that mean "show me matching notes" rather than "summarize my
/// reflections". `--since` / `--tool` are deliberately NOT here: they are
/// long-standing reflection-view filters and must keep that meaning.
fn is_list_flag(arg: &str) -> bool {
    let name = arg.split('=').next().unwrap_or(arg);
    matches!(name, "--query" | "-q" | "--symbol" | "--file" | "--id")
}

/// A hex-ish fragment long enough to be a note id rather than a search term.
///
/// 8 is the short-id width printed by every surface in this stack; a UUID is
/// 36 with dashes. Below 8 the risk of hijacking a legitimate search
/// (`svrn notes --query cafe`) outweighs the convenience.
fn looks_like_note_id(q: &str) -> bool {
    let len = q.len();
    (8..=36).contains(&len) && q.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// `svrn notes list` — the read-back surface for the notes this CLI writes.
///
/// Deliberately the same query path the MCP `notes` tool uses
/// (`NoteStore::read_notes`: FTS over content, plus symbol / file / kind
/// filters), so the CLI and the agent surface cannot drift into disagreeing
/// about what is stored.
///
/// Flags:
///   --query <s> / -q   Full-text search over note content.
///   --id <id>          Exact id (prefix-matched, so an 8-char short id works).
///   --kind <k>         Repeatable. decision | attempt | invariant | todo | …
///   --symbol <s>       Repeatable. Notes tagged with this symbol.
///   --file <p>         Repeatable. Notes tagged with this file path.
///   --limit <n>        Default 20, capped at 100 by the store.
///   --include-retired  Show retired notes too (default: hide).
///   --full             Print whole bodies instead of the first 3 lines.
///   --data-dir <p>     Override notes.db location.
async fn cmd_list(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_LIST);
        return 0;
    }

    let mut query: Option<String> = None;
    let mut id_prefix: Option<String> = None;
    let mut kinds: Vec<String> = Vec::new();
    let mut symbols: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut limit: usize = 20;
    let mut include_retired = false;
    let mut full = false;
    let mut data_dir: Option<PathBuf> = None;

    // Accept both `--flag value` and `--flag=value`; the hook and the docs
    // use the first form, `cli-contract.toml` steps use the second.
    let mut i = 0;
    while i < args.len() {
        let (name, inline) = match args[i].split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (args[i].clone(), None),
        };
        let mut take = |i: &mut usize| -> Option<String> {
            if let Some(v) = inline.clone() {
                return Some(v);
            }
            *i += 1;
            args.get(*i).cloned()
        };
        match name.as_str() {
            "--query" | "-q" => query = take(&mut i),
            "--id" => id_prefix = take(&mut i),
            "--kind" => {
                if let Some(v) = take(&mut i) {
                    kinds.push(v);
                }
            }
            "--symbol" => {
                if let Some(v) = take(&mut i) {
                    symbols.push(v);
                }
            }
            "--file" => {
                if let Some(v) = take(&mut i) {
                    files.push(v);
                }
            }
            "--limit" => {
                if let Some(v) = take(&mut i) {
                    match v.parse::<usize>() {
                        Ok(n) if n > 0 => limit = n,
                        _ => {
                            eprintln!("notes list: --limit must be a positive number, got {v:?}");
                            return 2;
                        }
                    }
                }
            }
            "--include-retired" => include_retired = true,
            "--full" => full = true,
            "--data-dir" => data_dir = take(&mut i).map(PathBuf::from),
            other if other.starts_with('-') => {
                eprintln!("notes list: unknown flag {other:?}");
                crate::util::help::print(&HELP_LIST);
                return 2;
            }
            // A bare word is the query: `svrn notes list embedrouter`.
            other => query = Some(other.to_string()),
        }
        i += 1;
    }

    let Some(notes_db) = crate::reflect_cmd::find_notes_db(data_dir.as_deref()) else {
        eprintln!(
            "notes list: could not locate notes.db. Run `svrn init` in this \
             repo (or pass --data-dir <path>)."
        );
        return 1;
    };
    let store = match NoteStore::open(&notes_db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("notes list: open {}: {e}", notes_db.display());
            return 1;
        }
    };

    // `--id` is an exact lookup, not a search: FTS would tokenize a UUID and
    // match nothing useful. Prefix so the 8-char short ids printed everywhere
    // else in this stack resolve.
    let rows = if let Some(prefix) = id_prefix.as_deref() {
        match store.scan_all(true).await {
            Ok(all) => all
                .into_iter()
                .filter(|r| r.id.starts_with(prefix))
                .take(limit)
                .collect::<Vec<_>>(),
            Err(e) => {
                eprintln!("notes list: {e}");
                return 1;
            }
        }
    } else {
        match store
            .read_notes(
                query.as_deref(),
                &symbols,
                &files,
                &kinds,
                limit,
                include_retired,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("notes list: {e}");
                return 1;
            }
        }
    };

    // An id pasted into --query finds nothing through FTS (the tokenizer has
    // no useful term for a UUID fragment), and the note-injection hooks quote
    // ids at exactly this call — "full note: `sovereign notes --query <id>`".
    // Rather than hand back an empty result for advice the tooling itself
    // gives, retry an id lookup when the query LOOKS like an id and the
    // search came back empty.
    let rows = if rows.is_empty() && id_prefix.is_none() {
        match query.as_deref().filter(|q| looks_like_note_id(q)) {
            Some(q) => match store.scan_all(true).await {
                Ok(all) => all
                    .into_iter()
                    .filter(|r| r.id.starts_with(q))
                    .take(limit)
                    .collect::<Vec<_>>(),
                Err(_) => rows,
            },
            None => rows,
        }
    } else {
        rows
    };

    if rows.is_empty() {
        // Exit 0: "no notes match" is an answer, not a failure. Say what was
        // searched so an empty result is not mistaken for a broken lookup —
        // that ambiguity is what made the missing surface hard to notice.
        println!("no notes matched (db: {})", notes_db.display());
        return 0;
    }

    for row in &rows {
        let retired = if row.retired_at.is_some() {
            "  [retired]"
        } else {
            ""
        };
        println!(
            "── {}  [{}]  {}{}",
            &row.id[..row.id.len().min(8)],
            row.kind,
            row.session_id,
            retired
        );
        if full {
            println!("{}", row.content);
        } else {
            for line in row.content.lines().take(3) {
                println!("   {line}");
            }
            if row.content.lines().count() > 3 {
                println!("   … (--full for the whole body)");
            }
        }
        println!();
    }
    println!(
        "{} note(s) from {}",
        rows.len(),
        notes_db.display()
    );
    0
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
///   svrn notes migrate-from /path/to/notes.db
///   svrn notes migrate-from /path/to/notes.db --target /alt/notes.db
async fn cmd_migrate_from(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!(
            "notes migrate-from: requires a source path. \
             Example: svrn notes migrate-from \
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
             svrn notes add --kind <k> -m \"...\"   Append a note\n\
             svrn notes list [--query <s>]        List / search the notes themselves\n\
             svrn notes --query <s>               Same as `list --query` (search implies the list view)\n\
             svrn notes list --id <id>            Read one note by id (8-char short ids work)\n\
             svrn notes promote <id> --to <s>     Promote scope\n\
             svrn notes migrate-from <path>       Merge a stray local notes.db into ~/.sovereign/notes.db\n\
             svrn notes rationalize               Candidate report: consolidate/supersede moves (no LLM, no writes)\n\
             svrn notes rationalize --distill     Preview the LLM-written survivors/verdicts (no writes)\n\
             svrn notes rationalize --apply --yes Write survivors + retire-with-pointer links\n\
             svrn notes gc [--days 30]            TTL sweep: tombstone expired telemetry (daemon runs this daily)\n\
             svrn notes retrieval-audit           Injected-note hit-rate: did injected notes get used? (E2/P4 baseline)\n\
             svrn notes --since 7d --tool <name>  Reflection filters",
        ),
        crate::util::help::HelpSection::Notes(
            "Replaces `svrn reflect` and `svrn atos promote`. Old names \
             still work and forward here. Bare `svrn notes` is the reflection \
             SUMMARY; `svrn notes list` is the notes themselves — the same \
             NoteStore::read_notes query the MCP `notes` tool uses, so the two \
             surfaces cannot disagree about what is stored.",
        ),
    ],
};

const HELP_LIST: crate::util::help::Help = crate::util::help::Help {
    command: "svrn notes list",
    summary: "List and search durable notes — the read-back for `svrn notes add`.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn notes list [--query <s>] [--kind <k>]... [--limit <n>]\n    \
             [--symbol <s>]... [--file <p>]... [--id <id>]\n    \
             [--include-retired] [--full] [--data-dir <p>]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--query <s> / -q", "Full-text search over note content (a bare word works too)"),
            ("--id <id>", "Exact id, prefix-matched so 8-char short ids resolve"),
            ("--kind <k>", "Repeatable: decision | attempt | invariant | todo | reflection | …"),
            ("--symbol <s>", "Repeatable: notes tagged with this symbol"),
            ("--file <p>", "Repeatable: notes tagged with this file path"),
            ("--limit <n>", "Max results (default 20; the store caps at 100)"),
            ("--include-retired", "Include retired notes (default: hidden)"),
            ("--full", "Print whole bodies instead of the first 3 lines"),
            ("--data-dir <p>", "Override the notes.db location"),
        ]),
        crate::util::help::HelpSection::Examples(&[
            ("svrn notes list --kind decision", "Every decision on record"),
            ("svrn notes --query \"grounding gate\"", "Search; no `list` needed"),
            ("svrn notes list --symbol EmbedRouter", "Notes tagged to a symbol"),
            ("svrn notes list --id 625ca452 --full", "Read one note whole"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Exits 0 with `no notes matched` when nothing hits — an empty \
             result is an answer, not an error. The db actually searched is \
             printed so an empty result is never confused with the wrong db.",
        ),
    ],
};

/// `svrn notes gc` — TTL sweep of operational-exhaust notes (the daemon runs
/// this daily on its own; this is the manual/cron entry point). Tombstones
/// ephemeral-kind notes older than `--days` (default 30). Never touches durable
/// kinds. Non-destructive (tombstone keeps the row).
async fn cmd_gc(args: &[String]) -> i32 {
    let mut days: i64 = 30;
    let mut data_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--days" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<i64>().ok()) {
                    Some(d) => days = d,
                    None => {
                        eprintln!("notes gc: --days wants a number");
                        return 2;
                    }
                }
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("notes gc: unknown flag {other:?}");
                return 2;
            }
        }
        i += 1;
    }

    let Some(notes_db) = crate::reflect_cmd::find_notes_db(data_dir.as_deref()) else {
        eprintln!("notes gc: could not locate notes.db (pass --data-dir <path>).");
        return 1;
    };
    let store = match NoteStore::open(&notes_db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("notes gc: open {}: {e}", notes_db.display());
            return 1;
        }
    };
    match store.purge_expired_ephemeral(days * 86_400).await {
        Ok(n) => {
            println!(
                "notes gc: tombstoned {n} ephemeral note(s) older than {days}d \
                 (kinds: {}). Durable knowledge untouched.",
                corpus_engine_notes::EPHEMERAL_KINDS.join(", ")
            );
            0
        }
        Err(e) => {
            eprintln!("notes gc: sweep failed: {e}");
            1
        }
    }
}

/// Overlap threshold above which two same-anchor notes are offered as a
/// supersede pair (token-Jaccard on content). Below it, a shared anchor is
/// treated as coincidence, not overtaking.
const SUPERSEDE_MIN_OVERLAP: f32 = 0.4;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Deterministic candidate report, no LLM, no writes.
    Report,
    /// Run the LLM to preview survivors / adjudicate pairs. No writes.
    Distill,
    /// Distill AND write survivors + retire-with-pointer links (needs `--yes`).
    Apply,
}

#[derive(Clone, Copy, PartialEq)]
enum MoveKind {
    Consolidate,
    Supersede,
    /// Reflection whose flagged limitation may have been fixed (code churned on
    /// its anchor since it was written). Distinct from the others because a
    /// reflection's staleness is a FIX event, not age or a duplicate.
    ReflectionFix,
}

/// An active reflection that is a fix-check candidate: its anchor (tool_name or
/// symbol) has code churn since the reflection was written, so the limitation it
/// flags MIGHT be resolved. Only the model rules resolved-vs-still-relevant.
struct ReflectionFixCandidate<'a> {
    note: &'a NoteRow,
    anchor: String,
    /// Commit subjects touching the anchor since `note.created_at`.
    churn: Vec<String>,
}

/// A telemetry-log consolidation group: all log rows for one (kind, tool,
/// month), which distill into a single durable survivor carrying the outcome
/// breakdown.
struct LogGroup<'a> {
    kind: String,
    tool: String,
    month: String,
    members: Vec<&'a NoteRow>,
    /// outcome → count, descending.
    outcomes: Vec<(String, usize)>,
}

/// `svrn notes rationalize` — move the note store toward all-signal.
///
/// The store gossips across peers and is fed (noisily) from commit messages and
/// per-turn telemetry, so low-signal rows accrete. Retrieval hides only
/// retired/tombstoned notes, not merely-stale ones. This surfaces — and, with
/// `--apply`, executes — two typed moves, and NEVER deletes:
///
///   CONSOLIDATE (many → one): a cluster of related low-signal rows distills
///   into one survivor. The model writes the survivor; the members are retired
///   with `retired_by` pointing at it — kept + auditable, never tombstoned.
///
///   SUPERSEDE (one → one): a newer note overtakes an older one on the same
///   anchor. The model rules "overtaking vs complementary"; on OVERTAKES the
///   older is retired with a pointer to the newer.
///
///   REFLECTION FIX-CHECK: an active reflection whose flagged limitation may be
///   resolved — its anchor (tool_name/symbol) has code churn since it was
///   written. The model rules RESOLVED vs STILL-RELEVANT; on RESOLVED the
///   reflection is retired. A reflection's staleness is a FIX event, not age —
///   so this is deliberately NOT the time-based ephemeral TTL.
///
/// Modes: default = deterministic candidate report (no LLM, no writes);
/// `--distill` = run the model and PREVIEW every survivor/verdict (no writes);
/// `--apply --yes` = also write. Scope with `--only consolidate|supersede|reflections`,
/// `--kind <k>`, `--limit <n>`. `--model` defaults to `primary` (never the fast
/// slot — distillation quality needs the big model). Fix-check needs to run
/// inside the code repo the reflections describe (git churn is the fix-signal).
async fn cmd_rationalize(args: &[String]) -> i32 {
    let mut data_dir: Option<PathBuf> = None;
    let mut mode = Mode::Report;
    let mut commit = false;
    let mut only: Option<MoveKind> = None;
    let mut kind_filter: Option<String> = None;
    let mut limit: usize = 25;
    let mut model = "primary".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            "--distill" => mode = Mode::Distill,
            "--apply" => mode = Mode::Apply,
            "--yes" | "-y" => commit = true,
            "--only" => {
                i += 1;
                only = match args.get(i).map(String::as_str) {
                    Some("consolidate") => Some(MoveKind::Consolidate),
                    Some("supersede") => Some(MoveKind::Supersede),
                    Some("reflections") | Some("reflection-fix") => Some(MoveKind::ReflectionFix),
                    other => {
                        eprintln!(
                            "notes rationalize: --only wants consolidate|supersede|reflections, got {other:?}"
                        );
                        return 2;
                    }
                };
            }
            "--kind" => {
                i += 1;
                kind_filter = args.get(i).cloned();
            }
            "--limit" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) => limit = n,
                    None => {
                        eprintln!("notes rationalize: --limit wants a number");
                        return 2;
                    }
                }
            }
            "--model" => {
                i += 1;
                if let Some(m) = args.get(i) {
                    model = m.clone();
                }
            }
            other => {
                eprintln!("notes rationalize: unknown flag {other:?}");
                return 2;
            }
        }
        i += 1;
    }

    let Some(notes_db) = crate::reflect_cmd::find_notes_db(data_dir.as_deref()) else {
        eprintln!(
            "notes rationalize: could not locate notes.db. Run `svrn init` in this \
             repo (or pass --data-dir <path>)."
        );
        return 1;
    };
    let store = match NoteStore::open(&notes_db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("notes rationalize: open {}: {e}", notes_db.display());
            return 1;
        }
    };

    // FULL scan — not read_notes (which caps at a 100-row retrieval window and
    // would hide the very bulk we're here to rationalize).
    let notes = match store.scan_all(false).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("notes rationalize: scan failed: {e}");
            return 1;
        }
    };

    // ---- composition (the truth the windowed view hid) ----
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_source: BTreeMap<&str, usize> = BTreeMap::new();
    for n in &notes {
        *by_kind.entry(n.kind.as_str()).or_default() += 1;
        *by_source.entry(n.source.as_str()).or_default() += 1;
    }
    let mut kind_hist: Vec<(&str, usize)> = by_kind.into_iter().collect();
    kind_hist.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    let mut source_hist: Vec<(&str, usize)> = by_source.into_iter().collect();
    source_hist.sort_by_key(|(_, c)| std::cmp::Reverse(*c));

    // ---- CONSOLIDATE candidates ----
    // (a) Telemetry log: group by (kind, tool, month). One survivor per group,
    //     summarizing the outcome distribution (the useful-vs-gap signal).
    let mut log_map: BTreeMap<(String, String, String), Vec<&NoteRow>> = BTreeMap::new();
    let mut dup_buckets: BTreeMap<String, Vec<&NoteRow>> = BTreeMap::new();
    for n in &notes {
        if is_ephemeral_kind(&n.kind) {
            let tool = n
                .symbols
                .first()
                .cloned()
                .unwrap_or_else(|| "?".to_string());
            let month = n.created_at.get(0..7).unwrap_or("?").to_string();
            log_map
                .entry((n.kind.clone(), tool, month))
                .or_default()
                .push(n);
        } else {
            // (b) Near-duplicate durable rows: identical normalized content.
            dup_buckets
                .entry(normalize_content(&n.content))
                .or_default()
                .push(n);
        }
    }
    let mut log_groups: Vec<LogGroup> = log_map
        .into_iter()
        .filter(|(_, v)| v.len() >= 2)
        .map(|((kind, tool, month), members)| {
            let mut oc: BTreeMap<String, usize> = BTreeMap::new();
            for m in &members {
                *oc.entry(parse_outcome(&m.content).unwrap_or_else(|| "?".to_string()))
                    .or_default() += 1;
            }
            let mut outcomes: Vec<(String, usize)> = oc.into_iter().collect();
            outcomes.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
            LogGroup {
                kind,
                tool,
                month,
                members,
                outcomes,
            }
        })
        .collect();
    log_groups.sort_by_key(|g| std::cmp::Reverse(g.members.len()));
    let log_row_total: usize = log_groups.iter().map(|g| g.members.len()).sum();

    let mut dup_clusters: Vec<Vec<&NoteRow>> = dup_buckets
        .into_values()
        .filter(|v| v.len() > 1)
        .map(|mut v| {
            v.sort_by(|a, b| b.created_at.cmp(&a.created_at)); // newest first
            v
        })
        .collect();
    dup_clusters.sort_by_key(|c| std::cmp::Reverse(c.len()));

    // ---- SUPERSEDE candidates (durable kinds only) ----
    // A shared anchor ALONE over-nominates: ten unrelated decisions can live in
    // one broad file path. A real supersede pair also shares topic vocabulary,
    // so we keep only pairs whose content token-overlap clears the threshold.
    let mut by_anchor: BTreeMap<(String, String), Vec<&NoteRow>> = BTreeMap::new();
    for n in &notes {
        if is_ephemeral_kind(&n.kind) {
            continue;
        }
        for s in &n.symbols {
            by_anchor
                .entry((format!("sym:{s}"), n.kind.clone()))
                .or_default()
                .push(n);
        }
        for f in &n.files {
            by_anchor
                .entry((format!("file:{f}"), n.kind.clone()))
                .or_default()
                .push(n);
        }
    }
    // key = (newer_id, older_id) so a pair sharing two anchors is counted once,
    // keeping its best (highest-overlap) anchor as the witness.
    type PairWitness<'a> = (f32, String, String, &'a NoteRow, &'a NoteRow);
    let mut pair_best: BTreeMap<(String, String), PairWitness> = BTreeMap::new();
    for ((anchor, kind), v) in &by_anchor {
        let mut seen = HashSet::new();
        let mut distinct: Vec<&NoteRow> = v
            .iter()
            .copied()
            .filter(|n| seen.insert(n.id.clone()))
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        distinct.sort_by(|a, b| b.created_at.cmp(&a.created_at)); // newest first
        let newer = distinct[0];
        let newer_tok = token_set(&newer.content);
        for older in &distinct[1..] {
            let overlap = jaccard(&newer_tok, &token_set(&older.content));
            if overlap < SUPERSEDE_MIN_OVERLAP {
                continue;
            }
            let key = (newer.id.clone(), older.id.clone());
            let entry =
                pair_best
                    .entry(key)
                    .or_insert((0.0, anchor.clone(), kind.clone(), newer, older));
            if overlap > entry.0 {
                *entry = (overlap, anchor.clone(), kind.clone(), newer, older);
            }
        }
    }
    let mut supersede_pairs: Vec<PairWitness> = pair_best.into_values().collect();
    supersede_pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Prose claims a supersede/replacement but carries no link — retrieval can't
    // act on prose. Exactly what a text-only migration leaves behind.
    let prose_supersede: Vec<&NoteRow> = notes
        .iter()
        .filter(|n| n.supersedes.is_none() && mentions_supersede(&n.content))
        .collect();

    // ---- REFLECTION FIX-CHECK candidates ----
    // A reflection's staleness is a FIX event, not age or a duplicate: it stops
    // being true when the limitation it flags gets fixed. So we find active
    // reflections with an anchor (tool_name, else first symbol) whose code has
    // CHURNED since the reflection was written — those are the ones that might
    // now be resolved. No churn ⇒ not a candidate (nothing suggests a fix). Only
    // the model rules resolved-vs-still-relevant; we never retire on churn alone.
    // Requires running inside the git repo the reflections describe; outside one,
    // churn comes back empty and this section is silently absent.
    let want_reflection_fix = only.is_none() || only == Some(MoveKind::ReflectionFix);
    let mut reflection_fixes: Vec<ReflectionFixCandidate> = Vec::new();
    if want_reflection_fix {
        for n in &notes {
            if n.kind != "reflection" {
                continue;
            }
            let Some(anchor) = reflection_anchor(n) else {
                continue;
            };
            let churn = git_churn_since(&anchor, &n.created_at);
            if churn.is_empty() {
                continue;
            }
            reflection_fixes.push(ReflectionFixCandidate {
                note: n,
                anchor,
                churn,
            });
        }
        // Most churn first — likeliest to have addressed the limitation.
        reflection_fixes.sort_by_key(|c| std::cmp::Reverse(c.churn.len()));
    }

    // ---- kind filter (applies to consolidation candidates) ----
    let kind_ok = |k: &str| kind_filter.as_deref().map(|f| f == k).unwrap_or(true);
    log_groups.retain(|g| kind_ok(&g.kind));
    dup_clusters.retain(|c| c.first().map(|n| kind_ok(&n.kind)).unwrap_or(false));

    // ============ REPORT MODE ============
    if mode == Mode::Report {
        println!("# Note rationalization — candidates (dry run · no LLM · nothing written)");
        println!("# db {}", notes_db.display());
        println!("# {} active notes", notes.len());
        println!(
            "#   by kind:   {}",
            kind_hist
                .iter()
                .map(|(k, c)| format!("{k} {c}"))
                .collect::<Vec<_>>()
                .join(" · ")
        );
        println!(
            "#   by source: {}",
            source_hist
                .iter()
                .map(|(s, c)| format!("{s} {c}"))
                .collect::<Vec<_>>()
                .join(" · ")
        );
        println!();

        let consolidate_count = log_groups.len() + dup_clusters.len();
        println!(
            "## CONSOLIDATE — {consolidate_count} clusters (many rows → one distilled survivor)"
        );
        println!(
            "#  --distill previews the survivor the model would write; --apply --yes writes it"
        );
        println!("#  and retires the members with retired_by → survivor (kept + auditable).");
        if !log_groups.is_empty() {
            println!(
                "#  telemetry log: {log_row_total} rows across {} groups →",
                log_groups.len()
            );
            for g in log_groups.iter().take(20) {
                let (lo, hi) = date_span(&g.members);
                let oc = g
                    .outcomes
                    .iter()
                    .map(|(o, c)| format!("{o} {c}"))
                    .collect::<Vec<_>>()
                    .join(" · ");
                println!(
                    "  [{} · {} · {}]  ×{}   ({lo} → {hi})   {oc}",
                    g.kind,
                    g.tool,
                    g.month,
                    g.members.len()
                );
            }
            if log_groups.len() > 20 {
                println!("  … {} more telemetry groups", log_groups.len() - 20);
            }
        }
        if !dup_clusters.is_empty() {
            println!("#  near-duplicate durable notes (identical content) →");
            for c in &dup_clusters {
                let keep = c[0];
                println!("  [{}×] {}", c.len(), first_line(&keep.content));
                println!("       keep {}  ({})", keep.id, keep.created_at);
            }
        }
        if consolidate_count == 0 {
            println!("  (none)");
        }
        println!();

        println!(
            "## SUPERSEDE — {} candidate pairs (newer ↔ older, content overlap ≥ {:.0}%; LLM confirms)",
            supersede_pairs.len(),
            SUPERSEDE_MIN_OVERLAP * 100.0
        );
        println!("#  overlap-gated so unrelated notes sharing one path don't get nominated.");
        for (overlap, anchor, kind, newer, older) in supersede_pairs.iter().take(30) {
            println!("  [{kind}] {anchor}  overlap {:.0}%", overlap * 100.0);
            println!(
                "       newer {}  ({})  {}",
                newer.id,
                newer.created_at,
                first_line(&newer.content)
            );
            println!(
                "       older {}  ({})  {}",
                older.id,
                older.created_at,
                first_line(&older.content)
            );
        }
        if supersede_pairs.len() > 30 {
            println!("  … {} more pairs", supersede_pairs.len() - 30);
        }
        if supersede_pairs.is_empty() {
            println!("  (none)");
        }
        println!();

        println!(
            "## PROSE-ONLY SUPERSEDE — {} (claims supersession in text, no link)",
            prose_supersede.len()
        );
        for n in &prose_supersede {
            println!("  {}  [{}]  {}", n.id, n.kind, first_line(&n.content));
        }
        if prose_supersede.is_empty() {
            println!("  (none)");
        }
        println!();

        println!(
            "## REFLECTION FIX-CHECK — {} candidates (limitation flagged, anchor code churned since)",
            reflection_fixes.len()
        );
        println!("#  --distill asks the model RESOLVED vs STILL-RELEVANT; --apply --yes retires only RESOLVED.");
        for c in reflection_fixes.iter().take(30) {
            println!(
                "  [{}] {}  ({} commit(s) since {})",
                c.anchor,
                c.note.id,
                c.churn.len(),
                &c.note.created_at.get(0..10).unwrap_or("?")
            );
            println!("       {}", first_line(&c.note.content));
            println!("       ↳ {}", c.churn[0]);
        }
        if reflection_fixes.is_empty() {
            println!("  (none — run inside the code repo so churn can be detected)");
        }
        println!();

        println!(
            "Next: `--distill` previews the LLM-written survivors/verdicts; `--apply --yes` writes them + \
             the retire links. Scope with --only consolidate|supersede|reflections / --kind / --limit."
        );
        return 0;
    }

    // ============ DISTILL / APPLY MODE (LLM) ============
    let writing = mode == Mode::Apply && commit;
    let base_url = format!(
        "http://localhost:{}",
        crate::util::urls::DEFAULT_CLIENT_PORT
    );
    let client = reqwest::Client::new();

    eprintln!(
        "notes rationalize: {} via model '{model}' at {base_url} (limit {limit}/type){}",
        if writing {
            "APPLY (writing)"
        } else {
            "distill (preview only)"
        },
        if mode == Mode::Apply && !commit {
            " — no --yes, so PREVIEW only"
        } else {
            ""
        }
    );

    let do_consolidate = only.is_none() || only == Some(MoveKind::Consolidate);
    let do_supersede = only.is_none() || only == Some(MoveKind::Supersede);
    let do_reflection_fix = only.is_none() || only == Some(MoveKind::ReflectionFix);
    let mut wrote = 0usize;
    let mut retired = 0usize;

    // ---- CONSOLIDATE: telemetry log groups ----
    if do_consolidate {
        for g in log_groups.iter().take(limit) {
            let (lo, hi) = date_span(&g.members);
            let oc = g
                .outcomes
                .iter()
                .map(|(o, c)| format!("{o}={c}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "\n── CONSOLIDATE  [{} · {} · {}]  ×{}  ({lo}→{hi})  {oc}",
                g.kind,
                g.tool,
                g.month,
                g.members.len()
            );
            let (system, user) = consolidate_prompt(g, &lo, &hi);
            let survivor =
                match daemon_complete(&client, &base_url, &model, &system, &user, 400, 0.2).await {
                    Ok(s) => s.trim().to_string(),
                    Err(e) => {
                        eprintln!("   LLM error, skipping group: {e}");
                        continue;
                    }
                };
            println!("   survivor →\n     {}", survivor.replace('\n', "\n     "));
            if writing {
                match store
                    .write_note_with_source(
                        "reflection",
                        &survivor,
                        vec![g.tool.clone()],
                        vec![],
                        "cli-rationalize",
                        NoteScope::Global,
                        None,
                        None,
                        NoteSource::Agent,
                        None,
                    )
                    .await
                {
                    Ok(sid) => {
                        wrote += 1;
                        println!(
                            "   wrote survivor {sid}; retiring {} members →",
                            g.members.len()
                        );
                        for m in &g.members {
                            match store
                                .retire_by_id(&m.id, &format!("consolidated into {sid}"))
                                .await
                            {
                                Ok(true) => retired += 1,
                                Ok(false) => {}
                                Err(e) => eprintln!("   retire {} failed: {e}", m.id),
                            }
                        }
                    }
                    Err(e) => eprintln!("   write survivor failed: {e}"),
                }
            }
        }

        // ---- CONSOLIDATE: near-duplicate durable clusters (no LLM — keep newest) ----
        for c in dup_clusters.iter().take(limit) {
            let keep = c[0];
            println!(
                "\n── CONSOLIDATE (near-dup ×{})  keep {}  {}",
                c.len(),
                keep.id,
                first_line(&keep.content)
            );
            if writing {
                for old in &c[1..] {
                    match store
                        .retire_by_id(&old.id, &format!("duplicate of {}", keep.id))
                        .await
                    {
                        Ok(true) => {
                            retired += 1;
                            println!("   retired duplicate {}", old.id);
                        }
                        Ok(false) => {}
                        Err(e) => eprintln!("   retire {} failed: {e}", old.id),
                    }
                }
            }
        }
    }

    // ---- SUPERSEDE: LLM adjudicates each pair ----
    if do_supersede {
        for (overlap, anchor, kind, newer, older) in supersede_pairs.iter().take(limit) {
            println!(
                "\n── SUPERSEDE?  [{kind}] {anchor}  overlap {:.0}%",
                overlap * 100.0
            );
            println!("   newer {}  {}", newer.id, first_line(&newer.content));
            println!("   older {}  {}", older.id, first_line(&older.content));
            let (system, user) = supersede_prompt(newer, older);
            let verdict =
                match daemon_complete(&client, &base_url, &model, &system, &user, 120, 0.0).await {
                    Ok(s) => s.trim().to_string(),
                    Err(e) => {
                        eprintln!("   LLM error, skipping pair: {e}");
                        continue;
                    }
                };
            let overtakes = verdict.to_uppercase().contains("OVERTAKES");
            println!("   verdict → {verdict}");
            if writing && overtakes {
                let reason = format!(
                    "superseded by {} (rationalize: {})",
                    newer.id,
                    first_line(&verdict)
                );
                match store.retire_by_id(&older.id, &reason).await {
                    Ok(true) => {
                        retired += 1;
                        println!("   retired older {} → newer {}", older.id, newer.id);
                    }
                    Ok(false) => {}
                    Err(e) => eprintln!("   retire {} failed: {e}", older.id),
                }
            }
        }
    }

    // ---- REFLECTION FIX-CHECK: LLM rules whether the limitation is resolved ----
    if do_reflection_fix {
        for c in reflection_fixes.iter().take(limit) {
            println!(
                "\n── REFLECTION FIX-CHECK?  [{}] {}  ({} commit(s) since {})",
                c.anchor,
                c.note.id,
                c.churn.len(),
                &c.note.created_at.get(0..10).unwrap_or("?")
            );
            println!("   limitation: {}", first_line(&c.note.content));
            for line in c.churn.iter().take(4) {
                println!("   churn: {line}");
            }
            let (system, user) = reflection_fix_prompt(c);
            let verdict =
                match daemon_complete(&client, &base_url, &model, &system, &user, 120, 0.0).await {
                    Ok(s) => s.trim().to_string(),
                    Err(e) => {
                        eprintln!("   LLM error, skipping reflection: {e}");
                        continue;
                    }
                };
            // First token is the verdict (prompt forces RESOLVED / STILL-RELEVANT);
            // starts_with avoids matching a trailing "...not resolved".
            let resolved = verdict.trim_start().to_uppercase().starts_with("RESOLVED");
            println!("   verdict → {verdict}");
            if writing && resolved {
                let reason = format!("fixed — {} (rationalize fix-check)", first_line(&verdict));
                match store.retire_by_id(&c.note.id, &reason).await {
                    Ok(true) => {
                        retired += 1;
                        println!("   retired reflection {} (limitation resolved)", c.note.id);
                    }
                    Ok(false) => {}
                    Err(e) => eprintln!("   retire {} failed: {e}", c.note.id),
                }
            }
        }
    }

    if writing {
        println!(
            "\n✓ applied: wrote {wrote} survivor(s), retired {retired} note(s).\n  \
             Reach: retirements are LOCAL to this node (retired_at doesn't gossip — that's \
             deliberate; tombstone is the propagating primitive, and we chose auditable retire). \
             The survivors are global notes, so the distilled SIGNAL gossips to peers; run \
             rationalize on each node to clean its own raw rows."
        );
    } else {
        println!(
            "\nPreview only — nothing written.{}",
            if mode == Mode::Apply {
                " Re-run with --yes to commit these writes."
            } else {
                " Re-run with --apply --yes to commit."
            }
        );
    }
    0
}

/// System + user prompt to distill one telemetry-log group into a durable note.
fn consolidate_prompt(g: &LogGroup, lo: &str, hi: &str) -> (String, String) {
    let system = "You distill a batch of low-signal operational log entries into ONE durable \
                  note for a shared engineering knowledge base. Output ONLY the note text: \
                  2-4 sentences, factual, no preamble, no markdown headers. State the totals \
                  and the outcome breakdown, and call out anything actionable (e.g. an \
                  elevated failure rate and its likely cause)."
        .to_string();
    let total: usize = g.members.len();
    let breakdown = g
        .outcomes
        .iter()
        .map(|(o, c)| {
            let pct = (*c as f32 / total as f32 * 100.0).round() as i32;
            format!("{o}: {c} ({pct}%)")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let samples = g
        .members
        .iter()
        .take(6)
        .map(|m| format!("- {}", truncate(&m.content, 200)))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "These {total} '{}' log entries are for tool '{}' during {} ({lo} → {hi}).\n\
         Outcome breakdown: {breakdown}.\n\
         Sample entries:\n{samples}\n\n\
         Write the single distilled note capturing this period's activity and health.",
        g.kind, g.tool, g.month
    );
    (system, user)
}

/// System + user prompt to adjudicate whether `newer` supersedes `older`.
fn supersede_prompt(newer: &NoteRow, older: &NoteRow) -> (String, String) {
    let system = "You decide whether a newer engineering note OVERTAKES an older one (makes it \
                  stale / contradicts / fully replaces it) or whether they are COMPLEMENTARY \
                  (both still true, different facets). Answer with exactly one word — OVERTAKES \
                  or COMPLEMENTARY — then a dash and at most 12 words of reason. Default to \
                  COMPLEMENTARY when unsure."
        .to_string();
    let user = format!(
        "NEWER ({}): {}\n\nOLDER ({}): {}\n\nDoes NEWER overtake OLDER?",
        newer.created_at,
        truncate(&newer.content, 600),
        older.created_at,
        truncate(&older.content, 600)
    );
    (system, user)
}

/// The anchor a reflection is "about" — the join key for the fix-check. Prefer
/// the explicit `tool_name` (session_reflection sets it); fall back to the first
/// symbol. `None` for anchorless general reflections, which can't be fix-checked.
fn reflection_anchor(n: &NoteRow) -> Option<String> {
    // 1. The explicit column — the common, clean path.
    if let Some(t) = n
        .tool_name
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Some(t.to_string());
    }
    // 2. First symbol.
    if let Some(s) = n.symbols.iter().map(|s| s.trim()).find(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    // 3. Recover from the `session_reflection` JSON body, where an agent
    //    sometimes names the tool inside the payload instead of the column.
    //    Prefer an explicit `tool_name`, then the first `tools_that_helped`
    //    entry. Commit-message reflections (source=committed) aren't JSON and
    //    fall through to None — correctly, since they have no tool anchor.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(n.content.trim()) {
        if let Some(t) = v
            .get("tool_name")
            .and_then(|t| t.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return Some(t.to_string());
        }
        if let Some(t) = v
            .get("tools_that_helped")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|t| t.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return Some(t.to_string());
        }
    }
    None
}

/// Commit subjects that changed lines matching `anchor` since `since_rfc3339`,
/// via `git log` pickaxe in the current repo. Empty when there's no churn, git
/// is unavailable, or we're not inside a repo — all of which correctly mean "no
/// fix-signal, not a candidate". Capped so a hot anchor doesn't flood.
///
/// Precision: for identifier-shaped anchors we use a word-bounded regex (`-G`)
/// so `symbols` no longer matches `symbolstable` and `build` no longer matches
/// `rebuild` — the generic-substring over-matching that flooded the candidate
/// set with commits that merely touched the term in passing. Non-word anchors
/// (containing `::`, `<`, …) fall back to the literal-substring pickaxe (`-S`),
/// where a `\b`-wrapped regex would be both unsafe and ill-defined. Anchors
/// under 3 chars pickaxe against nearly every diff, so they yield no signal.
fn git_churn_since(anchor: &str, since_rfc3339: &str) -> Vec<String> {
    let anchor = anchor.trim();
    if anchor.len() < 3 {
        return Vec::new();
    }
    let is_word = anchor.chars().all(|c| c.is_alphanumeric() || c == '_');
    let needle = if is_word {
        format!("-G\\b{anchor}\\b")
    } else {
        format!("-S{anchor}")
    };
    let output = std::process::Command::new("git")
        .args([
            "log",
            &format!("--since={since_rfc3339}"),
            "-n",
            "8",
            "--no-merges",
            "--pretty=format:%h %s",
            &needle,
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// System + user prompt to rule whether recent code churn resolved a reflection's
/// flagged limitation.
fn reflection_fix_prompt(c: &ReflectionFixCandidate) -> (String, String) {
    let system =
        "You decide whether recent code changes RESOLVED a limitation an engineer flagged \
                  about a tool. Answer with exactly one word — RESOLVED or STILL-RELEVANT — then a \
                  dash and at most 12 words. Default to STILL-RELEVANT unless the commits clearly \
                  address the SPECIFIC limitation described (a commit merely touching the tool is \
                  not enough)."
            .to_string();
    let churn = c
        .churn
        .iter()
        .take(8)
        .map(|s| format!("- {}", truncate(s, 160)))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "LIMITATION (about '{}', flagged {}):\n{}\n\nCommits changing '{}' since then:\n{churn}\n\n\
         Do these resolve the specific limitation?",
        c.anchor,
        &c.note.created_at.get(0..10).unwrap_or("?"),
        truncate(&c.note.content, 800),
        c.anchor,
    );
    (system, user)
}

/// POST an OpenAI-style chat completion to the daemon and return the assistant
/// text. `model="primary"` resolves to the primary slot (never the fast slot).
async fn daemon_complete(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
    temperature: f32,
) -> Result<String, String> {
    let url = format!("{base_url}/v1/chat/completions");
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": max_tokens,
        "temperature": temperature,
        "stream": false,
    });
    let resp = client.post(&url).json(&body).send().await.map_err(|e| {
        if e.is_connect() {
            format!("daemon not reachable at {url} — start it with `svrn daemon run`")
        } else {
            format!("request failed: {e}")
        }
    })?;
    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("daemon returned {code}: {}", truncate(&text, 300)));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("decode: {e}"))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "response had no choices[0].message.content".to_string())
}

/// Truncate to `max` chars with an ellipsis, for bounding prompt/log size.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Parse the outcome token out of a tool-decision note's content, which has the
/// shape `"{tool} → {outcome} — {reasoning}"` (see
/// `sovereign_core::memory::write_tool_decision`). Returns `None` when the
/// arrow shape is absent. The outcome (e.g. `useful`, `no-results`) may contain
/// hyphens, so we split only on the spaced ` — ` / ` - ` separator, never a
/// bare `-`.
fn parse_outcome(content: &str) -> Option<String> {
    let after = content.split_once(" → ")?.1;
    let cut = after.find(" — ").or_else(|| after.find(" - "));
    let outcome = match cut {
        Some(idx) => &after[..idx],
        None => after,
    }
    .trim();
    (!outcome.is_empty()).then(|| outcome.chars().take(24).collect())
}

/// Content token set for overlap scoring: lowercased words of length >= 4
/// (drops most stopwords and punctuation-glue without a stopword list). Used
/// only to gate supersede candidates, so a coarse set is fine.
fn token_set(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(|w| w.to_string())
        .collect()
}

/// Jaccard overlap of two token sets: |A ∩ B| / |A ∪ B|, in [0.0, 1.0].
/// Near-identical re-emitted notes score high; unrelated notes that merely
/// share a file path score low. Empty-vs-anything is 0.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    }
}

/// The earliest and latest `created_at` date (YYYY-MM-DD) across a cluster.
fn date_span(v: &[&NoteRow]) -> (String, String) {
    let day = |n: &NoteRow| n.created_at.get(0..10).unwrap_or("?").to_string();
    let mut lo = day(v[0]);
    let mut hi = lo.clone();
    for n in v {
        let d = day(n);
        if d < lo {
            lo = d.clone();
        }
        if d > hi {
            hi = d;
        }
    }
    (lo, hi)
}

/// Normalize note content for near-duplicate bucketing: collapse whitespace,
/// lowercase, and keep a stable prefix. Two notes that differ only in trailing
/// detail or formatting land in the same bucket; genuinely different notes do
/// not. Deliberately lossy — this is a candidate generator, not a proof.
fn normalize_content(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .chars()
        .take(160)
        .collect()
}

/// Does the prose claim to replace/reverse another note? Lexical only.
fn mentions_supersede(s: &str) -> bool {
    let lc = s.to_lowercase();
    lc.contains("supersede") || lc.contains("supersedes the") || lc.contains("replaces the prior")
}

/// First non-empty line of a note, truncated for the report.
fn first_line(s: &str) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > 84 {
        format!("{}…", line.chars().take(84).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod read_back_tests {
    use super::*;

    // The regression these pin: `svrn notes add` wrote notes the CLI then had
    // no way to read back. Bare `notes` renders the reflection SUMMARY, and
    // `--query` was not a flag at all — it printed "Unknown flag: --query"
    // and fell back to `svrn reflect` help. Only the MCP `notes` tool could
    // search. Caught 2026-07-28 by the `agent-notes` journey.

    #[test]
    fn a_search_flag_routes_to_the_list_view() {
        for flag in ["--query", "-q", "--symbol", "--file", "--id"] {
            assert!(is_list_flag(flag), "{flag} must reach the list view");
        }
        // `--flag=value` is the form cli-contract.toml steps use.
        assert!(is_list_flag("--query=grounding"));
    }

    #[test]
    fn reflection_filters_keep_meaning_the_reflection_view() {
        // `--since` / `--tool` are long-standing reflection-view filters.
        // Routing them to the list would silently change a view engineers
        // rely on — a worse bug than the one being fixed.
        for flag in ["--since", "--tool", "--source", "--feature"] {
            assert!(!is_list_flag(flag), "{flag} must stay on the reflection view");
        }
    }

    #[test]
    fn an_id_shaped_query_is_recognised_as_an_id() {
        // The note-injection hooks quote ids at exactly this call site
        // ("full note: `sovereign notes --query <id>`"), and FTS has no
        // useful term for a UUID fragment.
        assert!(looks_like_note_id("625ca452"));
        assert!(looks_like_note_id("f496d39e-20a6-422c-adbf-b3c40b5b14eb"));
    }

    #[test]
    fn ordinary_searches_are_not_hijacked_as_ids() {
        assert!(!looks_like_note_id("cafe"), "too short to risk it");
        assert!(!looks_like_note_id("grounding gate"));
        assert!(!looks_like_note_id("EmbedRouter"));
        assert!(!looks_like_note_id(""));
        // Hex-looking but past UUID length is prose, not an id.
        assert!(!looks_like_note_id(&"a".repeat(40)));
    }
}

#[cfg(test)]
mod rationalize_tests {
    use super::*;

    #[test]
    fn parse_outcome_reads_the_token_between_arrow_and_dash() {
        assert_eq!(
            parse_outcome("knowledge_lookup → useful — synthesised over 12 chunks").as_deref(),
            Some("useful")
        );
        // Hyphenated outcomes survive (we split on spaced separators, not bare '-').
        assert_eq!(
            parse_outcome("knowledge_lookup → no-results — retrieval returned 12 chunks")
                .as_deref(),
            Some("no-results")
        );
        // No arrow → not a tool-decision-shaped note.
        assert_eq!(parse_outcome("a normal decision note"), None);
    }

    #[test]
    fn jaccard_scores_reemitted_notes_high_and_unrelated_low() {
        let a = token_set("Three or more code-intel calls symbols callers with no build follow-up");
        let reemit = token_set("Three or more code-intel calls symbols with no build follow-up");
        let unrelated = token_set("runtime.rs was decomposed into helper and handler modules");
        assert!(
            jaccard(&a, &reemit) >= 0.5,
            "re-emitted note should score high"
        );
        assert!(
            jaccard(&a, &unrelated) < 0.4,
            "unrelated note should score low"
        );
        // Empty vs anything is 0 (no false pairing on contentless notes).
        assert_eq!(jaccard(&HashSet::new(), &a), 0.0);
    }

    #[test]
    fn token_set_drops_short_glue_words() {
        let t = token_set("the a of runtime decomposition");
        assert!(t.contains("runtime"));
        assert!(t.contains("decomposition"));
        assert!(!t.contains("the"), "3-char and shorter words are dropped");
        assert!(!t.contains("of"));
    }

    #[test]
    fn is_log_kind_flags_telemetry_not_durable_knowledge() {
        assert!(is_ephemeral_kind("tool_decision"));
        assert!(is_ephemeral_kind("checkpoint"));
        assert!(!is_ephemeral_kind("decision"));
        assert!(!is_ephemeral_kind("invariant"));
    }

    #[test]
    fn git_churn_since_respects_the_since_bound() {
        // Runs inside the repo, but nothing has been committed since the year
        // 2099 — so the fix-signal is empty regardless of anchor. Uses a
        // >=3-char anchor so it exercises the real git path, not the
        // short-anchor floor below.
        assert!(git_churn_since("runtime", "2099-01-01").is_empty());
    }

    #[test]
    fn git_churn_since_floors_out_tiny_anchors() {
        // A 1-2 char anchor pickaxes against nearly every diff — no signal.
        // Returns empty without even shelling out to git.
        assert!(git_churn_since("fn", "1970-01-01").is_empty());
        assert!(git_churn_since("x", "1970-01-01").is_empty());
    }

    /// Minimal `NoteRow` for anchor tests — only the three fields
    /// `reflection_anchor` reads vary; the rest are inert placeholders.
    fn anchor_row(tool_name: Option<&str>, symbols: &[&str], content: &str) -> NoteRow {
        NoteRow {
            id: String::new(),
            kind: "reflection".to_string(),
            content: content.to_string(),
            symbols: symbols.iter().map(|s| s.to_string()).collect(),
            files: Vec::new(),
            session_id: String::new(),
            created_at: String::new(),
            tool_name: tool_name.map(str::to_string),
            retired_at: None,
            retired_by: None,
            scope: "global".to_string(),
            feature_id: None,
            promoted_from: None,
            related_entity: None,
            source: "agent".to_string(),
            supersedes: None,
            payload_json: None,
        }
    }

    #[test]
    fn reflection_anchor_recovers_tool_from_json_body() {
        // Explicit tool_name in the JSON body when the column is empty.
        let base = anchor_row(
            None,
            &[],
            r#"{"task_summary":"x","tool_name":"blast","tools_that_helped":["callers"]}"#,
        );
        assert_eq!(reflection_anchor(&base).as_deref(), Some("blast"));

        // tool_name null in the body → fall back to first tools_that_helped.
        let helped = anchor_row(
            None,
            &[],
            r#"{"task_summary":"x","tool_name":null,"tools_that_helped":["lint_status","build"]}"#,
        );
        assert_eq!(reflection_anchor(&helped).as_deref(), Some("lint_status"));

        // A prose commit-message reflection isn't JSON → no anchor.
        let prose = anchor_row(
            None,
            &[],
            "docs(uap): skeptic-proof the demo — reconciled numbers",
        );
        assert!(reflection_anchor(&prose).is_none());

        // The explicit column still wins over the body.
        let column = anchor_row(Some("symbols"), &[], r#"{"tool_name":"blast"}"#);
        assert_eq!(reflection_anchor(&column).as_deref(), Some("symbols"));
    }

    #[test]
    fn truncate_bounds_length_and_is_char_safe() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 3), "abc…");
        // Counts chars, not bytes — must not panic on a multibyte boundary.
        assert_eq!(truncate("→→→→", 2), "→→…");
    }
}
