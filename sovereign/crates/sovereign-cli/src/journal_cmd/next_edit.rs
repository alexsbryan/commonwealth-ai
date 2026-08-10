// SPDX-License-Identifier: AGPL-3.0-or-later
//! The next-edit view of `svrn journal` — how THIS stream's records are
//! counted and rendered.
//!
//! Everything generic (where files live, bundling, the off-switches,
//! clearing) is in [`super`]; everything about the record shapes and the
//! honesty rules for the counts is in
//! `sovereign_contracts::types::next_edit_journal`. What is left here is
//! presentation: two renderers, wired into the registry by [`VIEW`].
//!
//! A second feature's view is a sibling file of this one plus a row in
//! `super::VIEWS`. It does not touch this file, and this file does not
//! know it exists.

use std::path::Path;

use sovereign_contracts::types::{journal_read_all, journal_stats, JournalLine, NEXT_EDIT_STREAM};

use super::JournalView;

/// Below this many judged episodes, the acceptance rate is labelled an
/// early signal rather than a number. Not a statistical threshold — a
/// floor under which a percentage is actively misleading (ARCH §18.5).
const MEASURABLE_JUDGED: usize = 20;

/// The registry row. `super` reads this and never names the module.
pub const VIEW: JournalView = JournalView {
    name: "next-edit",
    title: "Next-edit suggestions",
    records: "why the lane fired or stayed silent, which model answered, and what you did with it",
    stream: NEXT_EDIT_STREAM,
    stats,
    show,
};

fn stats(dir: &Path) -> Vec<String> {
    let (lines, unreadable) = journal_read_all(dir);
    let s = journal_stats(&lines, unreadable);
    let mut out = Vec::new();
    if s.episodes == 0 {
        out.push("  No episodes recorded yet.".into());
        out.push(
            "  The lane records an episode per prediction request, so this stays empty until"
                .into(),
        );
        out.push("  you edit with the extension running against a live daemon.".into());
        return out;
    }

    out.push(format!("  episodes                 {}", s.episodes));
    out.push(format!("  ├─ shown a suggestion    {}", s.shown));
    out.push(format!("  ├─ model lane fired      {}", s.fired));
    out.push(format!("  ├─ model answer dropped  {}", s.dropped));
    out.push(format!("  └─ on a fallback slot    {}", s.degraded));
    out.push(format!(
        "  latency                  p50 {} ms · p95 {} ms",
        s.p50_ms, s.p95_ms
    ));

    out.push(String::new());
    out.push(format!("  what became of the {} shown:", s.shown));
    out.push(format!("    accepted    {}", s.accepted));
    out.push(format!(
        "    dismissed   {}   (Esc — the only explicit no)",
        s.dismissed
    ));
    out.push(format!(
        "    diverged    {}   (you typed on; NOT a rejection)",
        s.diverged
    ));
    out.push(format!(
        "    superseded  {}   (a newer prediction replaced it)",
        s.superseded
    ));
    out.push(format!(
        "    unknown     {}   (never resolved — editor closed, or an older extension)",
        s.unknown
    ));

    // The rate, and immediately the reason to distrust it when that is
    // the honest thing to say. `None` prints as a could-not-judge rather
    // than as 0% (ARCH §18.1).
    out.push(String::new());
    match s.acceptance_rate() {
        None => {
            out.push("  acceptance (accepted / accepted+dismissed):  nothing judged yet".into())
        }
        Some(r) => {
            let judged = s.accepted + s.dismissed;
            out.push(format!(
                "  acceptance (accepted / accepted+dismissed):  {:.0}%  of {judged} judged",
                r * 100.0
            ));
            if let Some(cov) = s.reported_coverage() {
                out.push(format!(
                    "  coverage: {:.0}% of shown episodes reported an outcome",
                    cov * 100.0
                ));
            }
            if judged < MEASURABLE_JUDGED {
                out.push(format!(
                    "  NOTE: {judged} judged episode(s) is not a measurement — it is an early \
                     signal. Come back past {MEASURABLE_JUDGED}."
                ));
            } else if s.reported_coverage().is_some_and(|c| c < 0.5) {
                out.push(
                    "  NOTE: fewer than half the shown episodes reported an outcome, so treat \
                     the rate above as indicative, not measured."
                        .into(),
                );
            }
        }
    }
    if s.orphan_outcomes > 0 {
        out.push(format!(
            "  {} outcome(s) referenced an episode not in this window (a pruned day, usually)",
            s.orphan_outcomes
        ));
    }
    if s.unreadable > 0 {
        out.push(format!(
            "  {} line(s) could not be parsed and were skipped, not guessed at",
            s.unreadable
        ));
    }
    out
}

fn show(dir: &Path, last: usize) -> Vec<String> {
    let (lines, unreadable) = journal_read_all(dir);
    if lines.is_empty() {
        return vec!["  no records".into()];
    }
    let start = lines.len().saturating_sub(last);
    let mut out: Vec<String> = lines[start..].iter().map(render).collect();
    out.push(format!(
        "  {} of {} record(s){}",
        lines.len() - start,
        lines.len(),
        if unreadable > 0 {
            format!(" · {unreadable} unparseable, skipped")
        } else {
            String::new()
        }
    ));
    out
}

/// One record as a line a human can scan. Deliberately hand-rolled
/// rather than pretty-printed JSON — `bundle` is where the exact bytes
/// live, and this view is for reading.
fn render(line: &JournalLine) -> String {
    match line {
        JournalLine::Outcome(o) => {
            format!(
                "{}  outcome  {:<11} {}",
                &o.ts,
                o.outcome.as_str(),
                o.episode_id
            )
        }
        JournalLine::Episode(e) => {
            let mut why = String::new();
            if let Some(s) = &e.silent {
                why.push_str(&format!(" silent={s}"));
            }
            if let Some(r) = &e.reason {
                why.push_str(&format!(" reason={r}"));
            }
            if let Some(s) = &e.skipped {
                why.push_str(&format!(" gate={s}"));
            }
            if let Some(d) = &e.dropped {
                why.push_str(&format!(" dropped={d}"));
            }
            format!(
                "{}  episode  {:<6} {} edit(s) support={} sites={} {}ms .{}{}{}",
                &e.ts,
                e.engine,
                e.proposed,
                e.support,
                e.sites,
                e.total_ms,
                e.path_ext.as_deref().unwrap_or("?"),
                why,
                if e.degraded == Some(true) {
                    " [fallback slot]"
                } else {
                    ""
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::types::{NextEditEpisode, NextEditOutcome, NextEditOutcomeLine};

    fn sample() -> Vec<JournalLine> {
        let mut e = NextEditEpisode::new("model", 2, 1400);
        e.episode_id = "ep-1".into();
        e.support = 2;
        e.sites = 3;
        e.reason = Some("param_insert".into());
        e.region_bytes = Some(512);
        e.degraded = Some(true);
        e.path_ext = Some("rs".into());
        let mut silent = NextEditEpisode::new("rule", 0, 3);
        silent.episode_id = "ep-2".into();
        silent.silent = Some("no_sites".into());
        vec![
            JournalLine::Episode(e),
            JournalLine::Episode(silent),
            JournalLine::Outcome(NextEditOutcomeLine::new(
                "ep-1".into(),
                NextEditOutcome::Accepted,
            )),
        ]
    }

    #[test]
    fn render_never_prints_a_field_the_record_does_not_have() {
        let lines = sample();
        let ep = render(&lines[0]);
        assert!(ep.contains("reason=param_insert"));
        assert!(ep.contains("[fallback slot]"));
        assert!(ep.contains(".rs"));
        let silent = render(&lines[1]);
        assert!(silent.contains("silent=no_sites"));
        assert!(
            !silent.contains("reason="),
            "a rule-lane record has no consult reason"
        );
        assert!(!silent.contains("[fallback slot]"));
        assert!(render(&lines[2]).contains("accepted"));
    }

    /// A rate over a handful of episodes must SAY it is not a
    /// measurement, at the point of quoting it.
    #[test]
    fn a_small_judged_population_is_labelled_not_quoted_bare() {
        let dir = tempfile::tempdir().unwrap();
        for line in sample() {
            NEXT_EDIT_STREAM.append(dir.path(), &line).unwrap();
        }
        let out = stats(dir.path()).join("\n");
        assert!(out.contains("100%  of 1 judged"), "got:\n{out}");
        assert!(out.contains("is not a measurement"), "got:\n{out}");
        // And the unjudged population stays visible beside it.
        assert!(out.contains("diverged"));
        assert!(out.contains("unknown"));
    }

    #[test]
    fn an_empty_stream_says_so_rather_than_printing_zeroes() {
        let dir = tempfile::tempdir().unwrap();
        let out = stats(dir.path()).join("\n");
        assert!(out.contains("No episodes recorded yet"), "got:\n{out}");
        assert!(
            !out.contains("acceptance"),
            "no rate may be quoted over zero episodes"
        );
    }

    #[test]
    fn show_caps_at_last_n_and_says_how_many_of_how_many() {
        let dir = tempfile::tempdir().unwrap();
        for line in sample() {
            NEXT_EDIT_STREAM.append(dir.path(), &line).unwrap();
        }
        let out = show(dir.path(), 2).join("\n");
        assert!(out.contains("2 of 3 record(s)"), "got:\n{out}");
    }
}
