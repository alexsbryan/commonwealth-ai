// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn session audit` — check an agent's report to the operator against
//! the session that produced it.
//!
//! # The artifact nothing gated
//!
//! Six hooks fire in this repo and every one judges an INPUT: the tool call
//! (`prefer-code-intel`), the commit (`commit-smells`), the plan
//! (`validate-plan`). All of it gates what lands in git. The message the
//! operator actually reads passed through nothing.
//!
//! # Why the transcript is the right evidence universe
//!
//! `runtime::grounding` asks whether a generated answer is supported by a
//! retrieved corpus, where "supported" is genuinely fuzzy and calibrating
//! the judge needs a hand-labelled bank. Here the corpus is the session's
//! own tool calls, and a large share of claims are MECHANICALLY decidable:
//! the file exists or it does not, the test ran or it did not. That yields
//! gold labels for free, in domain — which is what the model pass will be
//! calibrated against when it lands (`quality/report-audit/`).
//!
//! # Four verdicts, not two
//!
//! Every finding carries a [`Verdict`], and `NeverRan` is the one this was
//! built for: the report says the tests pass and no test ran. That is not
//! "failed" — nothing failed, because nothing was measured — and reporting
//! it as a pass is the substitution ARCH §18.3 forbids.

pub mod checks;
pub mod evidence;

use checks::Finding;
use kernel_types::{Judgement, Reason, Verdict};

/// The subject every judgement from this pass carries.
const SUBJECT: &str = "report-audit";
use std::path::PathBuf;

use crate::util::help::{Help, HelpSection};

const HELP: Help = Help {
    command: "svrn session audit",
    summary: "Check a report against the session that produced it.",
    sections: &[
        HelpSection::Usage("svrn session audit [<session-id-prefix>] [options]"),
        HelpSection::Notes(
            "Reads a harness transcript, builds the evidence pool from its tool \
             calls, and rules on the claims in the final assistant message. Four \
             checks, no model: path citations, gate claims (\"the tests pass\"), \
             symbol citations, and figures with no source in the session.",
        ),
        HelpSection::Flags(&[
            ("--transcript <path>", "Audit this transcript file directly."),
            (
                "--message-file <path>",
                "Audit this text instead of the transcript's last assistant message.",
            ),
            ("--root <path>", "Repo root for resolving citations (default: cwd)."),
            ("--format <human|json>", "Output shape (default: human)."),
            ("--all", "Print the summary even when there are no findings."),
        ]),
        HelpSection::Notes(
            "Exit: 0 nothing withheld, 1 a blocking finding, 2 could not read. \
             Only gate claims block today; the rest report until \
             quality/report-audit/ has measured their false-positive rate.",
        ),
    ],
};

struct Args {
    id: Option<String>,
    transcript: Option<PathBuf>,
    message_file: Option<PathBuf>,
    root: PathBuf,
    json: bool,
    all: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        id: None,
        transcript: None,
        message_file: None,
        root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        json: false,
        all: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let take = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a {
            "--transcript" => out.transcript = Some(PathBuf::from(take(&mut i)?)),
            "--message-file" => out.message_file = Some(PathBuf::from(take(&mut i)?)),
            "--root" => out.root = PathBuf::from(take(&mut i)?),
            "--format" => out.json = take(&mut i)? == "json",
            "--all" => out.all = true,
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other => out.id = Some(other.to_string()),
        }
        i += 1;
    }
    Ok(out)
}

/// The report to rule on: an explicit message file, else the transcript's
/// last assistant text.
fn report_text(args: &Args, ev: &evidence::Evidence) -> Result<String, String> {
    if let Some(p) = &args.message_file {
        return std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()));
    }
    ev.assistant_texts
        .last()
        .cloned()
        .ok_or_else(|| "this transcript has no assistant message to audit".to_string())
}

/// Roll findings up into the one judgement a runner reads.
fn judge(findings: &[Finding], checked: usize) -> Judgement {
    let blocking = findings.iter().filter(|f| f.blocks()).count();
    let reason = |t: String| {
        Reason::new(t).unwrap_or_else(|| Reason::literal("the audit recorded no text here"))
    };
    if checked == 0 {
        return Judgement::never_ran(
            SUBJECT,
            Reason::literal("the report made no claim any check knows how to decide"),
        );
    }
    if blocking > 0 {
        return Judgement::failed(
            SUBJECT,
            reason(format!(
                "{blocking} of {checked} decidable claims are unsupported by this session"
            )),
        );
    }
    if findings.is_empty() {
        return Judgement::passed(
            SUBJECT,
            reason(format!(
                "{checked} decidable claims, all grounded in this session"
            )),
        );
    }
    Judgement::could_not_judge(
        SUBJECT,
        reason(format!(
            "{} of {checked} decidable claims could not be settled from this session",
            findings.len()
        )),
    )
}

/// How much of a claim to echo back. Enough to recognise the sentence,
/// not so much that one finding fills the terminal.
const CLAIM_ECHO_CHARS: usize = 160;

fn elide(s: &str, cap: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= cap {
        return one_line;
    }
    one_line.chars().take(cap).collect::<String>() + "…"
}

fn render_human(findings: &[Finding], j: &Judgement, checked: usize) -> String {
    let mut out = String::new();
    if findings.is_empty() {
        out.push_str(&format!(
            "report-audit: {checked} decidable claims, nothing unsupported.\n"
        ));
        return out;
    }
    out.push_str("report-audit findings\n\n");
    let mut sorted: Vec<&Finding> = findings.iter().collect();
    sorted.sort_by_key(|f| (!f.blocks(), f.check, f.subject.clone()));
    for f in sorted {
        let mark = if f.blocks() { "BLOCKS" } else { "advisory" };
        let claim = elide(&f.claim, CLAIM_ECHO_CHARS);
        out.push_str(&format!(
            "  [{}] {} · {}\n    claim:  {}\n    ruling: {}\n    why:    {}\n\n",
            mark,
            f.check.as_str(),
            f.subject,
            claim,
            f.verdict.as_str(),
            f.reason,
        ));
    }
    out.push_str(&format!("{}: {}\n", j.verdict().as_str(), j.reason().as_str()));
    out
}

fn render_json(findings: &[Finding], j: &Judgement, checked: usize) -> String {
    let rows: Vec<serde_json::Value> = findings
        .iter()
        .map(|f| {
            serde_json::json!({
                "check": f.check.as_str(),
                "verdict": f.verdict.as_str(),
                "blocks": f.blocks(),
                "subject": f.subject,
                "claim": f.claim,
                "reason": f.reason,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "subject": j.subject(),
        "verdict": j.verdict().as_str(),
        "reason": j.reason().as_str(),
        "decidable_claims": checked,
        "findings": rows,
    }))
    .unwrap_or_default()
}

pub fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let args = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let transcript = match resolve_transcript(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let ev = match evidence::extract(&transcript) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let report = match report_text(&args, &ev) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let ruled = checks::run(&report, &ev, &args.root);
    // A passing ruling is kept through the pass so `--all` can show the
    // decision (ARCH §1: a decision nothing can see did not happen), and
    // dropped here so it never counts as a finding.
    let findings: Vec<Finding> = ruled
        .iter()
        .filter(|f| f.verdict != Verdict::Passed)
        .cloned()
        .collect();
    let checked = checks::decidable_count(&report, &ev, &args.root);
    let j = judge(&findings, checked);

    if args.json {
        println!("{}", render_json(&findings, &j, checked));
    } else {
        let shown: Vec<Finding> = if args.all { ruled.clone() } else { findings.clone() };
        if !shown.is_empty() || args.all {
            print!("{}", render_human(&shown, &j, checked));
        }
        // The trailing judgement line the quality runner reads. Last, and
        // nothing after it.
        print!("{}", sovereign_cli_shared::lane_verdict::emit(&j));
    }

    i32::from(findings.iter().any(Finding::blocks))
}

fn resolve_transcript(args: &Args) -> Result<PathBuf, String> {
    if let Some(p) = &args.transcript {
        return if p.exists() {
            Ok(p.clone())
        } else {
            Err(format!("no transcript at {}", p.display()))
        };
    }
    let id = args
        .id
        .as_deref()
        .ok_or("name a session id prefix, or pass --transcript <path>")?;
    let dir = crate::cache_audit_cmd::resolve_transcript_dir(args.root.to_str(), None)?;
    crate::session_cmd::find_transcript(&dir, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use checks::Check;

    fn f(check: Check, verdict: Verdict) -> Finding {
        Finding {
            check,
            verdict,
            subject: "s".into(),
            claim: "c".into(),
            reason: "r".into(),
        }
    }

    #[test]
    fn a_report_with_no_decidable_claim_is_never_ran_not_passed() {
        let j = judge(&[], 0);
        assert_eq!(j.verdict(), Verdict::NeverRan);
    }

    #[test]
    fn clean_decidable_claims_pass() {
        let j = judge(&[], 5);
        assert_eq!(j.verdict(), Verdict::Passed);
    }

    #[test]
    fn a_blocking_finding_fails_the_report() {
        let j = judge(&[f(Check::GateClaim, Verdict::NeverRan)], 3);
        assert_eq!(j.verdict(), Verdict::Failed);
    }

    #[test]
    fn advisory_findings_alone_are_could_not_judge_never_failed() {
        let j = judge(&[f(Check::PathCitation, Verdict::CouldNotJudge)], 3);
        assert_eq!(j.verdict(), Verdict::CouldNotJudge);
    }
}
