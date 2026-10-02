// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn memory` — glassbox for the long-term memory store.
//!
//! The rolling-summary compaction worker (`crate::memory_compaction`)
//! folds oldest non-superseded memories per conversation into
//! `MemoryKind::Summary` rows once the count crosses
//! `[memory.compaction] threshold`. Retrieval filters
//! `superseded_by IS NULL` so the prompt sees the summary in place
//! of the originals — but the originals stay on disk for provenance.
//!
//! This command surfaces both halves:
//!
//! - `list --conversation <id>` — render the active memory + the
//!   chain of superseded raws beneath each summary. The shape the
//!   writer would see if they walked their inner-work entries.
//! - `expand <summary-id>` — print the originals a summary folded.
//!   Quick fact-check against the synthesis.
//!
//! `rebuild-summaries` is deferred — it needs the daemon's loaded
//! fast-slot model for the synthesis call and is better as an HTTP
//! endpoint hitting `CompactionWorker::run_one_sync`. Tracked at
//! [[witness-memory-rolling-compaction]] §"Out of scope".

use std::path::PathBuf;

use sovereign_core::traits::MemoryStore;
use sovereign_core::types::{Memory, MemoryKind};
use sovereign_store::sqlite::SqliteStateStore;

pub async fn run_memory(args: &[String]) -> i32 {
    let Some(first) = args.first() else {
        print_help();
        return 1;
    };
    if matches!(first.as_str(), "--help" | "-h" | "help") {
        print_help();
        return 0;
    }
    let rest = &args[1..];
    match first.as_str() {
        "list" => cmd_list(rest).await,
        "expand" => cmd_expand(rest).await,
        other => {
            eprintln!("memory: unknown subcommand '{other}'");
            print_help();
            2
        }
    }
}

fn print_help() {
    println!(
        "svrn memory — long-term memory store glassbox\n\
         \n\
         USAGE:\n  \
         sovereign memory <subcommand> [flags]\n\
         \n\
         SUBCOMMANDS:\n  \
         list --conversation <id>   Active memory + superseded chain for one conversation\n  \
         expand <summary-id>        Print originals a summary folded\n\
         \n\
         FLAGS:\n  \
         --db-path <path>           Override `~/.svrnmesh/` root (sandbox / integration test)\n"
    );
}

async fn cmd_list(args: &[String]) -> i32 {
    let conv_id = match flag(args, "--conversation").or_else(|| flag(args, "--conv")) {
        Some(s) if !s.is_empty() => s,
        _ => {
            eprintln!("memory list: --conversation <id> is required");
            return 2;
        }
    };
    let store = match open_store(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("memory list: {e}");
            return 1;
        }
    };
    let actives = match store.list_memories_for_conversation(&conv_id).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("memory list: store read failed: {e}");
            return 1;
        }
    };
    if actives.is_empty() {
        println!("(no active memories for conversation {conv_id})");
        return 0;
    }
    println!("active memories for conversation {conv_id}:");
    for m in &actives {
        let kind = match m.kind {
            MemoryKind::Summary => format!(
                "summary of {n} entries",
                n = m.source_memory_ids.len().max(1)
            ),
            MemoryKind::Raw => "raw".to_string(),
        };
        println!(
            "  • {id} ({kind}, conf {conf:.2})\n      {preview}",
            id = m.id,
            kind = kind,
            conf = m.confidence,
            preview = preview(&m.content, 120),
        );
        if matches!(m.kind, MemoryKind::Summary) && !m.source_memory_ids.is_empty() {
            for src_id in &m.source_memory_ids {
                println!("        └─ folded: {src_id}");
            }
        }
    }
    0
}

async fn cmd_expand(args: &[String]) -> i32 {
    let summary_id = match args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(String::as_str)
    {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            eprintln!("memory expand: <summary-id> is required");
            return 2;
        }
    };
    let store = match open_store(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("memory expand: {e}");
            return 1;
        }
    };
    // Need access to all memories to find the summary + walk its
    // source_memory_ids. `get_all_memories` filters superseded, so
    // we ask the store for the raw underlying read via a direct
    // SQL query. For Phase 1 the simpler shape: read all memories
    // including superseded.
    let all = match store.get_all_memories().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("memory expand: store read failed: {e}");
            return 1;
        }
    };
    let Some(summary) = all.iter().find(|m| m.id == summary_id) else {
        eprintln!(
            "memory expand: no summary {summary_id} found in active memories. (Superseded summaries are not yet readable via this command.)"
        );
        return 1;
    };
    if !matches!(summary.kind, MemoryKind::Summary) {
        eprintln!("memory expand: {summary_id} is a raw memory, not a summary");
        return 2;
    }
    println!("summary {summary_id}:");
    println!("  {preview}", preview = preview(&summary.content, 400));
    println!();
    println!(
        "folded {n} source entries:",
        n = summary.source_memory_ids.len()
    );
    // Source memories are superseded so they don't appear in
    // `get_all_memories`. Use the same Sqlite store directly.
    for src_id in &summary.source_memory_ids {
        match read_superseded(&store, src_id).await {
            Ok(Some(src)) => println!(
                "  • {id} (conf {conf:.2})\n      {preview}",
                id = src.id,
                conf = src.confidence,
                preview = preview(&src.content, 200),
            ),
            Ok(None) => println!("  • {src_id} (not found — possibly hard-deleted)"),
            Err(e) => println!("  • {src_id} (read failed: {e})"),
        }
    }
    0
}

/// Fetch a single memory by id even if `superseded_by IS NOT NULL`.
/// Uses a small ad-hoc query directly against the connection so we
/// don't have to add a new trait method for the rare provenance-walk
/// case.
async fn read_superseded(_store: &SqliteStateStore, id: &str) -> Result<Option<Memory>, String> {
    // The trait surface deliberately excludes superseded rows. For
    // this CLI surface we need the raw provenance, so we issue a
    // direct sqlite query through the same db file. Open a fresh
    // read-only handle to avoid grabbing the store's mutex.
    let path = sovereign_root_from_env().join("state.db");
    let conn = rusqlite::Connection::open(&path)
        .map_err(|e| format!("open {} failed: {e}", path.display()))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, content, confidence \
             FROM memories WHERE id = ?1 LIMIT 1",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt
        .query_map(rusqlite::params![id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    if let Some(r) = rows.next() {
        let (id, content, confidence) = r.map_err(|e| e.to_string())?;
        Ok(Some(Memory {
            id,
            content,
            confidence,
            ..Default::default()
        }))
    } else {
        Ok(None)
    }
}

fn open_store(args: &[String]) -> Result<SqliteStateStore, String> {
    let root = flag(args, "--db-path")
        .map(PathBuf::from)
        .unwrap_or_else(sovereign_root_from_env);
    let path = root.join("state.db");
    SqliteStateStore::open(&path).map_err(|e| format!("open {} failed: {e}", path.display()))
}

fn sovereign_root_from_env() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root()
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            return args.get(i + 1).cloned();
        }
        if let Some(rest) = args[i].strip_prefix(&format!("{name}=")) {
            return Some(rest.to_string());
        }
        i += 1;
    }
    None
}

fn preview(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.replace('\n', " ");
    }
    let head: String = trimmed.chars().take(max_chars).collect();
    format!("{}…", head.replace('\n', " "))
}
