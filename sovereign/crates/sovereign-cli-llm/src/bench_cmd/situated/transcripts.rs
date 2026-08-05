// SPDX-License-Identifier: AGPL-3.0-or-later
//! Chaos-run transcripts — the lane's input.
//!
//! **The situated lane does not generate.** It scores what the chaos bench
//! already produced by driving the production turn (routing → retrieval →
//! synthesis → grounding gate) and persisting one JSONL row per probe.
//!
//! That is the production-path mandate satisfied structurally rather than by
//! discipline (`SITUATED_FLYWHEEL.md` §"Production-path mandate"): there is
//! no bench-local chat loop here, so there is nowhere for a bench-only
//! scaffold to live, and no second generation path to drift from the real
//! one. A harness change that moves this lane's numbers moved the shipped
//! turn, because the shipped turn is what wrote the transcript.

use std::path::Path;

use serde::Deserialize;
use sovereign_eval::chaos_monkey::QuestionType;

/// One probe's turn, as the chaos runner banked it. Deliberately a SUBSET of
/// the row's fields — the lane reads the response and the probe's identity
/// and nothing else, so adding fields chaos-side never breaks it.
#[derive(Debug, Clone, Deserialize)]
pub struct Transcript {
    pub id: String,
    pub qtype: QuestionType,
    #[serde(default)]
    pub question: String,
    /// The visible response the reader got — post-gate, the real artifact.
    #[serde(default)]
    pub answer: String,
    /// The gate's own persisted action, carried through for the report
    /// header so a run can be read against the configuration that produced
    /// it. Not judged.
    #[serde(default)]
    pub gate_action: Option<String>,
}

/// Load a chaos `*.transcripts.jsonl`. A malformed line is reported and
/// skipped rather than aborting the load — but the count of skipped lines
/// is returned, never swallowed, because a run scored over half its bank is
/// not the run you think you are reading.
pub fn load(path: &Path) -> Result<(Vec<Transcript>, usize), String> {
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for (n, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Transcript>(line) {
            Ok(t) => rows.push(t),
            Err(e) => {
                skipped += 1;
                eprintln!("bench situated: {}:{} unreadable — {e}", path.display(), n + 1);
            }
        }
    }
    // Stable order keeps reports comparable across runs.
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((rows, skipped))
}

impl Transcript {
    /// A transcript with no visible answer cannot be judged on any
    /// criterion. Reported as could-not-judge rather than scored zero —
    /// absence is disclosed, never defaulted (ARCH_PRINCIPLES §18.3).
    pub fn is_judgeable(&self) -> bool {
        !self.answer.trim().is_empty()
    }

    /// The artifact handed to the judge: the question AND the response.
    ///
    /// Several criteria are unjudgeable from the response alone —
    /// "answers the question that was asked" is meaningless without the
    /// question, and "declines when the sources do not support an answer"
    /// reads differently against an in-domain probe than an out-of-domain
    /// one. The moral lane gets away with response-only because a dilemma
    /// response restates its own situation; a one-line factual answer does
    /// not.
    ///
    /// Composed here rather than by extending the shared judge protocol:
    /// that would change the instrument every lane shares and invalidate
    /// the moral lane's calibration. The cost is that this lane's
    /// calibration items must be authored in this same two-part shape.
    pub fn judged_text(&self) -> String {
        if self.question.trim().is_empty() {
            return self.answer.clone();
        }
        format!("Question asked:\n{}\n\nResponse:\n{}", self.question.trim(), self.answer.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.transcripts.jsonl");
        std::fs::write(&p, body).unwrap();
        (dir, p)
    }

    #[test]
    fn reads_the_chaos_row_shape_and_ignores_extra_fields() {
        // Exactly the shape chaos writes, extra keys included.
        let line = r#"{"id":"present-wife","qtype":"present","question":"who?","expected_action":"Answer","agent_action":"Abstained","pass":false,"answer":"I could not confirm...","retrieved_chunks":["a"],"gate_action":"abstained_specifics","draft":null,"epistemic_state":{"version":1}}"#;
        let (_d, p) = tmp(line);
        let (rows, skipped) = load(&p).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].qtype, QuestionType::Present);
        assert_eq!(rows[0].gate_action.as_deref(), Some("abstained_specifics"));
        assert!(rows[0].is_judgeable());
    }

    #[test]
    fn malformed_lines_are_counted_not_swallowed() {
        let (_d, p) = tmp(
            "{\"id\":\"a\",\"qtype\":\"present\",\"answer\":\"x\"}\nnot json\n\n{\"id\":\"b\",\"qtype\":\"absent_adjacent\",\"answer\":\"y\"}\n",
        );
        let (rows, skipped) = load(&p).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(skipped, 1, "a dropped row must be reported, not silently lost");
    }

    #[test]
    fn judged_text_carries_the_question() {
        let (_d, p) = tmp(
            "{\"id\":\"a\",\"qtype\":\"present\",\"question\":\"Who is the wife?\",\"answer\":\"Winnie.\"}",
        );
        let (rows, _) = load(&p).unwrap();
        let t = rows[0].judged_text();
        assert!(t.contains("Who is the wife?"), "criteria about the ASK need the ask: {t}");
        assert!(t.contains("Winnie."));
        // A transcript predating the question field still judges, on the
        // response alone, rather than emitting a dangling empty header.
        let (_d, p) = tmp("{\"id\":\"a\",\"qtype\":\"present\",\"answer\":\"Winnie.\"}");
        let (rows, _) = load(&p).unwrap();
        assert_eq!(rows[0].judged_text(), "Winnie.");
    }

    #[test]
    fn empty_answer_is_not_judgeable() {
        let (_d, p) = tmp("{\"id\":\"a\",\"qtype\":\"present\",\"answer\":\"   \"}");
        let (rows, _) = load(&p).unwrap();
        assert!(!rows[0].is_judgeable());
    }

    #[test]
    fn rows_are_sorted_so_reports_compare() {
        let (_d, p) = tmp(
            "{\"id\":\"z\",\"qtype\":\"present\",\"answer\":\"x\"}\n{\"id\":\"a\",\"qtype\":\"present\",\"answer\":\"y\"}\n",
        );
        let (rows, _) = load(&p).unwrap();
        assert_eq!(rows[0].id, "a");
    }
}
