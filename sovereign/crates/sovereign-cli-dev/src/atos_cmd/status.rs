// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atos status|promote|report` — read-only and mutation
//! commands for inspecting and manipulating feature state.
//!
//! - **`status`** prints the feature + milestone table. When invoked
//!   against a single feature it also renders the artifact-review
//!   checklist derived from the last ended milestone's compliance
//!   report.
//! - **`promote`** lifts a note from feature scope to global scope
//!   (or vice versa), optionally replacing its content.
//! - **`report`** renders a report section (milestone / red-team /
//!   epistemic / all) to stdout or a file.

use corpus_engine_notes::NoteScope;
use sovereign_atos::AtosOrchestrator;

use super::args::parse_args;
use super::stores::open_orchestrator;

// ─── status ──────────────────────────────────────────────────────────────────

pub(crate) async fn cmd_status(args: &[String]) -> i32 {
    let flags = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("atos: {e}");
            return 2;
        }
    };
    let positional = flags.positionals();
    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("status: {e}");
            return 1;
        }
    };

    if let Some(id) = positional.first() {
        match orc.get_feature(id).await {
            Ok(Some(f)) => {
                println!("{}  [{}]", f.id, f.state);
                println!("  title:   {}", f.title);
                println!("  stop:    {}", f.stop_condition);
                let milestones = orc.list_milestones(&f.id).await.unwrap_or_default();
                for m in &milestones {
                    let status = match (m.started_at, m.ended_at) {
                        (_, Some(_)) => "ended",
                        (Some(_), None) => "active",
                        (None, None) => "pending",
                    };
                    println!("  m{} [{status}] {} bytes", m.ordinal, m.brief_md.len());
                }

                // Render the artifact-review checklist for the most
                // recent ended milestone. This is the view the
                // operator inspects during review — it should let
                // them tick every box without reading implementation
                // code.
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
        match orc.list_features(false).await {
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

// ─── promote ─────────────────────────────────────────────────────────────────

pub(crate) async fn cmd_promote(args: &[String]) -> i32 {
    let flags = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("atos: {e}");
            return 2;
        }
    };
    let positional = flags.positionals();
    let Some(note_id) = positional.first().cloned() else {
        eprintln!("promote: missing <note-id>");
        return 2;
    };
    let to = match flags.value("to").map(|s| s.to_string()).as_deref() {
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
    let feature_id = flags.value("feature-id").map(|s| s.to_string());
    let content = match flags.value("content").map(|s| s.to_string()) {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("promote: read {p}: {e}");
                return 1;
            }
        },
        None => None,
    };

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("promote: {e}");
            return 1;
        }
    };

    match orc
        .promote_note(&note_id, to, feature_id.as_deref(), content.as_deref())
        .await
    {
        Ok(new_id) => {
            println!(
                "promoted note {note_id} -> {new_id} (scope={})",
                to.as_str()
            );
            0
        }
        Err(e) => {
            eprintln!("promote: {e}");
            1
        }
    }
}

// ─── report ──────────────────────────────────────────────────────────────────

pub(crate) async fn cmd_report(args: &[String]) -> i32 {
    let flags = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("atos: {e}");
            return 2;
        }
    };
    let positional = flags.positionals();
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("report: missing <feature-id>");
        return 2;
    };
    let section = match flags.value("section").map(|s| s.to_string()).as_deref() {
        None | Some("all") => sovereign_atos::ReportSection::All,
        Some("epistemic") => sovereign_atos::ReportSection::Epistemic,
        Some("red-team") | Some("redteam") => sovereign_atos::ReportSection::RedTeam,
        Some("milestone") => {
            let n = flags
                .value("milestone")
                .map(|s| s.to_string())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(1);
            sovereign_atos::ReportSection::Milestone(n)
        }
        Some(other) => {
            eprintln!("report: unknown --section '{other}'");
            return 2;
        }
    };
    let out_path = flags.value("out").map(|s| s.to_string());

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("report: {e}");
            return 1;
        }
    };

    match orc.render_report(&feature_id, section).await {
        Ok(md) => {
            if let Some(p) = out_path {
                if let Err(e) = std::fs::write(&p, md) {
                    eprintln!("report: write {p}: {e}");
                    return 1;
                }
                println!("report: wrote {p}");
            } else {
                print!("{md}");
            }
            0
        }
        Err(e) => {
            eprintln!("report: {e}");
            1
        }
    }
}

// ─── Artifact checklist ─────────────────────────────────────────────────────

/// Render the compliance-review checklist derived from §6 of the ATOS
/// design doc. The goal is that an operator can tick every box
/// without reading the implementation — decisions live in the note
/// log, coverage lives in the stop_condition result, and deviations
/// appear as `attempt` notes.
fn render_artifact_checklist(
    feature: &corpus_engine_atos::FeatureRow,
    milestone: &corpus_engine_atos::MilestoneRow,
    compliance_json: &str,
) {
    let parsed: serde_json::Value = match serde_json::from_str(compliance_json) {
        Ok(v) => v,
        Err(_) => {
            println!("  (compliance report present but not JSON-parsable)");
            return;
        }
    };

    let stop_passed = parsed
        .get("stop_passed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let empty_notes: Vec<serde_json::Value> = Vec::new();
    let notes = parsed
        .get("notes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_notes);

    let count_kind = |k: &str| {
        notes
            .iter()
            .filter(|n| n.get("kind").and_then(|v| v.as_str()) == Some(k))
            .count()
    };
    let decisions = count_kind("decision");
    let invariants = count_kind("invariant");
    let attempts = count_kind("attempt");
    let todos = count_kind("todo");

    println!();
    println!(
        "  ── Artifact review checklist (milestone {}) ──",
        milestone.ordinal
    );
    println!("  Spec compliance");
    print_check(
        feature.stop_condition.trim().is_empty() || stop_passed,
        &format!("stop_condition: '{}'", feature.stop_condition),
    );
    print_check(!feature.charter_md.trim().is_empty(), "charter_md present");

    println!();
    println!("  Decision log");
    print_check(
        decisions + invariants + attempts + todos > 0,
        &format!(
            "notes produced: {decisions} decisions, {invariants} invariants, \
             {attempts} attempts, {todos} todos"
        ),
    );
    print_check(
        attempts == 0 || attempts < 5,
        "no repeated failed attempts (indicates lost context across compaction)",
    );

    println!();
    println!("  Test evidence");
    print_check(stop_passed, "stop_condition exit=0");

    println!();
    println!("  Hints");
    if notes.is_empty() {
        println!(
            "    - no feature-scoped notes were written; review whether the agent used the scope parameter"
        );
    }
    if !stop_passed {
        println!("    - stop_condition failed; inspect the brief and the attempt notes");
    }
}

fn print_check(ok: bool, label: &str) {
    let mark = if ok { "[x]" } else { "[ ]" };
    println!("    {mark} {label}");
}
