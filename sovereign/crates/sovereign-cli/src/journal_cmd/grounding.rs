// SPDX-License-Identifier: AGPL-3.0-or-later
//! The grounding journal's view row: two renderers over
//! `sovereign_contracts::types::grounding_journal`, nothing else. The
//! machinery — selection, bundle, off/on, clear — is generic in
//! [`super`], written against the registry this row joins.

use std::path::Path;

use sovereign_contracts::types::{
    grounding_journal_read_all, grounding_journal_stats, GateJudgeVerdict, GroundingLine,
    GROUNDING_STREAM,
};

use super::JournalView;

pub const VIEW: JournalView = JournalView {
    name: "grounding",
    title: "Grounding gate",
    records:
        "one decision per gated answer: verdict, score, threshold, what the gate did, and the \
         (corpus, chunk-id) handles of the evidence it judged — never the claim, answer, or \
         chunk text",
    stream: GROUNDING_STREAM,
    stats: render_stats,
    show: render_show,
};

fn render_stats(dir: &Path) -> Vec<String> {
    let (lines, unreadable) = grounding_journal_read_all(dir);
    let s = grounding_journal_stats(&lines, unreadable);
    if s.decisions == 0 && s.unreadable == 0 {
        return vec!["  no decisions recorded yet".into()];
    }
    let mut out = Vec::new();
    out.push(format!(
        "  {} decision(s) · {} audited a claim · {} retried",
        s.decisions, s.audited, s.retried
    ));
    out.push(format!(
        "  verdicts: {} supported · {} unsupported · {} could-not-judge{}",
        s.supported,
        s.unsupported,
        s.could_not_judge,
        if s.never_ran > 0 {
            format!(" · {} never-ran (unexpected in phase 0)", s.never_ran)
        } else {
            String::new()
        }
    ));
    match s.flag_rate() {
        // The minimum-judged rule mirrors next-edit's: a rate over a
        // handful of decisions is an early signal, not a measurement
        // (ARCH §18.5).
        Some(r) => {
            let early = if s.supported + s.unsupported < 20 { " (early signal — under 20 judged)" } else { "" };
            out.push(format!("  flag rate: {:.1}% of judged claims{early}", r * 100.0));
        }
        None => out.push("  flag rate: nothing judged yet — not 0%".into()),
    }
    match s.evidence_coverage() {
        Some(c) => out.push(format!(
            "  evidence handles: {}/{} chunks resolvable ({:.0}%) — the bound on any mining pass",
            s.chunks_resolvable,
            s.chunks_seen,
            c * 100.0
        )),
        None => out.push("  evidence handles: no chunks recorded".into()),
    }
    out.push(format!("  gate wall: p50 {}ms · p95 {}ms", s.p50_ms, s.p95_ms));
    if s.unreadable > 0 {
        out.push(format!("  {} unreadable line(s) — skipped, never guessed at", s.unreadable));
    }
    out
}

fn render_show(dir: &Path, last: usize) -> Vec<String> {
    let (lines, unreadable) = grounding_journal_read_all(dir);
    let mut out = Vec::new();
    let start = lines.len().saturating_sub(last);
    for line in &lines[start..] {
        let GroundingLine::Decision(d) = line;
        let verdict = match d.verdict {
            GateJudgeVerdict::Supported => "supported",
            GateJudgeVerdict::Unsupported => "UNSUPPORTED",
            GateJudgeVerdict::CouldNotJudge => "could-not-judge",
            GateJudgeVerdict::NeverRan => "never-ran",
        };
        out.push(format!(
            "  {} · {} · {} vp={} tau={} · {} · {} chunk(s), {} resolvable · {}ms · {}",
            d.ts,
            d.surface,
            verdict,
            d.violation_prob.map(|v| format!("{v:.3}")).unwrap_or_else(|| "—".into()),
            d.tau,
            d.action.as_deref().unwrap_or("—"),
            d.chunks,
            d.evidence.len(),
            d.gate_ms,
            d.episode_id,
        ));
    }
    if out.is_empty() {
        out.push("  no records".into());
    }
    if unreadable > 0 {
        out.push(format!("  ({unreadable} unreadable line(s) skipped)"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::types::{grounding_journal_append, GroundingDecision};

    fn seed(dir: &Path, verdict: GateJudgeVerdict) {
        let mut d = GroundingDecision::new("chat", 0.55, 420);
        d.verdict = verdict;
        d.claim_audited = matches!(
            verdict,
            GateJudgeVerdict::Supported | GateJudgeVerdict::Unsupported
        );
        grounding_journal_append(dir, &GroundingLine::Decision(d)).unwrap();
    }

    #[test]
    fn stats_render_reports_could_not_judge_apart_from_the_rate() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), GateJudgeVerdict::Supported);
        seed(dir.path(), GateJudgeVerdict::Unsupported);
        seed(dir.path(), GateJudgeVerdict::CouldNotJudge);
        let text = render_stats(dir.path()).join("\n");
        assert!(text.contains("1 could-not-judge"), "{text}");
        assert!(text.contains("flag rate: 50.0%"), "{text}");
        assert!(text.contains("early signal"), "{text}");
    }

    #[test]
    fn an_unjudged_journal_never_prints_a_zero_rate() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), GateJudgeVerdict::CouldNotJudge);
        let text = render_stats(dir.path()).join("\n");
        assert!(text.contains("nothing judged yet — not 0%"), "{text}");
        assert!(!text.contains("0.0%"), "{text}");
    }

    #[test]
    fn show_caps_at_last_and_prints_the_join_id() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..3 {
            seed(dir.path(), GateJudgeVerdict::Supported);
        }
        let out = render_show(dir.path(), 2);
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("supported"));
    }
}
