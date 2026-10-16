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
        "one decision per gated answer and per claim `verify` judged: verdict, score, threshold, \
         what decided, the (corpus, chunk-id) handles of the evidence judged, and clients' \
         corrections by episode — never the claim, answer, or chunk text",
    stream: GROUNDING_STREAM,
    stats: render_stats,
    show: render_show,
};

fn render_stats(dir: &Path) -> Vec<String> {
    let (lines, unreadable) = grounding_journal_read_all(dir);
    let all = grounding_journal_stats(&lines, unreadable);
    if all.decisions == 0 && all.corrections == 0 && all.unreadable == 0 {
        return vec!["  no decisions recorded yet".into()];
    }
    // One block per surface: the in-turn gate and `oicp_verify` judge
    // different units at different thresholds, and one flag rate over both
    // would describe neither (VERIFICATION_COMMONS.md §2 keys an episode by
    // its surface).
    let mut surfaces: Vec<&str> = lines
        .iter()
        .filter_map(|l| match l {
            GroundingLine::Decision(d) => Some(d.surface.as_str()),
            GroundingLine::Correction(_) => None,
        })
        .collect();
    surfaces.sort_unstable();
    surfaces.dedup();
    let mut out = Vec::new();
    for surface in surfaces {
        let mine: Vec<GroundingLine> = lines
            .iter()
            .filter(|l| matches!(l, GroundingLine::Decision(d) if d.surface == surface))
            .cloned()
            .collect();
        out.push(format!("  surface {surface}"));
        out.extend(render_surface(&grounding_journal_stats(&mine, 0)));
        let split = mine
            .iter()
            .filter(|l| {
                matches!(l, GroundingLine::Decision(d)
                    if d.verdict == GateJudgeVerdict::Unsupported
                        && d.specifics.is_some_and(|t| t.checked > 0 && t.absent == 0))
            })
            .count();
        if split > 0 {
            out.push(format!(
                "  {split} unsupported with every specific present: facts in the sources, no \
                 single passage that carries the claim"
            ));
        }
    }
    if all.corrections > 0 {
        out.push(format!(
            "  {} correction(s) from clients (`show` lists them)",
            all.corrections
        ));
    }
    if all.unreadable > 0 {
        out.push(format!(
            "  {} unreadable line(s) — skipped, never guessed at",
            all.unreadable
        ));
    }
    out
}

fn render_surface(s: &sovereign_contracts::types::GroundingStats) -> Vec<String> {
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
            let early = if s.supported + s.unsupported < 20 {
                " (early signal — under 20 judged)"
            } else {
                ""
            };
            out.push(format!(
                "  flag rate: {:.1}% of judged claims{early}",
                r * 100.0
            ));
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
    out.push(format!(
        "  gate wall: p50 {}ms · p95 {}ms",
        s.p50_ms, s.p95_ms
    ));
    out
}

fn render_show(dir: &Path, last: usize) -> Vec<String> {
    let (lines, unreadable) = grounding_journal_read_all(dir);
    let mut out = Vec::new();
    let start = lines.len().saturating_sub(last);
    for line in &lines[start..] {
        let d = match line {
            GroundingLine::Decision(d) => d,
            GroundingLine::Correction(c) => {
                out.push(format!(
                    "  {} · correction of {} · expected {:?} · witness {:?}",
                    c.ts, c.episode_id, c.expected, c.witness_check
                ));
                continue;
            }
        };
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
            d.violation_prob
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "—".into()),
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
    use sovereign_contracts::types::{grounding_journal_append, GroundingDecisionLine};

    fn seed(dir: &Path, verdict: GateJudgeVerdict) {
        let mut d = GroundingDecisionLine::new("chat", 0.55, 420);
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

    /// Two surfaces, two blocks: a gate turn and a verify claim at different
    /// thresholds are never one flag rate. A verify claim unsupported with
    /// every specific present is counted as the split-evidence case, and a
    /// correction is counted beside, never as a decision.
    #[test]
    fn each_surface_is_its_own_block_and_corrections_are_counted_apart() {
        use sovereign_contracts::types::{
            CorrectedVerdict, GroundingCorrectionLine, SpecificsTally, WitnessCheck,
        };
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), GateJudgeVerdict::Supported);
        let mut v = GroundingDecisionLine::new("oicp_verify", 0.5, 2400);
        v.verdict = GateJudgeVerdict::Unsupported;
        v.claim_audited = true;
        v.specifics = Some(SpecificsTally {
            checked: 2,
            absent: 0,
        });
        let episode = v.episode_id.clone();
        grounding_journal_append(dir.path(), &GroundingLine::Decision(v)).unwrap();
        grounding_journal_append(
            dir.path(),
            &GroundingLine::Correction(GroundingCorrectionLine::new(
                &episode,
                CorrectedVerdict::Supported,
                None,
                WitnessCheck::NotGiven,
            )),
        )
        .unwrap();
        let text = render_stats(dir.path()).join("\n");
        assert!(text.contains("surface chat"), "{text}");
        assert!(text.contains("surface oicp_verify"), "{text}");
        assert_eq!(text.matches("flag rate").count(), 2, "{text}");
        assert!(
            text.contains("1 unsupported with every specific present"),
            "{text}"
        );
        assert!(text.contains("1 correction(s)"), "{text}");
        let shown = render_show(dir.path(), 10).join("\n");
        assert!(
            shown.contains(&format!("correction of {episode}")),
            "{shown}"
        );
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
