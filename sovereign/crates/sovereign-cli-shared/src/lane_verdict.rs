// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trailing-Judgement-line protocol — how a lane subprocess tells its
//! runner what it decided.
//!
//! `svrn quality check` runs each lane as a SUBPROCESS and has to learn one
//! thing from it: which of the four verdicts (ARCH §18.2) the lane earned,
//! and why. An exit code carries two of the four at best, which is exactly
//! the collapse `scripts/lib/ci-bench-verdict.sh` was extracted to fight —
//! that file reconstructs a four-verdict decision by `grep`ing a lane's prose
//! for `"N regressed"`, `"unmeasured — every question errored"` and the
//! daemon's own unreachability strings. It works, and every one of those
//! greps is a coupling to wording nobody promised to keep.
//!
//! So the lane SAYS it. The last non-empty line of a lane's stdout is a
//! [`Judgement`] in JSON:
//!
//! ```json
//! {"subject":"chat-ask","verdict":"failed","reason":"both halves answered: the gate located 0 quotes for `grounding gate`","as_of":1788560000}
//! ```
//!
//! - `subject`, `verdict`, `reason` are required. `verdict` is the kebab-case
//!   wire spelling [`Verdict::parse_wire`] already owns.
//! - `as_of` is optional and is unix epoch SECONDS — not `SystemTime`'s serde
//!   form (`{"secs_since_epoch":…}`), which no lane author would write by
//!   hand and no shell could emit.
//!
//! **A lane that emits no such line is `never-ran`, never a pass.** The
//! runner reports the exit code as the reason. That is the same rule as the
//! zero-test exit 4 in `scripts/sovereign-test.sh`: an instrument that said
//! nothing verified nothing.
//!
//! ONE decider (ARCH §10.6): [`emit`] writes the line, [`from_stdout`] reads
//! it, and they live here — in the crate both `sovereign-cli` (the runner)
//! and `sovereign-cli-llm` (the lanes) already depend on — rather than as a
//! `json!` literal on one side and a parser on the other.

use kernel_types::{Judgement, Reason, Verdict};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Render a judgement as the trailing line a lane prints last.
///
/// Includes the trailing newline: the caller prints this and nothing after.
#[must_use]
pub fn emit(j: &Judgement) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("subject".into(), j.subject().into());
    obj.insert("verdict".into(), j.verdict().as_str().into());
    obj.insert("reason".into(), j.reason().as_str().into());
    if let Some(secs) = j
        .dated_at()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
    {
        obj.insert("as_of".into(), secs.into());
    }
    format!("{}\n", serde_json::Value::Object(obj))
}

/// Why a candidate line was not a verdict. Reported, never defaulted
/// (ARCH §18.3) — the runner puts this text in the `never-ran` reason so a
/// lane that ALMOST spoke the protocol is distinguishable from one that
/// never tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineError {
    /// Nothing on stdout but whitespace.
    NoOutput,
    /// The last line is not JSON, or not a JSON object.
    NotJson,
    /// A required key is missing or not a string.
    MissingField(&'static str),
    /// `verdict` was present but is not one of the four wire spellings.
    UnknownVerdict(String),
    /// `reason` was present but is a placeholder the type refuses.
    PlaceholderReason(String),
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineError::NoOutput => f.write_str("the lane printed nothing on stdout"),
            LineError::NotJson => {
                f.write_str("the lane's last stdout line is not a JSON object")
            }
            LineError::MissingField(k) => {
                write!(f, "the lane's verdict line has no `{k}` string")
            }
            LineError::UnknownVerdict(v) => write!(
                f,
                "the lane reported verdict `{v}`, which is not one of \
                 passed/failed/could-not-judge/never-ran"
            ),
            LineError::PlaceholderReason(r) => {
                write!(f, "the lane's verdict reason `{r}` carries no information")
            }
        }
    }
}

/// Parse one line as the wire form. Strict — see [`from_stdout`] for the
/// rule about WHICH line this is applied to.
pub fn parse_line(line: &str) -> Result<Judgement, LineError> {
    let value: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|_| LineError::NotJson)?;
    let obj = value.as_object().ok_or(LineError::NotJson)?;
    let str_field = |k: &'static str| -> Result<String, LineError> {
        obj.get(k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or(LineError::MissingField(k))
    };
    let subject = str_field("subject")?;
    let verdict_raw = str_field("verdict")?;
    let reason_raw = str_field("reason")?;
    let verdict =
        Verdict::parse_wire(&verdict_raw).ok_or(LineError::UnknownVerdict(verdict_raw))?;
    let reason =
        Reason::new(reason_raw.clone()).ok_or(LineError::PlaceholderReason(reason_raw))?;
    let j = match verdict {
        Verdict::Passed => Judgement::passed(subject, reason),
        Verdict::Failed => Judgement::failed(subject, reason),
        Verdict::CouldNotJudge => Judgement::could_not_judge(subject, reason),
        Verdict::NeverRan => Judgement::never_ran(subject, reason),
    };
    Ok(match obj.get("as_of").and_then(serde_json::Value::as_u64) {
        Some(secs) => j.as_of(UNIX_EPOCH + Duration::from_secs(secs)),
        None => j,
    })
}

/// Read a lane's verdict from its captured stdout.
///
/// THE LAST NON-EMPTY LINE, and only that one. Deliberately not "the last
/// line that happens to parse": a lane that prints a JSON report and then
/// crashes before its verdict would otherwise have its report adopted as a
/// verdict, which is a green with nothing behind it. Trailing blank lines are
/// skipped because a `println!` at the end of a run is not a statement about
/// anything.
pub fn from_stdout(stdout: &str) -> Result<Judgement, LineError> {
    let last = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or(LineError::NoOutput)?;
    parse_line(last)
}

/// Convenience for a lane: print `j` as the trailing line, and return the
/// process exit code the runner expects (0 — the RUNNER decides what a
/// verdict means for the build, not the lane).
pub fn print(j: &Judgement) {
    print!("{}", emit(j));
}

/// The `as_of` a lane stamps on a verdict it just produced.
#[must_use]
pub fn now() -> SystemTime {
    SystemTime::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_survives_the_round_trip_including_its_date() {
        let when = UNIX_EPOCH + Duration::from_secs(1_788_560_000);
        let j = Judgement::failed(
            "chat-ask",
            Reason::literal("both halves answered: located 0 quotes"),
        )
        .as_of(when);
        let back = parse_line(&emit(&j)).expect("round trip");
        assert_eq!(back.subject(), "chat-ask");
        assert_eq!(back.verdict(), Verdict::Failed);
        assert_eq!(back.reason().as_str(), "both halves answered: located 0 quotes");
        assert_eq!(back.dated_at(), Some(when));
    }

    /// All four verdicts cross the wire, not two. A lane that could not judge
    /// must be able to SAY could-not-judge.
    #[test]
    fn all_four_verdicts_cross_the_wire() {
        for v in [
            Verdict::Passed,
            Verdict::Failed,
            Verdict::CouldNotJudge,
            Verdict::NeverRan,
        ] {
            let j = match v {
                Verdict::Passed => Judgement::passed("l", Reason::literal("ran")),
                Verdict::Failed => Judgement::failed("l", Reason::literal("ran")),
                Verdict::CouldNotJudge => {
                    Judgement::could_not_judge("l", Reason::literal("ran"))
                }
                Verdict::NeverRan => Judgement::never_ran("l", Reason::literal("ran")),
            };
            assert_eq!(parse_line(&emit(&j)).unwrap().verdict(), v);
        }
    }

    /// The line is the LAST one. A lane's report earlier in the stream is not
    /// a verdict, even when it is well-formed JSON — adopting it is how a
    /// crashed lane comes to read as green.
    #[test]
    fn an_earlier_json_line_is_never_adopted_as_the_verdict() {
        let stdout = "{\"subject\":\"chat-ask\",\"verdict\":\"passed\",\"reason\":\"all rows green\"}\n\
                      thread 'main' panicked at lane.rs:12\n";
        assert_eq!(from_stdout(stdout), Err(LineError::NotJson));
    }

    #[test]
    fn trailing_blank_lines_do_not_hide_the_verdict() {
        let stdout = "noise\n{\"subject\":\"t\",\"verdict\":\"passed\",\"reason\":\"ok\"}\n\n  \n";
        assert_eq!(from_stdout(stdout).unwrap().verdict(), Verdict::Passed);
    }

    /// An unknown verdict word is an absence, not a pass (ARCH §18.3). The
    /// same rule `posture_cmd`'s `oicp_conformance_row` states for its lane.
    #[test]
    fn an_unknown_verdict_word_is_refused_rather_than_mapped() {
        let line = "{\"subject\":\"t\",\"verdict\":\"green\",\"reason\":\"ok\"}";
        assert_eq!(
            parse_line(line),
            Err(LineError::UnknownVerdict("green".into()))
        );
    }

    /// A placeholder reason is refused HERE rather than reaching a table as a
    /// row nobody can act on. `Reason::new` already owns the list.
    #[test]
    fn a_placeholder_reason_is_refused() {
        let line = "{\"subject\":\"t\",\"verdict\":\"failed\",\"reason\":\"unknown\"}";
        assert_eq!(
            parse_line(line),
            Err(LineError::PlaceholderReason("unknown".into()))
        );
    }

    #[test]
    fn silence_is_reported_as_silence() {
        assert_eq!(from_stdout("   \n\n"), Err(LineError::NoOutput));
        assert_eq!(from_stdout(""), Err(LineError::NoOutput));
    }
}
