//! `sovereign reflect` — developer-facing session reflection summary.
//!
//! Reads accumulated reflections and tool call logs from the notes database
//! and prints a prioritised improvement backlog. Agents write structured
//! reflections at task completion via the `session_reflection` MCP tool;
//! the developer reads them here to decide what to improve.
//!
//! ## Usage
//!
//! ```text
//! sovereign reflect                              # 30-day summary (default)
//! sovereign reflect --since 7d                  # narrow time window
//! sovereign reflect --since 90d                 # widen time window
//! sovereign reflect --tool blast_radius         # filter to one tool
//! sovereign reflect --raw                       # full reflection prose, ungrouped
//! sovereign reflect --todos                     # open todo notes only
//! sovereign reflect --log                       # raw tool_call_log patterns
//! sovereign reflect --history                   # include retired reflections
//! sovereign reflect --retire --tool blast_radius --reason "fixed in PR #88"
//! sovereign reflect --retire --id <uuid>        --reason "fixed in PR #88"
//! ```

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use corpus_engine_notes::{NoteRow, NoteStore, ToolCallLogRow};

/// Old top-level `sovereign reflect` entry point. Prints the
/// deprecation banner and forwards to the canonical view handler.
/// `sovereign notes` (the new name) calls [`run_reflect_view`]
/// directly so it doesn't trigger the banner.
pub async fn run_reflect(args: &[String]) -> i32 {
    crate::util::deprecation::announce("sovereign reflect", "sovereign notes");
    run_reflect_view(args).await
}

/// Canonical reflection-view handler. Both the legacy `reflect`
/// alias and the new `sovereign notes` default view forward here.
pub(crate) async fn run_reflect_view(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    // ── Arg parsing ─────────────────────────────────────────────────────────
    let mut since_days: u64 = 30;
    let mut raw = false;
    let mut tool_filter: Option<String> = None;
    let mut todos_only = false;
    let mut show_log = false;
    let mut include_history = false;
    let mut retire_mode = false;
    let mut retire_id: Option<String> = None;
    let mut retire_reason: Option<String> = None;
    let mut yes = false;
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    since_days = parse_duration(v).unwrap_or(30);
                }
            }
            "--raw" => raw = true,
            "--tool" => {
                i += 1;
                tool_filter = args.get(i).cloned();
            }
            "--todos" => todos_only = true,
            "--log" => show_log = true,
            "--history" => include_history = true,
            "--retire" => retire_mode = true,
            "--id" => {
                i += 1;
                retire_id = args.get(i).cloned();
            }
            "--reason" => {
                i += 1;
                retire_reason = args.get(i).cloned();
            }
            "--yes" => yes = true,
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("Unknown flag: {other}");
                crate::util::help::print(&HELP);
                return 1;
            }
        }
        i += 1;
    }

    // ── Find notes.db ────────────────────────────────────────────────────────
    let notes_db = match find_notes_db(data_dir.as_deref()) {
        Some(p) => p,
        None => {
            eprintln!(
                "error: could not find notes.db — run `sovereign project serve` at least once, \
                 or pass --data-dir <path> to the directory containing notes.db"
            );
            return 1;
        }
    };

    let store = match NoteStore::open(&notes_db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not open {}: {e}", notes_db.display());
            return 1;
        }
    };

    // ── Retirement mode ──────────────────────────────────────────────────────
    if retire_mode {
        return run_retire(&store, tool_filter.as_deref(), retire_id.as_deref(), retire_reason.as_deref(), yes).await;
    }

    let since_ts = unix_now() - (since_days * 86_400) as i64;

    // ── --todos ──────────────────────────────────────────────────────────────
    if todos_only {
        return run_todos(&store).await;
    }

    // ── --log ────────────────────────────────────────────────────────────────
    if show_log {
        return run_log(&store, since_ts).await;
    }

    // ── Main summary ─────────────────────────────────────────────────────────
    run_summary(&store, since_ts, tool_filter.as_deref(), raw, include_history).await
}

// ─── Summary ──────────────────────────────────────────────────────────────────

async fn run_summary(
    store: &NoteStore,
    since_ts: i64,
    tool_filter: Option<&str>,
    raw: bool,
    include_history: bool,
) -> i32 {
    let reflections = match store.read_reflections(since_ts, tool_filter, include_history).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error reading reflections: {e}");
            return 1;
        }
    };

    let log_rows = store.tool_call_log_rows(since_ts, 50_000).await.unwrap_or_default();
    let todos = store.open_todos(5).await.unwrap_or_default();

    // Count sessions.
    let all_sessions: std::collections::HashSet<&str> =
        reflections.iter().map(|n| n.session_id.as_str()).collect();
    let active_count = reflections.iter().filter(|n| n.retired_at.is_none()).count();
    let retired_count = reflections.iter().filter(|n| n.retired_at.is_some()).count();
    let period_label = format!("last {} days", (unix_now() - since_ts) / 86_400);

    println!();
    println!("Code Intelligence — Session Reflection Summary");
    println!("{}", "━".repeat(50));
    println!(
        "Sessions analysed: {}     Reflections recorded: {}     Retired: {}     Period: {}",
        all_sessions.len(),
        active_count,
        retired_count,
        period_label
    );

    if raw {
        return print_raw(&reflections);
    }

    // ── IMPROVEMENT SIGNALS ──────────────────────────────────────────────────
    println!();
    println!("IMPROVEMENT SIGNALS (ranked by frequency)");
    println!("{}", "─".repeat(46));

    // Group active reflections by tool_name.
    let mut signals: HashMap<String, Vec<&NoteRow>> = HashMap::new();
    for r in reflections.iter().filter(|n| n.retired_at.is_none()) {
        let tool = r
            .tool_name
            .clone()
            .or_else(|| extract_tool_from_content(&r.content))
            .unwrap_or_else(|| "(unknown)".to_string());
        signals.entry(tool).or_default().push(r);
    }

    // Compute inferred patterns from tool_call_log.
    let inferred = infer_patterns(&log_rows);

    // Collect all signal keys and sort by session count descending.
    let mut signal_keys: Vec<&String> = signals.keys().collect();
    signal_keys.sort_by(|a, b| {
        let ca = count_sessions(signals[*a].iter().copied());
        let cb = count_sessions(signals[*b].iter().copied());
        cb.cmp(&ca)
    });

    // Also include purely inferred signals (no reflection text) that aren't already covered.
    let all_inferred_tools: Vec<String> = inferred.keys().cloned().collect();

    let mut combined_tools: Vec<String> = signal_keys.iter().map(|s| (*s).clone()).collect();
    for t in &all_inferred_tools {
        if !signals.contains_key(t) {
            combined_tools.push(t.clone());
        }
    }

    if combined_tools.is_empty() {
        println!("  (no signals in this period — great!)");
    }

    for tool in &combined_tools {
        let session_count = signals
            .get(tool)
            .map(|rows| count_sessions(rows.iter().copied()))
            .unwrap_or(0);

        if session_count > 0 {
            println!();
            println!("[{session_count} session{}] {tool}", if session_count == 1 { "" } else { "s" });

            if let Some(rows) = signals.get(tool) {
                for note in rows.iter().take(5) {
                    let snippets = extract_negative_snippets(&note.content);
                    for snippet in snippets.iter().take(2) {
                        if !snippet.is_empty() {
                            println!("  \"{}\"", truncate(snippet, 120));
                        }
                    }
                    if include_history && note.retired_at.is_some() {
                        if let Some(reason) = &note.retired_by {
                            println!("  [RETIRED: {}]", reason);
                        }
                    }
                }
            }
        }

        if let Some(patterns) = inferred.get(tool) {
            for (pattern, count) in patterns {
                println!("  → Inferred from log: {} (×{})", pattern, count);
            }
        }
    }

    // ── WHAT HELPED ──────────────────────────────────────────────────────────
    let mut helped_counts: HashMap<String, usize> = HashMap::new();
    for note in reflections.iter().filter(|n| n.retired_at.is_none()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&note.content) {
            if let Some(arr) = v.get("tools_that_helped").and_then(|v| v.as_array()) {
                for tool in arr {
                    if let Some(name) = tool.as_str() {
                        *helped_counts.entry(name.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    if !helped_counts.is_empty() {
        println!();
        println!("WHAT HELPED");
        println!("{}", "─".repeat(15));

        let mut sorted: Vec<(&String, &usize)> = helped_counts.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        let max = sorted.first().map(|(_, &c)| c).unwrap_or(1);

        for (tool, count) in sorted.iter().take(10) {
            let bar_len = ((*count * 12) / max).max(1);
            let bar = "█".repeat(bar_len);
            println!("  {:<22} {}  {} sessions", tool, bar, count);
        }
    }

    // ── OPEN TODOS ────────────────────────────────────────────────────────────
    if !todos.is_empty() {
        println!();
        println!("OPEN TODOS FROM AGENT SESSIONS");
        println!("{}", "─".repeat(33));
        for t in &todos {
            let preview = truncate(&t.content, 80);
            println!("  [todo] {preview}");
        }
        println!("  Run `sovereign reflect --todos` to see full list with context.");
    }

    // ── Footer ────────────────────────────────────────────────────────────────
    println!();
    println!("Run `sovereign reflect --raw` to see full reflection text.");
    println!("Run `sovereign reflect --since 7d` to narrow the period.");
    if !include_history {
        println!("Run `sovereign reflect --history` to include retired reflections.");
    }

    0
}

fn print_raw(reflections: &[NoteRow]) -> i32 {
    println!();
    println!("RAW REFLECTIONS");
    println!("{}", "─".repeat(17));
    if reflections.is_empty() {
        println!("  (none in this period)");
        return 0;
    }
    for note in reflections {
        println!();
        println!("  [{}] {} — session {}", note.kind, note.created_at, &note.session_id[..8.min(note.session_id.len())]);
        if let Some(ref tool) = note.tool_name {
            println!("  tool: {tool}");
        }
        if note.retired_at.is_some() {
            println!("  [RETIRED: {}]", note.retired_by.as_deref().unwrap_or("no reason"));
        }
        println!("  {}", note.content);
    }
    0
}

// ─── Todos ────────────────────────────────────────────────────────────────────

async fn run_todos(store: &NoteStore) -> i32 {
    let todos = store.open_todos(50).await.unwrap_or_default();
    if todos.is_empty() {
        println!("No open todo notes.");
        return 0;
    }
    println!();
    println!("OPEN TODOS ({} total)", todos.len());
    println!("{}", "─".repeat(15));
    for t in &todos {
        println!();
        println!("  [{}] {}", t.created_at, t.id);
        println!("  {}", t.content);
        if !t.symbols.is_empty() {
            println!("  symbols: {}", t.symbols.join(", "));
        }
    }
    0
}

// ─── Tool call log ────────────────────────────────────────────────────────────

async fn run_log(store: &NoteStore, since_ts: i64) -> i32 {
    let rows = store.tool_call_log_rows(since_ts, 1000).await.unwrap_or_default();
    if rows.is_empty() {
        println!("No tool call log entries in this period.");
        return 0;
    }
    let inferred = infer_patterns(&rows);
    println!();
    println!("TOOL CALL LOG PATTERNS");
    println!("{}", "─".repeat(22));
    println!("  {} calls logged in period.", rows.len());
    println!();

    // Per-tool summary.
    let mut tool_counts: HashMap<&str, (usize, usize, usize)> = HashMap::new(); // (success, error, empty)
    for r in &rows {
        let e = tool_counts.entry(r.tool_name.as_str()).or_default();
        match r.outcome.as_str() {
            "success" => e.0 += 1,
            "error" => e.1 += 1,
            "empty_result" => e.2 += 1,
            _ => {}
        }
    }
    let mut tool_list: Vec<_> = tool_counts.iter().collect();
    tool_list.sort_by(|a, b| (b.1.0 + b.1.1 + b.1.2).cmp(&(a.1.0 + a.1.1 + a.1.2)));
    for (tool, (ok, err, empty)) in tool_list.iter().take(20) {
        println!("  {:<25} ok:{:<6} err:{:<6} empty:{}", tool, ok, err, empty);
    }

    if !inferred.is_empty() {
        println!();
        println!("INFERRED PATTERNS");
        println!("{}", "─".repeat(17));
        for (tool, patterns) in &inferred {
            for (pattern, count) in patterns {
                println!("  [{tool}] {pattern} (×{count})");
            }
        }
    }
    0
}

// ─── Retirement ───────────────────────────────────────────────────────────────

async fn run_retire(
    store: &NoteStore,
    tool_name: Option<&str>,
    retire_id: Option<&str>,
    reason: Option<&str>,
    yes: bool,
) -> i32 {
    let reason = match reason {
        Some(r) => r,
        None => {
            eprintln!("error: --reason is required with --retire (e.g. --reason \"fixed in PR #88\")");
            return 1;
        }
    };

    if let Some(id) = retire_id {
        // Single retirement by ID.
        if !yes && !confirm(&format!("Retire reflection {id}?")) {
            println!("Aborted.");
            return 0;
        }
        match store.retire_by_id(id, reason).await {
            Ok(true) => println!("Retired {id}."),
            Ok(false) => {
                eprintln!("error: reflection '{id}' not found or already retired");
                return 1;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
        return 0;
    }

    let tool = match tool_name {
        Some(t) => t,
        None => {
            eprintln!("error: --retire requires either --tool <name> or --id <uuid>");
            return 1;
        }
    };

    // Preview what will be retired.
    let active = match store.read_reflections(0, Some(tool), false).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    if active.is_empty() {
        println!("No active reflections found for tool '{tool}'.");
        return 0;
    }

    println!();
    println!("Retiring {} active reflection{} about {}:", active.len(), if active.len() == 1 { "" } else { "s" }, tool);
    for note in &active {
        let snippet = extract_task_summary(&note.content).unwrap_or_else(|| truncate(&note.content, 80));
        println!("  [{}] \"{}\"", note.id, snippet);
    }
    println!();

    if !yes && !confirm(&format!("Retire all {}?", active.len())) {
        println!("Aborted.");
        return 0;
    }

    match store.retire_by_tool(tool, reason).await {
        Ok(ids) => {
            println!("Retired {} reflection{}.", ids.len(), if ids.len() == 1 { "" } else { "s" });
            println!("These reflections will no longer surface to agents or in reflect output.");
            println!("Run `sovereign reflect --history --tool {tool}` to see them.");
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    }
    0
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Locate notes.db. Looks for `<data_dir>/*/notes.db` (one level of project
/// subdirectory) or `<data_dir>/notes.db` directly. Returns the most recently
/// modified one, or accepts an explicit path.
pub(crate) fn find_notes_db(data_dir: Option<&Path>) -> Option<PathBuf> {
    // If caller passed --data-dir, treat that directory as the base and look
    // for notes.db directly inside it (or one subdirectory deep).
    if let Some(base) = data_dir {
        let direct = base.join("notes.db");
        if direct.exists() {
            return Some(direct);
        }
        // One subdirectory level: <data-dir>/<project>/notes.db
        return find_in_subdirs(base);
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let sovereign_home = home.join(".sovereign");

    // Priority 1: Active pointer written by `project serve` at startup.
    // This is the canonical source of truth — it always points at the database
    // the running (or most recently run) server was using, regardless of
    // working directory.
    let pointer = sovereign_home.join("active_notes_db");
    if let Ok(contents) = std::fs::read_to_string(&pointer) {
        let path = PathBuf::from(contents.trim());
        if path.exists() {
            return Some(path);
        }
    }

    // Priority 2: Walk up from cwd — mirrors find_sovereign_dir() in
    // project_cmd.rs. Works when reflect is run from inside the same project
    // tree that was served.
    if let Ok(cwd) = std::env::current_dir() {
        let mut current = cwd;
        loop {
            let candidate = current.join(".sovereign").join("notes.db");
            if candidate.exists() {
                return Some(candidate);
            }
            match current.parent() {
                Some(p) => current = p.to_path_buf(),
                None => break,
            }
        }
    }

    // Priority 3: ~/.sovereign/notes.db  (global fallback)
    let home_direct = sovereign_home.join("notes.db");
    if home_direct.exists() {
        return Some(home_direct);
    }

    // Priority 4: ~/.sovereign/<subdir>/notes.db  (multi-project layout)
    if let Some(p) = find_in_subdirs(&sovereign_home) {
        return Some(p);
    }

    // Priority 5: ~/.sovereign/indexes/*/notes.db  (legacy layout)
    find_in_subdirs(&sovereign_home.join("indexes"))
}

/// Search one directory level deep for notes.db, returning the most recently
/// modified file found (or None if no candidates exist).
fn find_in_subdirs(base: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let candidate = entry.path().join("notes.db");
            if candidate.exists() {
                let mtime = candidate
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                candidates.push((candidate, mtime));
            }
        }
    }
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.into_iter().next().map(|(p, _)| p)
}

/// Parse "7d" / "24h" / "30" into a number of days.
fn parse_duration(s: &str) -> Option<u64> {
    if let Some(n) = s.strip_suffix('d') {
        n.parse().ok()
    } else if let Some(n) = s.strip_suffix('h') {
        n.parse::<u64>().ok().map(|h| h / 24 + 1)
    } else {
        s.parse().ok()
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        format!("{}…", chars[..max].iter().collect::<String>())
    }
}

fn count_sessions<'a>(rows: impl Iterator<Item = &'a NoteRow>) -> usize {
    rows.map(|n| n.session_id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// Try to extract a `task_summary` from JSON content.
fn extract_task_summary(content: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| v.get("task_summary")?.as_str().map(|s| s.to_string()))
}

/// Try to extract the `tool_name` field from JSON content when the column is NULL.
fn extract_tool_from_content(content: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| {
            v.get("tool_name")
                .and_then(|t| t.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
}

/// Extract the negative fields from a JSON reflection content blob.
fn extract_negative_snippets(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
        for key in &["manual_work_that_should_be_a_tool", "misleading_outputs", "wished_i_had_known"] {
            if let Some(s) = v.get(key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
        }
        // Fall back to task_summary if nothing negative.
        if out.is_empty() {
            if let Some(s) = v.get("task_summary").and_then(|v| v.as_str()) {
                out.push(s.to_string());
            }
        }
    } else {
        // Plain text content — return as-is.
        out.push(content.to_string());
    }
    out
}

/// Detect behavioural patterns from the tool call log.
///
/// Returns a map of tool_name → Vec<(description, count)>.
fn infer_patterns(rows: &[ToolCallLogRow]) -> HashMap<String, Vec<(String, usize)>> {
    let mut result: HashMap<String, Vec<(String, usize)>> = HashMap::new();

    // Pattern A: test_status (success) immediately followed by run_tests in same session
    // within 60 seconds — agent didn't trust freshness.
    {
        let mut count = 0usize;
        for i in 0..rows.len().saturating_sub(1) {
            let a = &rows[i];
            let b = &rows[i + 1];
            if a.session_id == b.session_id
                && a.tool_name == "run_tests"
                && b.tool_name == "test_status"
                && b.outcome == "success"
                && (a.called_at - b.called_at).unsigned_abs() <= 60
            {
                count += 1;
            }
        }
        if count > 0 {
            result
                .entry("test_status".to_string())
                .or_default()
                .push(("test_status(fresh) immediately followed by run_tests — agent didn't trust freshness".to_string(), count));
        }
    }

    // Pattern B: blast_radius followed by 4+ symbol_lookup calls in same session.
    {
        let mut session_groups: HashMap<&str, Vec<&ToolCallLogRow>> = HashMap::new();
        for r in rows {
            session_groups.entry(&r.session_id).or_default().push(r);
        }
        let mut count = 0usize;
        for calls in session_groups.values() {
            let has_blast = calls.iter().any(|r| r.tool_name == "blast_radius");
            if has_blast {
                let symbol_lookups = calls.iter().filter(|r| r.tool_name == "symbol_lookup").count();
                if symbol_lookups >= 4 {
                    count += 1;
                }
            }
        }
        if count > 0 {
            result
                .entry("blast_radius".to_string())
                .or_default()
                .push((format!("blast_radius followed by 4+ symbol_lookup calls in {} session(s) — agent had to manually trace callers", count), count));
        }
    }

    // Pattern C: project_context empty_result repeated ≥3 times across sessions.
    {
        let empty_sessions: std::collections::HashSet<&str> = rows
            .iter()
            .filter(|r| r.tool_name == "project_context" && r.outcome == "empty_result")
            .map(|r| r.session_id.as_str())
            .collect();
        if empty_sessions.len() >= 3 {
            result
                .entry("project_context".to_string())
                .or_default()
                .push((format!("project_context returned empty results in {} sessions — index may be missing content", empty_sessions.len()), empty_sessions.len()));
        }
    }

    result
}

fn confirm(prompt: &str) -> bool {
    print!("{} [y/N] ", prompt);
    io::stdout().flush().ok();
    let stdin = io::stdin();
    let line = stdin.lock().lines().next().and_then(|l| l.ok()).unwrap_or_default();
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign reflect",
    summary: "Review session reflections and retire ones that are no longer relevant.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign reflect [flags]"),
        crate::util::help::HelpSection::Flags(&[
            ("--since <Nd|Nh>",    "Period to analyse (default: 30d)"),
            ("--tool <name>",      "Filter signals to one tool"),
            ("--raw",              "Print full reflection prose ungrouped"),
            ("--todos",            "List open todo notes only"),
            ("--log",              "Show raw tool_call_log patterns"),
            ("--history",          "Include retired reflections"),
            ("--retire",           "Retire matching reflections (requires --tool or --id + --reason)"),
            ("--id <uuid>",        "Target a specific reflection for retirement"),
            ("--reason <why>",     "Retirement rationale (required with --retire)"),
            ("--yes",              "Skip retirement confirmation prompt"),
            ("--data-dir <path>",  "Directory containing notes.db (default: ~/.sovereign/indexes)"),
        ]),
        crate::util::help::HelpSection::Examples(&[
            ("sovereign reflect",                                           "30-day backlog summary"),
            ("sovereign reflect --since 7d --tool blast_radius",            "Last week, one tool only"),
            ("sovereign reflect --retire --tool blast_radius --reason \"macro support added in v0.4.2\"",
             "Retire all blast_radius reflections"),
        ]),
    ],
};
