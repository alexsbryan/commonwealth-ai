// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign atos teardown <feature>` — interactive note classification
//! pass that ends with a frozen epistemic-report.md and the feature
//! marked `completed`.
//!
//! Default: interactive. `--auto`: retire everything (no promotions —
//! promotions are cheap-but-consequential so auto-promote is
//! deliberately absent until a future Fast-slot suggestion pass with
//! a confirmation gate lands). `--dry-run`: print what would happen
//! without mutating.

use sovereign_atos::AtosOrchestrator;

use super::args::split_args;
use super::stores::open_orchestrator;

pub(crate) async fn cmd_teardown(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("teardown: missing <feature-id>");
        return 2;
    };
    let auto = flags.iter().any(|(k, _)| k == "auto");
    let dry_run = flags.iter().any(|(k, _)| k == "dry-run");

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("teardown: {e}");
            return 1;
        }
    };
    let Some(feature) = orc.get_feature(&feature_id).await.ok().flatten() else {
        eprintln!("teardown: feature '{feature_id}' not found");
        return 1;
    };
    if feature.state == "completed" {
        println!("teardown: feature '{}' is already completed.", feature.id);
        return 0;
    }

    let candidates = match orc.teardown_candidates(&feature_id).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("teardown: load candidates: {e}");
            return 1;
        }
    };

    println!();
    println!("  ── atos teardown ────────────────────────────────────────");
    println!("  Feature: {}", feature.id);
    println!("  Notes to review: {}", candidates.len());
    if candidates.is_empty() {
        println!("  (no feature-scoped decision/invariant/attempt/uncertainty/pointer notes)");
    }
    println!();

    let mut actions: Vec<sovereign_atos::TeardownAction> = Vec::new();
    for (idx, note) in candidates.iter().enumerate() {
        let first = note.content.lines().next().unwrap_or("").trim();
        let trimmed: String = first.chars().take(120).collect();
        println!(
            "  Note {}/{} [{}] {}\n    files: {}\n    id: {}",
            idx + 1,
            candidates.len(),
            note.kind,
            trimmed,
            if note.files.is_empty() {
                "(none)".into()
            } else {
                note.files.join(", ")
            },
            note.id
        );

        let choice = if auto {
            // Conservative auto: retire everything. Future M4 adds
            // Fast-slot suggestion for promotions.
            'r'
        } else {
            match prompt_teardown_action() {
                Some(c) => c,
                None => {
                    println!("teardown: aborted.");
                    return 1;
                }
            }
        };

        let action = match choice {
            'p' | 'P' => sovereign_atos::TeardownAction::Promote {
                note_id: note.id.clone(),
                rewritten_content: None,
            },
            'a' | 'A' => sovereign_atos::TeardownAction::Archive {
                note_id: note.id.clone(),
            },
            'r' | 'R' => sovereign_atos::TeardownAction::Retire {
                note_id: note.id.clone(),
            },
            _ => sovereign_atos::TeardownAction::Skip {
                note_id: note.id.clone(),
            },
        };
        actions.push(action);
        println!();
    }

    if dry_run {
        println!("  DRY RUN — no mutations applied. Action counts:");
        let mut p = 0;
        let mut a = 0;
        let mut r = 0;
        let mut s = 0;
        for act in &actions {
            match act {
                sovereign_atos::TeardownAction::Promote { .. } => p += 1,
                sovereign_atos::TeardownAction::Archive { .. } => a += 1,
                sovereign_atos::TeardownAction::Retire { .. } => r += 1,
                sovereign_atos::TeardownAction::Skip { .. } => s += 1,
            }
        }
        println!("    promoted: {p}\n    archived: {a}\n    retired:  {r}\n    skipped:  {s}");
        return 0;
    }

    let report = match orc.apply_teardown(&feature_id, actions).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("teardown: apply: {e}");
            return 1;
        }
    };

    println!();
    println!(
        "  applied: promoted {} / archived {} / retired {} / skipped {}",
        report.promoted.len(),
        report.archived.len(),
        report.retired.len(),
        report.skipped.len()
    );

    // Final artifact: epistemic-report.md.
    match orc
        .render_and_write_report(&feature_id, sovereign_atos::ReportSection::Epistemic)
        .await
    {
        Ok(path) => println!("  wrote {}", path.display()),
        Err(e) => eprintln!("  warning: render epistemic-report.md failed: {e}"),
    }

    0
}

fn prompt_teardown_action() -> Option<char> {
    use std::io::Write;
    eprint!("    action [P]romote / [a]rchive / [r]etire / [s]kip / [q]uit (default: s): ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    match line.trim().to_lowercase().as_str() {
        "q" | "quit" => None,
        "p" | "promote" => Some('p'),
        "a" | "archive" => Some('a'),
        "r" | "retire" => Some('r'),
        "" | "s" | "skip" => Some('s'),
        other => {
            eprintln!("    unknown response '{other}' — treating as skip.");
            Some('s')
        }
    }
}
