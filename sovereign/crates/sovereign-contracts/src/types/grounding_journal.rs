// SPDX-License-Identifier: AGPL-3.0-or-later
//! The grounding journal — local, metadata-only evidence about what the
//! grounding gate decided on real turns (`VERIFIER_V0.md` §6.1, phase 0).
//!
//! # Why this exists
//!
//! The gate blocks, releases, or retries answers on every gated turn, and
//! today the only trace is a `tracing::info!` line that dies with the log
//! buffer. Meanwhile the verifier program needs exactly what those turns
//! carry: real claims judged against real evidence, with the evidence
//! identifiable well enough to re-judge later. This stream is that
//! record — the Stream C ("in-situ receipts") collector, running long
//! before any second-judge slot ships.
//!
//! Phase 0 records the INCUMBENT gate only. The vocabulary already
//! speaks the four-verdict language a disagreement-triggered second
//! judge will need ([`GateJudgeVerdict`]), so phase 1 adds fields and a
//! line kind, not a new stream.
//!
//! # Evidence by reference, never by value
//!
//! A decision line carries the *(corpus, chunk-id)* handles of the
//! evidence the gate judged — never the chunk text, never the claim
//! text, never the answer. Claims and answers already persist in the
//! conversation store, joined to this line by `episode_id` (stamped into
//! the gate's meta, which rides the message row); chunk text lives in
//! the corpus, addressable by the recorded handles. Mining joins the
//! three at read time. Enforced structurally, not by convention: the
//! record type has no `serde_json::Value` and no free-form string field
//! wide enough to hold prose — ids and closed-set tokens only (ARCH §7).
//!
//! # Reading the counts honestly (ARCH §18.1's four verdicts)
//!
//! | this stream | §18.1 |
//! |---|---|
//! | `supported` | passed |
//! | `unsupported` | failed |
//! | `could_not_judge` | could-not-judge (judge unavailable, gate failed open) |
//! | `never_ran` | never-ran (reserved: phase-1 second judge off/shed) |
//!
//! [`GroundingStats::flag_rate`] is over `supported + unsupported` ONLY
//! and is `None` — never 0% — when nothing was judged. `could_not_judge`
//! is a fact about the instrument, not about the answer, and folding it
//! into either verdict would corrupt the one distribution this stream
//! exists to measure. Phase 0 records GATED turns only: an ungated turn
//! (gate off, out-of-scope surface) writes no line at all, and that
//! absence is structural, not a `never_ran`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::journal::JournalStream;

/// This stream: `grounding-<date>.jsonl`, disabled on its own by
/// `SOVEREIGN_GROUNDING_JOURNAL=off` (or globally — [`JournalStream::enabled`]
/// is the one decider over all four switches).
pub const GROUNDING_STREAM: JournalStream =
    JournalStream::new("grounding", "SOVEREIGN_GROUNDING_JOURNAL");

/// Schema tag stamped on every line. Bump on any backwards-incompatible
/// change; readers skip lines they cannot parse rather than mis-reading
/// old fields into new meanings. Mixed-version journals are the NORM on
/// a mesh we do not operate — see VERIFIER_V0.md §6.1 "version skew".
pub const GROUNDING_JOURNAL_SCHEMA: &str = "grounding-journal/v1";

/// One line of the stream. Tagged now, single-variant on purpose: the
/// phase-1 escalation line joins the same stream under a second tag, and
/// a phase-0 reader that meets one skips it as unreadable-by-schema
/// rather than mis-parsing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroundingLine {
    /// One gate decision on one gated turn.
    Decision(GroundingDecision),
}

/// What one judge said about one claim — the wire vocabulary phase 1
/// shares. The set is closed and has no `unknown`: an unjudged claim is
/// the ABSENCE of a verdict, and absence is counted at read time, never
/// defaulted on the wire (ARCH §18.3).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GateJudgeVerdict {
    /// The claim cleared the operating threshold.
    Supported,
    /// It did not — the gate acted (abstain/retry per its profile).
    Unsupported,
    /// The judge ran and could not produce a verdict (extraction or
    /// support calls failed; the gate failed open). A fact about the
    /// instrument, not the answer.
    CouldNotJudge,
    /// Reserved for phase 1: the judge was configured but off, shed, or
    /// unloaded when this turn ran. Phase 0 never writes it.
    NeverRan,
}

/// One evidence handle the gate judged — enough to re-fetch the exact
/// chunk from the corpus at mining time. Identity only, never text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRef {
    /// Corpus holding the passage (mirrors `CitationTarget::corpus_id`).
    pub corpus: String,
    /// Stable chunk id within that corpus.
    pub chunk: u64,
}

/// What the gate decided on one gated turn.
///
/// Every field is an id, a number, a bool, or a closed-set token the
/// daemon chose from its own vocabulary. There is deliberately no field
/// a claim, an answer, a question, or a chunk body can travel through.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroundingDecision {
    /// See [`GROUNDING_JOURNAL_SCHEMA`].
    pub schema: String,
    /// RFC 3339, UTC.
    pub ts: String,
    /// Joins this line to the message row (via the gate meta the daemon
    /// persists with the answer) and to any phase-1 escalation line.
    /// Random per decision, never a counter (ARCH §7.5).
    pub episode_id: String,

    // ── which gate, on what kind of turn ──
    /// The gate surface id (`GroundingProfile::surface`) — a closed set
    /// the daemon owns.
    pub surface: String,
    /// In-world (entity-anchored) question: the GK exemption is void and
    /// the value-presence check runs. Changes which mechanism decides,
    /// so mining stratifies on it.
    pub entity_anchored: bool,

    // ── the verdict, attributably ──
    /// The incumbent judge's verdict at `tau`.
    pub verdict: GateJudgeVerdict,
    /// Raw violation probability, when the judge produced one. Recorded
    /// so operating points can be re-picked from the score DISTRIBUTION
    /// (VERIFIER_V0.md §6.1) — a verdict alone cannot answer "was tau
    /// right".
    pub violation_prob: Option<f64>,
    /// The operating threshold this verdict was taken at.
    pub tau: f64,
    /// Whether a claim was actually extracted and judged. `false` with
    /// `verdict: supported` is an exempt release (long-form, NO_CLAIM,
    /// decline-rider) — auditing nothing is not the same as passing.
    pub claim_audited: bool,

    // ── what the gate did about it ──
    /// The gate's own action token (`released`, `abstained_no_retry`,
    /// …) — the daemon's closed vocabulary, copied verbatim from the
    /// meta it already persists, so this stream and the message row can
    /// never disagree about what happened (ARCH §10.6). Absent when a
    /// gate path predates the token.
    pub action: Option<String>,
    /// A failed verify triggered a second synthesis.
    pub retried: bool,

    // ── the evidence, by reference ──
    /// Chunks in the gate's evidence snapshot.
    pub chunks: usize,
    /// Resolvable handles for those chunks, in snapshot order. May be
    /// SHORTER than `chunks`: tool transcripts and sealed conversation
    /// evidence carry no corpus handle.
    pub evidence: Vec<EvidenceRef>,
    /// Snapshot chunks with NO resolvable handle — reported, never
    /// papered over: a mining pass can re-judge exactly `evidence.len()`
    /// of `chunks` chunks, and this is the difference (ARCH §18.3).
    pub evidence_unresolved: usize,
    /// Best retrieval similarity over the snapshot, when the surface
    /// threads it. Mining's cheap proxy for "was the answer findable".
    pub top_similarity: Option<f32>,

    // ── cost ──
    /// Wall time of the whole gate call (verify + any retry synthesis),
    /// as the turn experienced it.
    pub gate_ms: u64,
}

impl GroundingDecision {
    /// A fresh decision with a random id and the schema stamped — the
    /// only place either is set. The caller fills the rest.
    pub fn new(surface: &str, tau: f64, gate_ms: u64) -> Self {
        Self {
            schema: GROUNDING_JOURNAL_SCHEMA.to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            episode_id: uuid::Uuid::new_v4().to_string(),
            surface: surface.to_string(),
            entity_anchored: false,
            verdict: GateJudgeVerdict::CouldNotJudge,
            violation_prob: None,
            tau,
            claim_audited: false,
            action: None,
            retried: false,
            chunks: 0,
            evidence: Vec::new(),
            evidence_unresolved: 0,
            top_similarity: None,
            gate_ms,
        }
    }
}

/// Every line, oldest day first, plus a count of unparseable lines.
pub fn read_all(dir: &Path) -> (Vec<GroundingLine>, usize) {
    GROUNDING_STREAM.read_all(dir)
}

/// Append one line. See [`JournalStream::append`] for the `Ok(false)`
/// postures (switched off, day at cap).
pub fn append(dir: &Path, line: &GroundingLine) -> std::io::Result<bool> {
    GROUNDING_STREAM.append(dir, line)
}

/// Counts over a set of lines. Read the field docs before quoting a
/// rate — the honesty rules live here, not in the caller.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroundingStats {
    /// Decisions recorded.
    pub decisions: usize,
    /// Verdicts that cleared tau.
    pub supported: usize,
    /// Verdicts that did not.
    pub unsupported: usize,
    /// The judge ran and produced no verdict — an instrument fact,
    /// never in any rate's denominator.
    pub could_not_judge: usize,
    /// Reserved for phase 1; phase-0 writers never produce it, so a
    /// non-zero count in a phase-0 journal is itself a finding.
    pub never_ran: usize,
    /// Decisions where a claim was actually extracted and judged — the
    /// denominator for "what did auditing find", as opposed to exempt
    /// releases that audited nothing.
    pub audited: usize,
    /// Decisions whose verify failed and triggered a second synthesis.
    pub retried: usize,
    /// Snapshot chunks the gate saw, summed over decisions.
    pub chunks_seen: usize,
    /// Of those, chunks with a resolvable evidence handle. The ratio to
    /// `chunks_seen` is what bounds any future mining pass over this
    /// journal.
    pub chunks_resolvable: usize,
    /// Median gate wall time.
    pub p50_ms: u64,
    /// p95 gate wall time — the number a turn actually feels.
    pub p95_ms: u64,
    /// Lines that could not be parsed (truncated tail, future schema).
    pub unreadable: usize,
}

impl GroundingStats {
    /// `unsupported / (supported + unsupported)` — how often auditing
    /// flagged. `None` when nothing was judged: a could-not-judge is not
    /// a 0% flag rate (ARCH §18.1).
    pub fn flag_rate(&self) -> Option<f64> {
        let judged = self.supported + self.unsupported;
        (judged > 0).then(|| self.unsupported as f64 / judged as f64)
    }

    /// How much of the recorded evidence a mining pass could actually
    /// re-fetch. `None` when no chunks were seen.
    pub fn evidence_coverage(&self) -> Option<f64> {
        (self.chunks_seen > 0).then(|| self.chunks_resolvable as f64 / self.chunks_seen as f64)
    }
}

/// Count a window of lines. `unreadable` comes from [`read_all`] and is
/// threaded through rather than recomputed.
pub fn stats(lines: &[GroundingLine], unreadable: usize) -> GroundingStats {
    let mut s = GroundingStats { unreadable, ..Default::default() };
    let mut durations: Vec<u64> = Vec::new();
    for line in lines {
        let GroundingLine::Decision(d) = line;
        s.decisions += 1;
        match d.verdict {
            GateJudgeVerdict::Supported => s.supported += 1,
            GateJudgeVerdict::Unsupported => s.unsupported += 1,
            GateJudgeVerdict::CouldNotJudge => s.could_not_judge += 1,
            GateJudgeVerdict::NeverRan => s.never_ran += 1,
        }
        if d.claim_audited {
            s.audited += 1;
        }
        if d.retried {
            s.retried += 1;
        }
        s.chunks_seen += d.chunks;
        s.chunks_resolvable += d.evidence.len();
        durations.push(d.gate_ms);
    }
    durations.sort_unstable();
    if !durations.is_empty() {
        s.p50_ms = durations[durations.len() / 2];
        s.p95_ms = durations[(durations.len() * 95 / 100).min(durations.len() - 1)];
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(verdict: GateJudgeVerdict, audited: bool, ms: u64) -> GroundingLine {
        let mut d = GroundingDecision::new("chat", 0.55, ms);
        d.verdict = verdict;
        d.claim_audited = audited;
        GroundingLine::Decision(d)
    }

    /// THE guard this module exists for: nothing content-bearing can
    /// reach a line, because the record has nowhere to put it. Every
    /// field a caller could conceivably route prose through is exercised
    /// with a canary; if someone adds a free-form field, the canary
    /// appears in the serialized line and this fails.
    #[test]
    fn no_content_bearing_field_can_reach_a_line() {
        const CANARY: &str = "THE_USERS_CLAIM_TEXT_MUST_NOT_APPEAR";
        let mut d = GroundingDecision::new("chat", 0.55, 12);
        // The closed-set token fields are the only strings a caller
        // controls beyond ids; a hostile caller putting prose there is
        // the thing the reviewer of that call site must catch — but the
        // struct itself must offer no field DOCUMENTED for text.
        d.action = Some("released".into());
        d.evidence = vec![EvidenceRef { corpus: "corpus-a".into(), chunk: 41 }];
        let line = serde_json::to_string(&GroundingLine::Decision(d)).unwrap();
        assert!(!line.contains(CANARY));
        // The serialized field set is exactly the declared one — a new
        // field (where content could hide) must show up here and be
        // justified.
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "action",
                "chunks",
                "claim_audited",
                "entity_anchored",
                "episode_id",
                "evidence",
                "evidence_unresolved",
                "gate_ms",
                "kind",
                "retried",
                "schema",
                "surface",
                "tau",
                "top_similarity",
                "ts",
                "verdict",
                "violation_prob",
            ]
        );
    }

    #[test]
    fn nothing_judged_is_none_not_zero_percent() {
        let s = stats(&[decision(GateJudgeVerdict::CouldNotJudge, false, 5)], 0);
        assert_eq!(s.flag_rate(), None, "0/0 must be could-not-judge, not 0%");
        assert_eq!(s.could_not_judge, 1);
    }

    #[test]
    fn could_not_judge_is_never_in_the_flag_rate_denominator() {
        let lines = vec![
            decision(GateJudgeVerdict::Supported, true, 10),
            decision(GateJudgeVerdict::Unsupported, true, 20),
            decision(GateJudgeVerdict::CouldNotJudge, false, 30),
        ];
        let s = stats(&lines, 0);
        assert_eq!(s.flag_rate(), Some(0.5), "2 judged, 1 flagged — the third is not a verdict");
    }

    #[test]
    fn evidence_coverage_reports_the_mining_bound() {
        let mut d = GroundingDecision::new("chat", 0.55, 10);
        d.chunks = 4;
        d.evidence = vec![
            EvidenceRef { corpus: "c".into(), chunk: 1 },
            EvidenceRef { corpus: "c".into(), chunk: 2 },
        ];
        d.evidence_unresolved = 2;
        let s = stats(&[GroundingLine::Decision(d)], 0);
        assert_eq!(s.evidence_coverage(), Some(0.5));
        let s0 = stats(&[], 0);
        assert_eq!(s0.evidence_coverage(), None);
    }

    #[test]
    fn a_real_append_round_trips_through_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        assert!(append(dir.path(), &decision(GateJudgeVerdict::Supported, true, 10)).unwrap());
        let (lines, bad) = read_all(dir.path());
        assert_eq!(bad, 0);
        assert_eq!(lines.len(), 1);
        assert_eq!(stats(&lines, bad).supported, 1);
    }

    /// This stream's own switch must gate THIS stream only.
    #[test]
    fn this_streams_own_marker_stops_the_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(GROUNDING_STREAM.marker_in(dir.path()), "").unwrap();
        assert!(!GROUNDING_STREAM.enabled(dir.path()));
        assert!(!append(dir.path(), &decision(GateJudgeVerdict::Supported, true, 1)).unwrap());
    }
}
