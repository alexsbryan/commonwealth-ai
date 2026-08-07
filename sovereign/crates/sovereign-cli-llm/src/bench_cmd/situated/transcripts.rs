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
    /// How many released quotes named a section, as the GATE decided it.
    ///
    /// This is the structural answer to `cites_a_source`, and it is read, not
    /// judged. The gate resolves locators through the corpus's chunk→section
    /// join and knows exactly how many it emitted; asking a judge to recover
    /// that from prose measured 5/7 against this field's 7/7 (2026-08-05),
    /// and the two it lost were the two answers that ALSO disclosed a gap —
    /// so the judged version actively penalised the behaviour this lane
    /// exists to reward.
    ///
    /// `Some(0)` covers every turn that named no section — whether the
    /// citation path released without a locator, or the turn abstained or took
    /// the legacy ladder and released no citation at all. All of those are
    /// genuine misses on this criterion, and chaos always writes a number.
    ///
    /// `None` therefore has exactly ONE meaning: a transcript banked before
    /// this field existed. Only that case is could-not-judge. Conflating the
    /// two is not academic — it was tried first and reported `cites_a_source`
    /// as 100% over a denominator of 3 instead of 3/7, because four legitimate
    /// misses were being dropped from the denominator rather than counted
    /// (caught on the verification run, 2026-08-05).
    #[serde(default)]
    pub citation_located: Option<u64>,
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
                eprintln!(
                    "bench situated: {}:{} unreadable — {e}",
                    path.display(),
                    n + 1
                );
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

    /// `cites_a_source`, READ from the gate rather than judged.
    ///
    /// `Some(true)` when the release named at least one section; `Some(false)`
    /// when the citation path released but had nothing nameable to point at;
    /// `None` when the turn produced no structural answer at all, which the
    /// caller must surface as could-not-judge rather than as a miss (§18.3 —
    /// absence is reported, never defaulted).
    ///
    /// One decider (§10.6): the gate resolves the chunk→section join and is
    /// the only thing that knows what it emitted. Re-deriving that from prose
    /// is what produced the disclosure penalty this replaced.
    pub fn cites_a_source(&self) -> Option<bool> {
        self.citation_located.map(|n| n > 0)
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
        format!(
            "Question asked:\n{}\n\nResponse:\n{}",
            self.question.trim(),
            self.answer.trim()
        )
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

    /// The three states must stay DISTINCT. Collapsing `None` into `false` is
    /// the §18.3 violation this accessor exists to prevent: a transcript
    /// banked before the gate persisted the count would silently score as
    /// "did not cite" and read as a regression that never happened.
    #[test]
    fn absent_locator_count_is_unknown_not_a_miss() {
        let row = |located: Option<u64>| Transcript {
            id: "p".into(),
            qtype: QuestionType::Present,
            question: "q".into(),
            answer: "a".into(),
            gate_action: None,
            citation_located: located,
        };
        assert_eq!(
            row(None).cites_a_source(),
            None,
            "ONLY a pre-field transcript is unknown"
        );
        assert_eq!(
            row(Some(0)).cites_a_source(),
            Some(false),
            "named no section — a real miss, and it MUST stay in the denominator"
        );
        assert_eq!(row(Some(1)).cites_a_source(), Some(true));
        assert_eq!(row(Some(2)).cites_a_source(), Some(true));
    }

    /// Both arms: the field the live gate writes must be read, AND a
    /// transcript predating it must stay unknown rather than becoming a zero.
    #[test]
    fn reads_the_located_count_the_gate_writes() {
        let with = r#"{"id":"a","qtype":"present","question":"q","answer":"x","gate_action":"citation_grounded","citation_located":2}"#;
        let (_d, p) = tmp(with);
        let (rows, _) = load(&p).unwrap();
        assert_eq!(rows[0].cites_a_source(), Some(true));

        // An abstained turn writes 0, NOT null — it stays a judged miss so the
        // criterion keeps its denominator. This is the arm that regressed the
        // reported rate to 100%-of-3 when it was written as null.
        let abstained = r#"{"id":"a","qtype":"present","question":"q","answer":"x","gate_action":"abstained","citation_located":0}"#;
        let (_d, p) = tmp(abstained);
        let (rows, _) = load(&p).unwrap();
        assert_eq!(
            rows[0].cites_a_source(),
            Some(false),
            "an abstention is a miss on this criterion, never a could-not-judge"
        );

        let legacy =
            r#"{"id":"a","qtype":"present","question":"q","answer":"x","gate_action":"released"}"#;
        let (_d, p) = tmp(legacy);
        let (rows, _) = load(&p).unwrap();
        assert_eq!(
            rows[0].cites_a_source(),
            None,
            "a transcript predating the field must be unknown, never a miss"
        );
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
        assert_eq!(
            skipped, 1,
            "a dropped row must be reported, not silently lost"
        );
    }

    #[test]
    fn judged_text_carries_the_question() {
        let (_d, p) = tmp(
            "{\"id\":\"a\",\"qtype\":\"present\",\"question\":\"Who is the wife?\",\"answer\":\"Winnie.\"}",
        );
        let (rows, _) = load(&p).unwrap();
        let t = rows[0].judged_text();
        assert!(
            t.contains("Who is the wife?"),
            "criteria about the ASK need the ask: {t}"
        );
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
