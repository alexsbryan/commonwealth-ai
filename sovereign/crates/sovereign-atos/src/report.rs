//! Pure SQL → markdown report renderer.
//!
//! The five sections Yara reads in her epistemic report, in order:
//!
//! 1. **What was built** — charter milestones crossed with actual
//!    runs. Each milestone gets a ✓ (has a passing normal run) or ⚠
//!    (no passing run, or last run failed).
//! 2. **Tests and stop conditions** — `stop_stdout` from the most
//!    recent passing run per milestone. M3.2 captures this during
//!    `end-milestone`.
//! 3. **What I'm uncertain about** — `uncertainty`-kind notes grouped
//!    by `files` tag.
//! 4. **Where I would look first if this breaks** — `postmortem_pointer`
//!    notes grouped by file then by the leading `file:fn:line`
//!    pointer if present.
//! 5. **Red team findings** — `redteam_finding` notes grouped by
//!    confidence (`high` → `medium` → `low`). Each note's JSON body
//!    is decoded for the `{invariant, status, evidence}` tuple.
//!
//! No LLM calls. Deterministic ordering (notes by `created_at DESC`
//! within group). The same renderer powers all three hook artifacts
//! (`milestone-<n>.md`, `red-team.md`, `epistemic-report.md`) — the
//! caller picks a [`ReportSection`](crate::ReportSection) slice.

use std::collections::BTreeMap;

use corpus_engine_notes::{NoteRow, NoteScope, NoteStore, ScopeFilter};
use corpus_engine_atos::{AtosRunRow, FeatureRow, MilestoneRow};

use crate::{Error, ReportSection, Result};

/// Render a report section for one feature. Called by
/// `LocalAtosOrchestrator::render_report`.
pub async fn render(
    notes: &NoteStore,
    feature: &FeatureRow,
    milestones: &[MilestoneRow],
    runs: &[AtosRunRow],
    section: ReportSection,
) -> Result<String> {
    let feature_notes = fetch_feature_notes(notes, &feature.id).await?;
    match section {
        ReportSection::Milestone(ordinal) => {
            Ok(render_milestone(feature, milestones, runs, ordinal, &feature_notes))
        }
        ReportSection::RedTeam => Ok(render_red_team(feature, runs, &feature_notes)),
        ReportSection::Epistemic | ReportSection::All => {
            Ok(render_full(feature, milestones, runs, &feature_notes))
        }
    }
}

// ─── Per-milestone ───────────────────────────────────────────────────────────

fn render_milestone(
    feature: &FeatureRow,
    milestones: &[MilestoneRow],
    runs: &[AtosRunRow],
    ordinal: i64,
    notes: &[NoteRow],
) -> String {
    let Some(milestone) = milestones.iter().find(|m| m.ordinal == ordinal) else {
        return format!(
            "# {} — milestone {} (not found)\n\n_No milestone with ordinal {ordinal} on feature `{}`._\n",
            feature.id, ordinal, feature.id
        );
    };
    let mut out = String::new();
    out.push_str(&format!(
        "# {} — milestone {} report\n\n",
        feature.id, ordinal
    ));
    out.push_str(&format!("**Title:** {}\n\n", derive_title(&milestone.brief_md)));

    out.push_str("## Stop condition\n\n");
    let stop = extract_stop_condition(&milestone.brief_md);
    let (verdict, latest_run) = milestone_verdict(runs, &milestone.id);
    out.push_str(&format!(
        "`{}` — **{}**\n\n",
        if stop.is_empty() { "(manual review)".into() } else { stop },
        verdict
    ));
    if let Some(run) = latest_run.as_ref() {
        if let Some(stdout) = run.stop_stdout.as_deref().filter(|s| !s.is_empty()) {
            out.push_str("<details><summary>Captured stdout</summary>\n\n```\n");
            out.push_str(stdout);
            out.push_str("\n```\n\n</details>\n\n");
        }
    }

    let ms_notes_owned = notes_since(notes, milestone.started_at);
    let ms_notes: Vec<&NoteRow> = ms_notes_owned.iter().collect();
    render_uncertainty(&mut out, &ms_notes);
    render_postmortem(&mut out, &ms_notes);
    render_decision_log_summary(&mut out, &ms_notes);

    out
}

// ─── Red team ────────────────────────────────────────────────────────────────

fn render_red_team(feature: &FeatureRow, runs: &[AtosRunRow], notes: &[NoteRow]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — red team report\n\n", feature.id));
    let rt_runs: Vec<&AtosRunRow> = runs.iter().filter(|r| r.mode == "redteam").collect();
    if rt_runs.is_empty() {
        out.push_str("_No red-team runs have fired yet for this feature._\n");
        return out;
    }
    out.push_str(&format!(
        "**Red-team runs:** {} (most recent first)\n\n",
        rt_runs.len()
    ));
    for r in rt_runs.iter().rev().take(5) {
        let when = if let Some(e) = r.ended_at {
            format!("{}s", e - r.started_at)
        } else {
            "in-flight".into()
        };
        out.push_str(&format!(
            "- `{}` driver={} duration={}\n",
            short_id(&r.id),
            r.driver,
            when
        ));
    }
    out.push_str("\n");

    let findings: Vec<&NoteRow> = notes
        .iter()
        .filter(|n| n.kind == "redteam_finding")
        .collect();
    if findings.is_empty() {
        out.push_str("## Findings\n\n_No findings recorded._\n");
        return out;
    }

    out.push_str("## Findings\n\n");
    render_redteam_findings_by_confidence(&mut out, &findings, RedteamStyle::Detailed);
    out
}

// ─── Full epistemic report ───────────────────────────────────────────────────

fn render_full(
    feature: &FeatureRow,
    milestones: &[MilestoneRow],
    runs: &[AtosRunRow],
    notes: &[NoteRow],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — {}\n\n", feature.id, feature.title));
    out.push_str(&format!("**State:** {}\n\n", feature.state));

    // 1. What was built.
    out.push_str("## What was built\n\n");
    if milestones.is_empty() {
        out.push_str("_No milestones were provisioned._\n\n");
    } else {
        let mut sorted = milestones.to_vec();
        sorted.sort_by_key(|m| m.ordinal);
        for m in &sorted {
            let (verdict, _) = milestone_verdict(runs, &m.id);
            let mark = if verdict == "PASS" { "✓" } else { "⚠" };
            out.push_str(&format!(
                "- {mark} **Milestone {}.** {}\n",
                m.ordinal,
                derive_title(&m.brief_md)
            ));
        }
        out.push_str("\n");
    }

    // 2. Tests.
    out.push_str("## Tests and stop conditions\n\n");
    let mut sorted = milestones.to_vec();
    sorted.sort_by_key(|m| m.ordinal);
    let mut any_stdout = false;
    for m in &sorted {
        let latest = runs
            .iter()
            .filter(|r| r.milestone_id == m.id && r.mode == "normal")
            .max_by_key(|r| r.started_at);
        let stop = extract_stop_condition(&m.brief_md);
        let stop_display = if stop.is_empty() {
            "(manual review)".to_string()
        } else {
            format!("`{stop}`")
        };
        out.push_str(&format!(
            "### Milestone {} — {}\n\n{}\n\n",
            m.ordinal,
            derive_title(&m.brief_md),
            stop_display
        ));
        if let Some(r) = latest {
            if let Some(stdout) = r.stop_stdout.as_deref().filter(|s| !s.is_empty()) {
                any_stdout = true;
                out.push_str("```\n");
                out.push_str(stdout);
                out.push_str("\n```\n\n");
            }
        }
    }
    if !any_stdout {
        out.push_str(
            "_No captured stdout available. Stop conditions either ran before \
             `end-milestone` persisted stdout, or were manual-review._\n\n",
        );
    }

    render_uncertainty(&mut out, &notes.iter().collect::<Vec<_>>());
    render_postmortem(&mut out, &notes.iter().collect::<Vec<_>>());

    // Red-team section if any findings exist.
    let findings: Vec<&NoteRow> = notes
        .iter()
        .filter(|n| n.kind == "redteam_finding")
        .collect();
    if !findings.is_empty() {
        out.push_str("## Red team findings\n\n");
        render_redteam_findings_by_confidence(&mut out, &findings, RedteamStyle::Compact);
    }

    render_decision_log_summary(&mut out, &notes.iter().collect::<Vec<_>>());

    out
}

// ─── Section renderers ───────────────────────────────────────────────────────

fn render_uncertainty(out: &mut String, notes: &[&NoteRow]) {
    let items: Vec<&&NoteRow> = notes
        .iter()
        .filter(|n| n.kind == "uncertainty")
        .collect();
    if items.is_empty() {
        return;
    }
    out.push_str("## What I'm uncertain about\n\n");
    // Group by the first files entry; unlisted items appear in an
    // "untagged" bucket.
    let mut by_file: BTreeMap<String, Vec<&&NoteRow>> = BTreeMap::new();
    for n in items {
        let key = n
            .files
            .first()
            .cloned()
            .unwrap_or_else(|| "(no file tag)".into());
        by_file.entry(key).or_default().push(n);
    }
    for (file, rows) in by_file {
        out.push_str(&format!("### {}\n\n", file));
        for n in rows {
            let first = n.content.lines().next().unwrap_or("").trim();
            out.push_str(&format!("- ⚠ `[note:{}]` {}\n", n.id, first));
            for line in n.content.lines().skip(1).take(3) {
                let t = line.trim();
                if !t.is_empty() {
                    out.push_str(&format!("  {}\n", t));
                }
            }
        }
        out.push_str("\n");
    }
}

fn render_postmortem(out: &mut String, notes: &[&NoteRow]) {
    let items: Vec<&&NoteRow> = notes
        .iter()
        .filter(|n| n.kind == "postmortem_pointer")
        .collect();
    if items.is_empty() {
        return;
    }
    out.push_str("## Where I would look first if this breaks\n\n");
    for (i, n) in items.iter().enumerate() {
        let first = n.content.lines().next().unwrap_or("").trim();
        out.push_str(&format!("{}. `[note:{}]` {}\n", i + 1, n.id, first));
    }
    out.push_str("\n");
}

/// Output shape for the red-team findings block. Both the stand-alone
/// `red-team.md` report and the embedded "Red team findings" section
/// of the epistemic report need the same confidence-bucket grouping;
/// they differ only in per-finding verbosity.
#[derive(Clone, Copy)]
enum RedteamStyle {
    /// Stand-alone red-team report. Each finding gets invariant +
    /// evidence + the note's `files` list rendered as separate
    /// sub-bullets.
    Detailed,
    /// Embedded in the epistemic report. Condensed: one bullet per
    /// finding with the invariant inline; evidence stays as a
    /// sub-bullet but the files list is omitted to keep the parent
    /// report scannable.
    Compact,
}

/// Shared renderer for `## Findings` (red-team report) and `## Red
/// team findings` (epistemic report). Groups notes by confidence
/// bucket (high → medium → low), dropping empty buckets.
fn render_redteam_findings_by_confidence(
    out: &mut String,
    findings: &[&NoteRow],
    style: RedteamStyle,
) {
    for bucket in ["high", "medium", "low"] {
        let in_bucket: Vec<&&NoteRow> = findings
            .iter()
            .filter(|n| decode_redteam_confidence(&n.content) == bucket)
            .collect();
        if in_bucket.is_empty() {
            continue;
        }
        out.push_str(&format!("### {} confidence\n\n", bucket));
        for n in in_bucket {
            let f = decode_redteam_finding(&n.content);
            match style {
                RedteamStyle::Detailed => {
                    out.push_str(&format!(
                        "- **{}** — `[note:{}]`\n  - Invariant: {}\n",
                        f.status, n.id, f.invariant
                    ));
                    if !f.evidence.is_empty() {
                        out.push_str(&format!("  - Evidence: {}\n", f.evidence));
                    }
                    if !n.files.is_empty() {
                        out.push_str(&format!("  - Files: {}\n", n.files.join(", ")));
                    }
                }
                RedteamStyle::Compact => {
                    out.push_str(&format!(
                        "- **{}** — `[note:{}]`: {}\n",
                        f.status, n.id, f.invariant
                    ));
                    if !f.evidence.is_empty() {
                        out.push_str(&format!("  - Evidence: {}\n", f.evidence));
                    }
                }
            }
        }
        out.push_str("\n");
    }
}

fn render_decision_log_summary(out: &mut String, notes: &[&NoteRow]) {
    let decisions = notes.iter().filter(|n| n.kind == "decision").count();
    let invariants = notes.iter().filter(|n| n.kind == "invariant").count();
    let attempts = notes.iter().filter(|n| n.kind == "attempt").count();
    let uncertainties = notes.iter().filter(|n| n.kind == "uncertainty").count();
    let pointers = notes.iter().filter(|n| n.kind == "postmortem_pointer").count();
    let findings = notes.iter().filter(|n| n.kind == "redteam_finding").count();
    out.push_str("## Decision log summary\n\n");
    out.push_str(&format!(
        "- decisions: {decisions}\n- invariants: {invariants}\n\
         - attempts: {attempts}\n- uncertainties: {uncertainties}\n\
         - postmortem pointers: {pointers}\n- red-team findings: {findings}\n\n"
    ));
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

async fn fetch_feature_notes(notes: &NoteStore, feature_id: &str) -> Result<Vec<NoteRow>> {
    let filter = ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(feature_id.to_string()),
    };
    let rows = notes
        .read_notes_scoped(None, &[], &[], &[], 100, false, &filter)
        .await
        .map_err(|e| Error::Store(e.to_string()))?;
    Ok(rows)
}

fn notes_since(notes: &[NoteRow], _since: Option<i64>) -> Vec<NoteRow> {
    // M3.6 deliberately does not filter by milestone-start time.
    // Early feature milestones share context, and the per-milestone
    // artifact is a consolidated view anyway. M3.7 teardown uses
    // every feature-scoped note regardless. Keep the parameter so the
    // call-site compiles; wire a real filter in M4 once we're
    // certain of the desired UX.
    notes.to_vec()
}

fn milestone_verdict(runs: &[AtosRunRow], milestone_id: &str) -> (String, Option<AtosRunRow>) {
    let normal: Vec<&AtosRunRow> = runs
        .iter()
        .filter(|r| r.milestone_id == milestone_id && r.mode == "normal")
        .collect();
    if normal.is_empty() {
        return ("UNTESTED".to_string(), None);
    }
    let latest = normal.iter().copied().max_by_key(|r| r.started_at).cloned();
    let passed = latest.as_ref().and_then(|r| r.stop_passed).unwrap_or(false);
    let verdict = if passed { "PASS" } else { "FAIL" };
    (verdict.to_string(), latest)
}

fn extract_stop_condition(brief_md: &str) -> String {
    crate::local::extract_milestone_stop_condition(brief_md)
}

fn derive_title(brief_md: &str) -> String {
    let first = brief_md.lines().next().unwrap_or("").trim();
    first
        .trim_start_matches('#')
        .trim()
        .trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | ')' | ':' | ' '))
        .trim()
        .to_string()
}

fn short_id(s: &str) -> &str {
    if s.len() > 8 { &s[..8] } else { s }
}

// ─── Red-team decode ─────────────────────────────────────────────────────────

#[derive(Default)]
struct RedteamFinding {
    invariant: String,
    status: String,
    evidence: String,
}

fn decode_redteam_finding(content: &str) -> RedteamFinding {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
        return RedteamFinding {
            invariant: content.lines().next().unwrap_or("").trim().to_string(),
            status: "unknown".into(),
            evidence: String::new(),
        };
    };
    RedteamFinding {
        invariant: v.get("invariant").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        status: v.get("status").and_then(|x| x.as_str()).unwrap_or("unknown").to_string(),
        evidence: v.get("evidence").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    }
}

fn decode_redteam_confidence(content: &str) -> String {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| v.get("confidence").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_else(|| "low".into())
}
