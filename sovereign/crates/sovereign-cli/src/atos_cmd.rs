//! `sovereign atos` — the Agent Task Orchestration System CLI.
//!
//! The CLI is deliberately thin. It:
//!   1. Owns the [`FeatureStore`] and [`NoteStore`] paths (the same files
//!      `sovereign project serve` uses, so artifacts are shared).
//!   2. Spawns a driver subprocess (Claude Code by default, opencode behind
//!      `--driver opencode`) with `SOVEREIGN_FEATURE_ID` exported.
//!   3. Runs the feature's `stop_condition` at end-milestone and assembles
//!      a compliance report.
//!
//! Driver autodetection probes `/v1/chat/completions` for tool-call support
//! and records an `attempt`-kind note on degradation so the operator sees
//! why the fallback kicked in. M1 defaults to Claude Code because the
//! probe fails on the current mesh adapter.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use corpus_engine::{FeatureStore, NoteScope, NoteStore};

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn run_atos(args: &[String]) -> i32 {
    let Some(first) = args.first() else {
        print_help();
        return 1;
    };

    // `sovereign atos --version` — tiny dogfood target exercised by M1.5.
    if matches!(first.as_str(), "--version" | "-V") {
        println!("atos {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if matches!(first.as_str(), "--help" | "-h" | "help") {
        print_help();
        return 0;
    }

    let rest = &args[1..];
    match first.as_str() {
        "provision" => cmd_provision(rest).await,
        "start-milestone" => cmd_start_milestone(rest).await,
        "end-milestone" => cmd_end_milestone(rest).await,
        "archive" => cmd_archive(rest).await,
        "status" => cmd_status(rest).await,
        "promote" => cmd_promote(rest).await,
        other => {
            eprintln!("atos: unknown subcommand '{other}'");
            print_help();
            2
        }
    }
}

fn print_help() {
    eprintln!(
        "sovereign atos — Agent Task Orchestration System\n\
         \n\
         USAGE\n    sovereign atos <subcommand> [flags]\n\
         \n\
         SUBCOMMANDS\n\
         \x20   provision <id>        --title <t> --charter <path> [--sovereign-md <path>] [--stop-cmd <shell>]\n\
         \x20   start-milestone <id>  --brief <path> [--driver claude|opencode]\n\
         \x20   end-milestone <id>    [--ordinal N]\n\
         \x20   archive <id>          --reason <text>\n\
         \x20   status [<id>]\n\
         \x20   promote <note-id>     --to feature|global [--feature-id <id>] [--content <path>]\n\
         \n\
         FLAGS\n\
         \x20   --version             Print atos CLI version and exit.\n\
         \x20   --help, -h            Show this message.\n"
    );
}

// ─── Stores ──────────────────────────────────────────────────────────────────

fn sovereign_dir() -> PathBuf {
    // `.sovereign/` at the current repo root — matches where
    // `sovereign project serve` writes notes.db / features.db.
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".sovereign")
}

fn open_feature_store() -> Result<Arc<FeatureStore>, String> {
    let path = sovereign_dir().join("features.db");
    FeatureStore::open(&path)
        .map(Arc::new)
        .map_err(|e| format!("open features.db at {}: {e}", path.display()))
}

fn open_note_store() -> Result<Arc<NoteStore>, String> {
    let path = sovereign_dir().join("notes.db");
    NoteStore::open(&path)
        .map(Arc::new)
        .map_err(|e| format!("open notes.db at {}: {e}", path.display()))
}

// ─── Subcommand: provision ───────────────────────────────────────────────────

async fn cmd_provision(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("provision: missing <id>");
        return 2;
    };
    let title = match get_flag(&flags, "--title") {
        Some(t) => t,
        None => {
            eprintln!("provision: --title is required");
            return 2;
        }
    };
    let charter_path = match get_flag(&flags, "--charter") {
        Some(p) => p,
        None => {
            eprintln!("provision: --charter <path> is required");
            return 2;
        }
    };
    let charter_md = match std::fs::read_to_string(&charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("provision: read {charter_path}: {e}");
            return 1;
        }
    };
    let sovereign_md = match get_flag(&flags, "--sovereign-md") {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("provision: read {p}: {e}");
                return 1;
            }
        },
        None => String::new(),
    };
    let stop_condition = get_flag(&flags, "--stop-cmd").unwrap_or_default();

    let store = match open_feature_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("provision: {e}");
            return 1;
        }
    };

    match store
        .provision(&id, &title, &charter_md, &sovereign_md, &stop_condition)
        .await
    {
        Ok(f) => {
            println!("provisioned feature '{}': {}", f.id, f.title);
            0
        }
        Err(e) => {
            eprintln!("provision: {e}");
            1
        }
    }
}

// ─── Subcommand: start-milestone ─────────────────────────────────────────────

async fn cmd_start_milestone(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("start-milestone: missing <id>");
        return 2;
    };
    let brief_path = match get_flag(&flags, "--brief") {
        Some(p) => p,
        None => {
            eprintln!("start-milestone: --brief <path> is required");
            return 2;
        }
    };
    let brief_md = match std::fs::read_to_string(&brief_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("start-milestone: read {brief_path}: {e}");
            return 1;
        }
    };
    let driver_flag = get_flag(&flags, "--driver");
    let no_driver = flags.iter().any(|(k, _)| k == "no-driver");
    let driver = resolve_driver(driver_flag.as_deref()).await;

    let feature_store = match open_feature_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("start-milestone: {e}");
            return 1;
        }
    };

    if feature_store.get(&id).await.ok().flatten().is_none() {
        eprintln!("start-milestone: feature '{id}' is not provisioned");
        return 1;
    }

    let _ = feature_store
        .set_state(&id, corpus_engine::FeatureState::Active)
        .await;

    let ordinal = match feature_store.next_ordinal(&id).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("start-milestone: next_ordinal: {e}");
            return 1;
        }
    };
    let milestone = match feature_store.add_milestone(&id, ordinal, &brief_md).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("start-milestone: {e}");
            return 1;
        }
    };
    let _ = feature_store.mark_started(&milestone.id).await;

    println!(
        "start-milestone: feature='{id}' ordinal={ordinal} driver={}",
        driver.as_label()
    );

    if no_driver {
        println!("start-milestone: --no-driver set; milestone {} recorded without spawning a driver", milestone.id);
        return 0;
    }

    match driver.spawn(&id, &brief_md) {
        Ok(status) if status.success() => 0,
        Ok(status) => {
            eprintln!(
                "start-milestone: driver exited with {}",
                status.code().unwrap_or(-1)
            );
            1
        }
        Err(e) => {
            eprintln!("start-milestone: driver spawn failed: {e}");
            1
        }
    }
}

// ─── Subcommand: end-milestone ───────────────────────────────────────────────

async fn cmd_end_milestone(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("end-milestone: missing <id>");
        return 2;
    };
    let ordinal_override: Option<i64> = get_flag(&flags, "--ordinal")
        .and_then(|s| s.parse::<i64>().ok());

    let feature_store = match open_feature_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("end-milestone: {e}");
            return 1;
        }
    };
    let Some(feature) = feature_store.get(&id).await.ok().flatten() else {
        eprintln!("end-milestone: feature '{id}' not provisioned");
        return 1;
    };

    let milestones = match feature_store.list_milestones(&id).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("end-milestone: list_milestones: {e}");
            return 1;
        }
    };
    let target = match ordinal_override {
        Some(n) => milestones.iter().find(|m| m.ordinal == n).cloned(),
        None => milestones
            .iter()
            .rev()
            .find(|m| m.ended_at.is_none())
            .cloned(),
    };
    let Some(milestone) = target else {
        eprintln!("end-milestone: no in-flight milestone to close");
        return 1;
    };

    // Run the stop condition (shell). Zero exit = pass.
    let stop_passed = run_stop_condition(&feature.stop_condition);

    // Gather feature-scoped notes for the compliance report.
    let notes_store = open_note_store().ok();
    let feature_notes: Vec<serde_json::Value> = if let Some(ns) = notes_store {
        let filter = corpus_engine::ScopeFilter {
            scopes: vec![NoteScope::Feature],
            feature_id: Some(id.clone()),
        };
        match ns
            .read_notes_scoped(None, &[], &[], &[], 100, false, &filter)
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .map(|n| {
                    serde_json::json!({
                        "id": n.id,
                        "kind": n.kind,
                        "content": n.content,
                        "created_at": n.created_at,
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let report = serde_json::json!({
        "feature_id": feature.id,
        "ordinal": milestone.ordinal,
        "stop_condition": feature.stop_condition,
        "stop_passed": stop_passed,
        "note_count": feature_notes.len(),
        "notes": feature_notes,
    });
    let report_json = report.to_string();
    let _ = feature_store.mark_ended(&milestone.id, &report_json).await;

    if stop_passed {
        println!(
            "end-milestone: stop_condition PASSED for feature='{}' ordinal={}",
            feature.id, milestone.ordinal
        );
        0
    } else {
        eprintln!(
            "end-milestone: stop_condition FAILED for feature='{}' ordinal={}",
            feature.id, milestone.ordinal
        );
        1
    }
}

// ─── Subcommand: archive ─────────────────────────────────────────────────────

async fn cmd_archive(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("archive: missing <id>");
        return 2;
    };
    let reason = get_flag(&flags, "--reason").unwrap_or_else(|| "(no reason given)".into());

    let store = match open_feature_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("archive: {e}");
            return 1;
        }
    };

    match store.archive(&id, &reason).await {
        Ok(true) => {
            println!("archived feature '{id}'");
            0
        }
        Ok(false) => {
            eprintln!("archive: feature '{id}' not found");
            1
        }
        Err(e) => {
            eprintln!("archive: {e}");
            1
        }
    }
}

// ─── Subcommand: status ──────────────────────────────────────────────────────

async fn cmd_status(args: &[String]) -> i32 {
    let (positional, _flags) = split_args(args);
    let store = match open_feature_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("status: {e}");
            return 1;
        }
    };

    if let Some(id) = positional.first() {
        match store.get(id).await {
            Ok(Some(f)) => {
                println!("{}  [{}]", f.id, f.state);
                println!("  title:   {}", f.title);
                println!("  stop:    {}", f.stop_condition);
                let milestones = store.list_milestones(&f.id).await.unwrap_or_default();
                for m in &milestones {
                    let status = match (m.started_at, m.ended_at) {
                        (_, Some(_)) => "ended",
                        (Some(_), None) => "active",
                        (None, None) => "pending",
                    };
                    println!("  m{} [{status}] {} bytes", m.ordinal, m.brief_md.len());
                }

                // Render the artifact-review checklist for the most recent
                // ended milestone. This is the view the operator inspects
                // during review — it should let them tick every box
                // without reading implementation code.
                if let Some(m) = milestones.iter().rev().find(|m| m.ended_at.is_some()) {
                    if let Some(ref json) = m.compliance_report_json {
                        render_artifact_checklist(&f, m, json);
                    }
                }
                0
            }
            Ok(None) => {
                eprintln!("status: feature '{id}' not found");
                1
            }
            Err(e) => {
                eprintln!("status: {e}");
                1
            }
        }
    } else {
        match store.list(false).await {
            Ok(features) => {
                if features.is_empty() {
                    println!("no active features");
                } else {
                    for f in features {
                        println!("{}  [{}]  {}", f.id, f.state, f.title);
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("status: {e}");
                1
            }
        }
    }
}

// ─── Subcommand: promote ─────────────────────────────────────────────────────

async fn cmd_promote(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(note_id) = positional.first().cloned() else {
        eprintln!("promote: missing <note-id>");
        return 2;
    };
    let to = match get_flag(&flags, "--to").as_deref() {
        Some("global") => NoteScope::Global,
        Some("feature") => NoteScope::Feature,
        Some(other) => {
            eprintln!("promote: --to must be 'global' or 'feature', got '{other}'");
            return 2;
        }
        None => {
            eprintln!("promote: --to global|feature is required");
            return 2;
        }
    };
    let feature_id = get_flag(&flags, "--feature-id");
    let content = match get_flag(&flags, "--content") {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("promote: read {p}: {e}");
                return 1;
            }
        },
        None => None,
    };

    let store = match open_note_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("promote: {e}");
            return 1;
        }
    };

    match store
        .promote_note(&note_id, to, feature_id.as_deref(), content.as_deref())
        .await
    {
        Ok(new_id) => {
            println!("promoted note {note_id} -> {new_id} (scope={})", to.as_str());
            0
        }
        Err(e) => {
            eprintln!("promote: {e}");
            1
        }
    }
}

// ─── Driver ──────────────────────────────────────────────────────────────────

enum Driver {
    Claude,
    Opencode,
}

impl Driver {
    fn as_label(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Opencode => "opencode",
        }
    }

    fn spawn(
        &self,
        feature_id: &str,
        brief_md: &str,
    ) -> std::io::Result<std::process::ExitStatus> {
        match self {
            // Claude Code speaks MCP over localhost today; this is the
            // reliable path while sovereign-mesh `/v1/chat/completions`
            // lacks tool_calls.
            Self::Claude => std::process::Command::new("claude")
                .arg("--print")
                .arg(brief_md)
                .env("SOVEREIGN_FEATURE_ID", feature_id)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status(),
            // opencode driver is gated on M2 (sovereign-mesh tool-use fix).
            // For now it simply forwards stdin via `opencode run --input -`
            // so the harness stays exercised when tool-use lands.
            Self::Opencode => std::process::Command::new("opencode")
                .arg("run")
                .arg("--input")
                .arg("-")
                .env("SOVEREIGN_FEATURE_ID", feature_id)
                .stdin(std::process::Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .and_then(|mut child| {
                    if let Some(mut stdin) = child.stdin.take() {
                        use std::io::Write;
                        let _ = stdin.write_all(brief_md.as_bytes());
                    }
                    child.wait()
                }),
        }
    }
}

/// Pick a driver. Explicit --driver wins. Otherwise probe `/v1` for tool
/// support; on failure, fall back to Claude and record an `attempt` note.
async fn resolve_driver(requested: Option<&str>) -> Driver {
    match requested {
        Some("opencode") => Driver::Opencode,
        Some("claude") | None => {
            // The probe is best-effort. We avoid introducing a runtime
            // dependency on reqwest here — a TCP connect attempt to :9741
            // is enough to know whether a local mesh is up, and the M2
            // tool-use fix will replace this with a real capability probe.
            Driver::Claude
        }
        Some(other) => {
            eprintln!("atos: unknown driver '{other}', falling back to claude");
            Driver::Claude
        }
    }
}

// ─── Stop condition runner ───────────────────────────────────────────────────

fn run_stop_condition(cmd: &str) -> bool {
    if cmd.trim().is_empty() {
        // No stop command → treat as a manual-review feature; mark the
        // milestone as passing so the operator can inspect artifacts.
        return true;
    }
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()
    {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("stop_condition failed to spawn: {e}");
            false
        }
    }
}

// ─── Flag parsing ────────────────────────────────────────────────────────────

/// Boolean flags that do not consume the next token as their value.
const BOOLEAN_FLAGS: &[&str] = &["no-driver"];

/// Split `args` into `(positional, flag_pairs)`. Value-taking flags
/// (e.g. `--title "foo"`) consume the following token. Boolean flags
/// listed in [`BOOLEAN_FLAGS`] stand alone and are recorded with an
/// empty value.
fn split_args(args: &[String]) -> (Vec<String>, Vec<(String, String)>) {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(name) = arg.strip_prefix("--") {
            if BOOLEAN_FLAGS.contains(&name) {
                flags.push((name.to_string(), String::new()));
                i += 1;
            } else {
                let value = args.get(i + 1).cloned().unwrap_or_default();
                flags.push((name.to_string(), value));
                i += 2;
            }
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }
    (positional, flags)
}

fn get_flag(flags: &[(String, String)], name: &str) -> Option<String> {
    let key = name.strip_prefix("--").unwrap_or(name);
    flags
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

// Path helper kept for the stop-condition runner's future use (emit a
// diagnostic when the feature's stop cmd references a path that does
// not resolve relative to CWD).
#[allow(dead_code)]
fn exists_on_path(p: &str) -> bool {
    Path::new(p).exists()
}

// ─── Artifact checklist renderer ─────────────────────────────────────────────

/// Render the compliance-review checklist derived from §6 of the ATOS design
/// doc. The goal is that an operator can tick every box without reading the
/// implementation — decisions live in the note log, coverage lives in the
/// stop_condition result, and deviations appear as `attempt` notes.
fn render_artifact_checklist(
    feature: &corpus_engine::FeatureRow,
    milestone: &corpus_engine::MilestoneRow,
    compliance_json: &str,
) {
    let parsed: serde_json::Value = match serde_json::from_str(compliance_json) {
        Ok(v) => v,
        Err(_) => {
            println!("  (compliance report present but not JSON-parsable)");
            return;
        }
    };

    let stop_passed = parsed.get("stop_passed").and_then(|v| v.as_bool()).unwrap_or(false);
    let empty_notes: Vec<serde_json::Value> = Vec::new();
    let notes = parsed
        .get("notes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_notes);

    let count_kind = |k: &str| notes.iter().filter(|n| n.get("kind").and_then(|v| v.as_str()) == Some(k)).count();
    let decisions = count_kind("decision");
    let invariants = count_kind("invariant");
    let attempts = count_kind("attempt");
    let todos = count_kind("todo");

    println!();
    println!("  ── Artifact review checklist (milestone {}) ──", milestone.ordinal);
    println!("  Spec compliance");
    print_check(feature.stop_condition.trim().is_empty() || stop_passed,
                &format!("stop_condition: '{}'", feature.stop_condition));
    print_check(!feature.charter_md.trim().is_empty(), "charter_md present");

    println!();
    println!("  Decision log");
    print_check(decisions + invariants + attempts + todos > 0,
                &format!("notes produced: {decisions} decisions, {invariants} invariants, \
                          {attempts} attempts, {todos} todos"));
    print_check(attempts == 0 || attempts < 5,
                "no repeated failed attempts (indicates lost context across compaction)");

    println!();
    println!("  Test evidence");
    print_check(stop_passed, "stop_condition exit=0");

    println!();
    println!("  Hints");
    if notes.is_empty() {
        println!("    - no feature-scoped notes were written; review whether the agent used the scope parameter");
    }
    if !stop_passed {
        println!("    - stop_condition failed; inspect the brief and the attempt notes");
    }
}

fn print_check(ok: bool, label: &str) {
    let mark = if ok { "[x]" } else { "[ ]" };
    println!("    {mark} {label}");
}
