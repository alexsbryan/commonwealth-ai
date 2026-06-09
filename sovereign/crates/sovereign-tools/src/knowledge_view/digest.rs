// SPDX-License-Identifier: AGPL-3.0-or-later
//! Landscape digest formatter — pure function from a
//! `FieldSkeleton` to the markdown block that gets spliced into
//! the system prompt.
//!
//! No I/O, no clock, no tokio. Deliberately separated from
//! `manager.rs` so its output is easy to test in isolation and its
//! token-budget behaviour stays legible.

use corpus_engine::enrichment::skeleton::FieldSkeleton;

use super::tokens::{estimate_tokens, is_settled_status};
use super::view_kind::ViewKind;

/// Human-readable heading for an arbitrary view id. Falls back to a
/// generic label for any id not matching a known `ViewKind` so
/// diagnostic output from misconfigured recipes still reads cleanly.
pub(crate) fn view_title(view_id: &str) -> &'static str {
    ViewKind::from_id(view_id)
        .map(|k| k.title())
        .unwrap_or("Knowledge view")
}

/// Render a landscape-digest markdown block for one view.
///
/// The output is divided into three clearly-labelled sections:
///
///   - **Settled concerns** — canonical questions with at least one
///     position that `is_settled_status` recognises as consensus.
///   - **Live tensions** — fault lines flattened across every
///     canonical question.
///   - **Open questions** — the skeleton's explicit open-question list.
///
/// Each section is capped at five bullets; each bullet is gated
/// against `budget_tokens` using `estimate_tokens`. A final sweep
/// trims trailing lines if the accumulated estimate overshoots —
/// conservative by design: we'd rather drop a line than blow the
/// system-prompt budget.
pub(crate) fn format_landscape(
    skeleton: &FieldSkeleton,
    view_id: &str,
    budget_tokens: usize,
) -> String {
    let mut out = String::new();
    let title = view_title(view_id);
    out.push_str(&format!("{title}:\n\n"));

    // Settled concerns: canonical questions where at least one
    // position is reported as dominant/held/settled-style.
    let settled: Vec<_> = skeleton
        .canonical_questions
        .iter()
        .filter(|q| q.positions.iter().any(|p| is_settled_status(&p.status)))
        .collect();
    if !settled.is_empty() {
        out.push_str("  Settled concerns:\n");
        for q in settled.iter().take(5) {
            let line = format!("    — {}\n", q.question);
            if estimate_tokens(&out) + estimate_tokens(&line) > budget_tokens {
                break;
            }
            out.push_str(&line);
        }
        out.push('\n');
    }

    // Live tensions: fault lines across all canonical questions.
    let fault_lines: Vec<_> = skeleton
        .canonical_questions
        .iter()
        .flat_map(|q| q.fault_lines.iter())
        .collect();
    if !fault_lines.is_empty() {
        out.push_str("  Live tensions:\n");
        for fl in fault_lines.iter().take(5) {
            let line = format!("    — {}\n", fl.crux);
            if estimate_tokens(&out) + estimate_tokens(&line) > budget_tokens {
                break;
            }
            out.push_str(&line);
        }
        out.push('\n');
    }

    // Open questions.
    if !skeleton.open_questions.is_empty() {
        out.push_str("  Open questions:\n");
        for oq in skeleton.open_questions.iter().take(5) {
            let line = format!("    — {}\n", oq.question);
            if estimate_tokens(&out) + estimate_tokens(&line) > budget_tokens {
                break;
            }
            out.push_str(&line);
        }
    }

    // Hard guard: if we somehow overshot (a long per-bullet entry that
    // squeaked past the per-line check), trim at the last newline that
    // keeps us under budget. Conservative — better to lose a line than
    // leak past the prompt budget.
    while estimate_tokens(&out) > budget_tokens {
        match out.rfind('\n') {
            Some(idx) if idx > 0 => out.truncate(idx),
            _ => {
                out.clear();
                break;
            }
        }
    }

    // Glassbox: record what we produced for this view so an operator
    // tailing `tracing=debug` can verify budget adherence without
    // having to log the full prompt. `input_*` counts let us correlate
    // "skeleton had 12 questions" with "digest shows 5" when
    // downstream looks sparse.
    tracing::debug!(
        view_id,
        budget_tokens,
        output_tokens = estimate_tokens(&out),
        input_canonical_questions = skeleton.canonical_questions.len(),
        input_open_questions = skeleton.open_questions.len(),
        "digest: rendered"
    );

    out
}
