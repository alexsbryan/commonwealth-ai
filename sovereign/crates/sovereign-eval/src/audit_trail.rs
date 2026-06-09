// SPDX-License-Identifier: AGPL-3.0-or-later
//! Audit-trail analyzer.
//!
//! For run #2 (the extension feature), we want to verify that decisions
//! and invariants the agent recorded during run #1 actually surface
//! when run #2 queries `notes`. That's the "auditable systems" axis the
//! user named explicitly: the system has to carry the *why* across
//! sessions, otherwise the audit trail is theatre.
//!
//! We work entirely from the two manifests. Inputs:
//! - run #1 manifest: contains run-1's notes (decisions, invariants,
//!   uncertainties, etc.)
//! - run #2 manifest: contains run-2's tool calls — specifically every
//!   `notes` (or legacy `read_notes`) invocation with its args_json.
//!
//! For each substantive run-1 note, we ask: did any run-2 `notes`
//! query return a result that includes it? Since the manifest doesn't
//! capture tool *responses* (that's a known gap), we approximate by
//! checking whether the run-2 query terms could plausibly have matched
//! the run-1 note (FTS5-style overlap on content + symbols).

use crate::manifest::{Manifest, NoteRow};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub run1_substantive_notes: u32,
    pub run2_notes_queries: u32,
    pub matched_notes: u32,
    pub unmatched_notes: Vec<UnmatchedNote>,
    pub matched_examples: Vec<MatchedNote>,
    pub coverage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmatchedNote {
    pub note_id: String,
    pub kind: String,
    pub content_excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedNote {
    pub note_id: String,
    pub kind: String,
    pub content_excerpt: String,
    pub matched_query: String,
}

pub fn analyze(run1: &Manifest, run2: &Manifest) -> AuditReport {
    let substantive: Vec<&NoteRow> = run1
        .notes
        .decisions
        .iter()
        .chain(run1.notes.invariants.iter())
        .chain(run1.notes.uncertainties.iter())
        .collect();

    let queries = extract_notes_queries(run2);

    let mut matched: Vec<MatchedNote> = Vec::new();
    let mut matched_ids: HashSet<String> = HashSet::new();
    for note in &substantive {
        let needles = note_terms(note);
        for q in &queries {
            if query_overlaps(q, &needles) {
                matched_ids.insert(note.id.clone());
                matched.push(MatchedNote {
                    note_id: note.id.clone(),
                    kind: note.kind.clone(),
                    content_excerpt: excerpt(&note.content, 200),
                    matched_query: q.clone(),
                });
                break;
            }
        }
    }

    let unmatched: Vec<UnmatchedNote> = substantive
        .iter()
        .filter(|n| !matched_ids.contains(&n.id))
        .map(|n| UnmatchedNote {
            note_id: n.id.clone(),
            kind: n.kind.clone(),
            content_excerpt: excerpt(&n.content, 200),
        })
        .collect();

    let coverage = if substantive.is_empty() {
        0.0
    } else {
        matched_ids.len() as f64 / substantive.len() as f64
    };

    AuditReport {
        run1_substantive_notes: substantive.len() as u32,
        run2_notes_queries: queries.len() as u32,
        matched_notes: matched_ids.len() as u32,
        unmatched_notes: unmatched,
        matched_examples: matched.into_iter().take(20).collect(),
        coverage,
    }
}

fn extract_notes_queries(run2: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    for ev in &run2.tool_calls {
        if ev.phase != "before" {
            continue;
        }
        match ev.tool_name.as_str() {
            "notes" | "read_notes" => {}
            _ => continue,
        }
        let Some(args) = ev.args_json.as_deref() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
            continue;
        };
        if let Some(q) = v.get("query").and_then(|q| q.as_str()) {
            if !q.is_empty() {
                out.push(q.to_string());
            }
        }
    }
    out
}

fn note_terms(n: &NoteRow) -> Vec<String> {
    let mut terms: Vec<String> = n
        .content
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.len() >= 4)
        .filter(|w| !is_stopword(w))
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

fn query_overlaps(query: &str, note_terms: &[String]) -> bool {
    let qterms: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.len() >= 4)
        .filter(|w| !is_stopword(w))
        .collect();
    if qterms.is_empty() {
        return false;
    }
    let note_set: HashSet<&String> = note_terms.iter().collect();
    qterms.iter().any(|q| note_set.contains(q))
}

fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "this"
            | "that"
            | "with"
            | "from"
            | "have"
            | "been"
            | "they"
            | "them"
            | "their"
            | "would"
            | "could"
            | "should"
            | "where"
            | "which"
            | "when"
            | "what"
            | "into"
            | "than"
            | "then"
            | "your"
            | "about"
    )
}

fn excerpt(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::*;

    fn skel() -> Manifest {
        Manifest {
            schema_version: 1,
            run: RunInfo {
                run_id: "r".into(),
                feature_id: "f".into(),
                milestone_id: "m".into(),
                driver: "opencode".into(),
                session_id: None,
                started_at: 0,
                ended_at: None,
                exit_code: None,
                stop_passed: None,
                mode: "normal".into(),
                stop_stdout: None,
            },
            experiment_repo: ExperimentRepo {
                root: std::path::PathBuf::new(),
                charter_path: None,
                charter_sha256: None,
                spec_shas: vec![],
                git_head: None,
            },
            models: vec![],
            opencode_version: None,
            tool_calls: vec![],
            notes: NotesByKind::default(),
            generated_at_unix: 0,
        }
    }

    fn note(id: &str, kind: &str, content: &str) -> NoteRow {
        NoteRow {
            id: id.into(),
            kind: kind.into(),
            content: content.into(),
            created_at: 0,
            tool_name: None,
            source: None,
            feature_id: None,
            scope: None,
        }
    }

    fn ev(tool: &str, args: &str) -> ToolCallEvent {
        ToolCallEvent {
            event_id: "e".into(),
            call_id: "c".into(),
            tool_name: tool.into(),
            phase: "before".into(),
            args_json: Some(args.into()),
            outcome: None,
            duration_ms: None,
            fired_at: 0,
        }
    }

    #[test]
    fn coverage_full_when_query_overlaps() {
        let mut r1 = skel();
        r1.notes.decisions.push(note(
            "n1",
            "decision",
            "We picked extension hint over property because of forward compat",
        ));
        let mut r2 = skel();
        r2.tool_calls
            .push(ev("notes", r#"{"query":"extension hint forward compat"}"#));
        let r = analyze(&r1, &r2);
        assert_eq!(r.run1_substantive_notes, 1);
        assert_eq!(r.run2_notes_queries, 1);
        assert_eq!(r.matched_notes, 1);
        assert!((r.coverage - 1.0).abs() < 1e-9);
    }

    #[test]
    fn coverage_zero_when_no_queries() {
        let mut r1 = skel();
        r1.notes.invariants.push(note(
            "n1",
            "invariant",
            "Hard gates eliminate before scoring",
        ));
        let r2 = skel();
        let r = analyze(&r1, &r2);
        assert_eq!(r.matched_notes, 0);
        assert_eq!(r.coverage, 0.0);
    }

    #[test]
    fn legacy_read_notes_alias_recognized() {
        let mut r1 = skel();
        r1.notes
            .decisions
            .push(note("n1", "decision", "blending math uses failure rate"));
        let mut r2 = skel();
        r2.tool_calls
            .push(ev("read_notes", r#"{"query":"blending failure"}"#));
        let r = analyze(&r1, &r2);
        assert_eq!(r.matched_notes, 1);
    }
}
