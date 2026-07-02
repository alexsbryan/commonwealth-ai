// SPDX-License-Identifier: AGPL-3.0-or-later
//! Render a `HarnessRun` as the per-stage verdict ladder — failures shown, not
//! summarized. Each verdict prints the declared bar (`expected`) alongside what
//! actually happened (`observed`), with the concrete failing items beneath.

use super::{EvidenceItem, HarnessRun, Locus, StageResult, Status};

const RULE: &str = "──────────────────────────────────────────────────────────────────────────";

/// Render the run as plain text for the CLI report.
pub fn render_report(run: &HarnessRun) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Harness   sample {}   recipe {}\n",
        short(&run.sample_id),
        short(&run.recipe_hash)
    ));
    out.push_str(RULE);
    out.push('\n');

    for stage in &run.stages {
        out.push_str(&format!(
            "{:<14} {}\n",
            stage.stage.to_uppercase(),
            badge(stage_status(stage))
        ));
        for v in &stage.verdicts {
            out.push_str(&format!(
                "    {} {}  →  {}\n",
                mark(v.status),
                v.expected,
                v.observed
            ));
            for e in &v.evidence {
                out.push_str(&format!("        · {} — {}\n", locus(&e.locus), excerpt(e)));
            }
        }
    }

    out.push_str(RULE);
    out.push('\n');
    out.push_str(&format!(
        "VERDICT  {}   did the recipe do what it declares?\n",
        if run.green() { "GREEN" } else { "RED" }
    ));
    out
}

/// The worst verdict in a stage drives its badge (Fail > Warn > Pass).
fn stage_status(stage: &StageResult) -> Status {
    if stage.verdicts.iter().any(|v| v.status == Status::Fail) {
        Status::Fail
    } else if stage.verdicts.iter().any(|v| v.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Pass
    }
}

fn badge(s: Status) -> &'static str {
    match s {
        Status::Pass => "PASS",
        Status::Fail => "FAIL",
        Status::Warn => "WARN",
    }
}

fn mark(s: Status) -> char {
    match s {
        Status::Pass => '✓',
        Status::Fail => '✗',
        Status::Warn => '⚠',
    }
}

fn locus(l: &Locus) -> String {
    match l {
        Locus::Doc(id) => format!("doc {id}"),
        Locus::Chunk(id) => format!("chunk {id}"),
        Locus::Atom(id) => format!("atom {id}"),
    }
}

/// One-line, length-bounded excerpt for the console.
fn excerpt(e: &EvidenceItem) -> String {
    let flat = e.excerpt.replace(['\n', '\r'], " ");
    let trimmed: String = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    trimmed.chars().take(110).collect()
}

fn short(hash: &str) -> &str {
    if hash.len() >= 8 {
        &hash[..8]
    } else {
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckId, Verdict};

    fn v(status: Status, expected: &str, observed: &str, ev: Vec<EvidenceItem>) -> Verdict {
        Verdict {
            check: CheckId::ExtractCoverage,
            status,
            expected: expected.into(),
            observed: observed.into(),
            evidence: ev,
        }
    }

    #[test]
    fn renders_ladder_with_shown_evidence_and_red_verdict() {
        let run = HarnessRun {
            sample_id: "7f3acafe1234".into(),
            recipe_hash: "91be0000abcd".into(),
            stages: vec![StageResult {
                stage: "extract".into(),
                config_hash: "x".into(),
                cache_hit: false,
                verdicts: vec![
                    v(
                        Status::Pass,
                        "section: holding: present in ≥100% of files",
                        "found in 3/3 files",
                        vec![],
                    ),
                    v(
                        Status::Fail,
                        "section: dissent: present in ≥100% of files",
                        "found in 2/3 files",
                        vec![EvidenceItem {
                            locus: Locus::Doc("us-code-§1342".into()),
                            excerpt: "There is no separate dissent;\n  see the per curiam".into(),
                        }],
                    ),
                ],
            }],
        };
        let report = render_report(&run);
        assert!(report.contains("EXTRACT"));
        assert!(report.contains("FAIL"));
        assert!(report.contains("found in 2/3 files"));
        assert!(report.contains("doc us-code-§1342"));
        assert!(report.contains("VERDICT  RED"));
        assert!(!run.green());
    }
}
