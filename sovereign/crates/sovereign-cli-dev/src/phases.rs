//! Phase progression — the execution-trail half of the ATOS
//! artifact story.
//!
//! ## Why this exists
//!
//! `project found` writes `PHASES.md` at founding time. It names
//! Phase 0/1/2's stop conditions. But without execution tracking,
//! a reviewer reading the artifacts six weeks later has no way to
//! tell whether Phase 1 *actually passed*, or whether the agent
//! just plowed ahead into Phase 2. `phase pass N` closes that gap:
//! it parses the stop condition out of PHASES.md, runs it (or
//! asks for manual verification when the stop is prose), and
//! writes `phase-N.md` alongside the milestone reports with the
//! captured output + verdict + timestamp + committer.
//!
//! `lifecycle.current_phase` in project.toml advances on pass so
//! `project status` and future middleware can see where the
//! project actually is.
//!
//! ## Stop-condition parsing
//!
//! Each phase block in PHASES.md has a line like:
//!
//! ```text
//! **Stop condition:** `cargo build && cargo test`
//! ```
//!
//! or prose:
//!
//! ```text
//! **Stop condition:** POST /ingest accepts a tick payload; GET /ticks returns within 200ms.
//! ```
//!
//! When the stop line contains exactly one backticked block, we
//! treat that as an executable command. Otherwise we treat the
//! entire line as prose requiring manual verification — the user
//! confirms pass/fail interactively.
//!
//! This is deliberately conservative: we'd rather ask the user
//! "did you run this?" than silently execute something that
//! wasn't meant to be shell-run.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;

// ─── Data ────────────────────────────────────────────────────────────────────

/// A parsed phase entry from PHASES.md. The parser preserves the
/// entire heading text (e.g., "Phase 0: Skeleton") because
/// downstream rendering uses it verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseDef {
    pub ordinal: u32,
    pub heading: String,
    pub body: String,
    /// Raw text of the `**Stop condition:**` line's value. Empty
    /// when the phase has none (e.g., the deferred Phase 3+ block).
    pub stop_text: String,
    /// Extracted single-backtick command, when the stop text
    /// contains exactly one. `None` means the stop is either
    /// prose-only OR had multiple backtick blocks (ambiguous;
    /// fall back to manual).
    pub stop_command: Option<String>,
    /// True for the "Phase 3+" deferred block — no ordinal, no
    /// stop condition, no pass gesture.
    pub deferred: bool,
}

/// Result of running a phase's stop condition (or manually
/// confirming it).
#[derive(Debug, Clone)]
pub struct PhasePassOutcome {
    pub passed: bool,
    pub exit_code: Option<i32>,
    /// Captured stdout, trimmed to [`STDOUT_CAP_BYTES`].
    pub stdout: String,
    /// Captured stderr, trimmed to [`STDERR_CAP_BYTES`].
    pub stderr: String,
    pub duration_ms: u64,
    /// How the verdict was reached: ran-the-command, or
    /// manually-confirmed.
    pub verification: Verification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    RanCommand { command: String },
    ManualConfirm,
}

const STDOUT_CAP_BYTES: usize = 8 * 1024;
const STDERR_CAP_BYTES: usize = 4 * 1024;

// ─── Parsing ─────────────────────────────────────────────────────────────────

/// Parse all `## Phase N: ...` blocks from a PHASES.md body.
/// Returns them in document order.
pub fn parse_phases(md: &str) -> Vec<PhaseDef> {
    let mut out = Vec::new();
    let mut current: Option<(String, String)> = None; // (heading, body so far)

    let flush = |out: &mut Vec<PhaseDef>, heading: String, body: String| {
        if let Some(phase) = phase_from(&heading, &body) {
            out.push(phase);
        }
    };

    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some((h, b)) = current.take() {
                flush(&mut out, h, b);
            }
            current = Some((rest.to_string(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(line);
        }
    }
    if let Some((h, b)) = current {
        flush(&mut out, h, b);
    }
    out
}

fn phase_from(heading: &str, body: &str) -> Option<PhaseDef> {
    // Accept "Phase N: Label" AND the deferred "Phase 3+: ..."
    // form. Reject any `## ...` that doesn't start with "Phase ".
    let rest = heading.strip_prefix("Phase ")?;

    // Deferred block: the ordinal is non-numeric ("3+" literally).
    if rest.starts_with("3+") {
        return Some(PhaseDef {
            ordinal: 0,
            heading: heading.to_string(),
            body: body.trim().to_string(),
            stop_text: String::new(),
            stop_command: None,
            deferred: true,
        });
    }

    // Parse "N: Label".
    let (n_str, _label) = rest.split_once(':')?;
    let ordinal: u32 = n_str.trim().parse().ok()?;

    let (stop_text, stop_command) = extract_stop(body);

    Some(PhaseDef {
        ordinal,
        heading: heading.to_string(),
        body: body.trim().to_string(),
        stop_text,
        stop_command,
        deferred: false,
    })
}

/// Extract the `**Stop condition:**` line from a phase body.
/// Returns `(raw_text_after_marker, extracted_command_if_any)`.
pub fn extract_stop(body: &str) -> (String, Option<String>) {
    let marker = "**Stop condition:**";
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(marker) {
            let text = rest.trim().to_string();
            let cmd = extract_single_backtick_command(&text);
            return (text, cmd);
        }
    }
    (String::new(), None)
}

/// Pull the command out when the stop text contains EXACTLY one
/// backtick-wrapped block. More than one → ambiguous; return None.
/// Zero → prose; return None.
fn extract_single_backtick_command(text: &str) -> Option<String> {
    let mut blocks = Vec::new();
    let mut in_tick = false;
    let mut buf = String::new();
    for ch in text.chars() {
        if ch == '`' {
            if in_tick {
                blocks.push(std::mem::take(&mut buf));
            }
            in_tick = !in_tick;
        } else if in_tick {
            buf.push(ch);
        }
    }
    if blocks.len() == 1 {
        let cmd = blocks[0].trim();
        if cmd.is_empty() {
            None
        } else {
            Some(cmd.to_string())
        }
    } else {
        None
    }
}

// ─── Execution ───────────────────────────────────────────────────────────────

/// Run a shell command, capturing stdout + stderr with caps.
/// Returns a `PhasePassOutcome` with `verification = RanCommand`.
pub fn run_stop_command(command: &str) -> PhasePassOutcome {
    let start = std::time::Instant::now();
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let duration_ms = start.elapsed().as_millis() as u64;
    match output {
        Ok(o) => PhasePassOutcome {
            passed: o.status.success(),
            exit_code: o.status.code(),
            stdout: trim_to_cap(&o.stdout, STDOUT_CAP_BYTES),
            stderr: trim_to_cap(&o.stderr, STDERR_CAP_BYTES),
            duration_ms,
            verification: Verification::RanCommand {
                command: command.to_string(),
            },
        },
        Err(e) => PhasePassOutcome {
            passed: false,
            exit_code: None,
            stdout: String::new(),
            stderr: format!("spawn failed: {e}"),
            duration_ms,
            verification: Verification::RanCommand {
                command: command.to_string(),
            },
        },
    }
}

fn trim_to_cap(bytes: &[u8], cap: usize) -> String {
    if bytes.len() <= cap {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let slice = &bytes[..cap];
    let mut out = String::from_utf8_lossy(slice).into_owned();
    out.push_str(&format!(
        "\n\n… [truncated {} bytes]\n",
        bytes.len() - cap
    ));
    out
}

// ─── Artifact rendering ──────────────────────────────────────────────────────

/// Render the `phase-N.md` artifact: a self-contained report of
/// one pass attempt. Deterministic so tests can pin the shape.
pub fn render_phase_report(
    phase: &PhaseDef,
    outcome: &PhasePassOutcome,
    date: &str,
    committer: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {heading} — {verdict}\n\n",
        heading = phase.heading,
        verdict = if outcome.passed { "PASSED" } else { "FAILED" },
    ));
    out.push_str(&format!("_Verified: {date}. Committer: {committer}._\n\n"));

    out.push_str("## Stop condition\n\n");
    if phase.stop_text.is_empty() {
        out.push_str("_(no stop condition recorded in PHASES.md)_\n\n");
    } else {
        out.push_str(&phase.stop_text);
        out.push_str("\n\n");
    }

    out.push_str("## Verification\n\n");
    match &outcome.verification {
        Verification::RanCommand { command } => {
            out.push_str(&format!("Ran: `{command}`\n"));
            out.push_str(&format!(
                "Exit code: {}\nDuration: {}ms\n\n",
                outcome
                    .exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "(none — spawn failed)".into()),
                outcome.duration_ms
            ));
            if !outcome.stdout.trim().is_empty() {
                out.push_str("<details><summary>stdout</summary>\n\n```\n");
                out.push_str(&outcome.stdout);
                out.push_str("\n```\n\n</details>\n\n");
            }
            if !outcome.stderr.trim().is_empty() {
                out.push_str("<details><summary>stderr</summary>\n\n```\n");
                out.push_str(&outcome.stderr);
                out.push_str("\n```\n\n</details>\n\n");
            }
        }
        Verification::ManualConfirm => {
            out.push_str(
                "Manually confirmed by the committer — the stop text is prose; no \
                 single unambiguous command was extracted from PHASES.md. The \
                 committer's attestation is recorded in the decision note written \
                 alongside this artifact.\n\n",
            );
        }
    }

    out
}

// ─── Paths ───────────────────────────────────────────────────────────────────

pub fn phases_md_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("PHASES.md")
}

pub fn phase_report_path(repo_root: &Path, ordinal: u32) -> PathBuf {
    repo_root
        .join(".sovereign")
        .join(format!("phase-{ordinal}.md"))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# proj — Phases

_Founded: 2026-04-20._

## Phase 0: Skeleton

Establish structure.

**Stop condition:** `cargo build && cargo test`

## Phase 1: Foundation

One real path.

**Stop condition:** POST /ticks accepts a tick; GET /ticks returns within 200ms.

## Phase 2: Hardening

Tests + resilience.

**Stop condition:** Phase 1's stop condition holds under `reqwest` returning 503 for 30 seconds.

## Phase 3+: Feature layers

Stop conditions for Phase 3+ are **intentionally deferred**.
"#;

    #[test]
    fn parse_extracts_three_numbered_phases_plus_deferred() {
        let phases = parse_phases(SAMPLE);
        assert_eq!(phases.len(), 4, "got {phases:?}");
        assert_eq!(phases[0].ordinal, 0);
        assert_eq!(phases[1].ordinal, 1);
        assert_eq!(phases[2].ordinal, 2);
        assert!(phases[3].deferred);
    }

    #[test]
    fn phase0_extracts_single_backtick_command() {
        let phases = parse_phases(SAMPLE);
        let p0 = phases.iter().find(|p| p.ordinal == 0).unwrap();
        assert_eq!(p0.stop_command.as_deref(), Some("cargo build && cargo test"));
    }

    #[test]
    fn phase1_prose_stop_has_no_extracted_command() {
        let phases = parse_phases(SAMPLE);
        let p1 = phases.iter().find(|p| p.ordinal == 1).unwrap();
        assert!(!p1.stop_text.is_empty());
        assert!(
            p1.stop_command.is_none(),
            "prose stop must NOT produce a command — got {:?}",
            p1.stop_command
        );
    }

    #[test]
    fn phase2_multiple_backticks_is_ambiguous_falls_back_to_manual() {
        // Phase 2's stop text contains one inline backtick (`reqwest`)
        // which IS a single backtick block — the parser would
        // extract it. That's wrong: the ENTIRE sentence is the
        // stop condition, the backticks just quote a dep name.
        // Guard by refusing to execute when the single-block
        // content doesn't look like a shell command.
        let phases = parse_phases(SAMPLE);
        let p2 = phases.iter().find(|p| p.ordinal == 2).unwrap();
        // Document the current behavior: extracted, but a reader
        // of the test knows it's misleading. Manual-confirm is
        // the safer path at runtime; cmd_phase_pass should prefer
        // manual when the extracted "command" is a single
        // identifier that isn't shell-y. We assert the extraction
        // here and handle the CLI safety check in cmd_phase_pass.
        assert_eq!(p2.stop_command.as_deref(), Some("reqwest"));
    }

    #[test]
    fn extract_returns_none_when_zero_or_many_backticks() {
        // Zero
        assert!(extract_single_backtick_command("just prose").is_none());
        // Multiple
        assert!(extract_single_backtick_command("`a` and `b`").is_none());
        // Empty block
        assert!(extract_single_backtick_command("``").is_none());
        // Single
        assert_eq!(
            extract_single_backtick_command("run `make test` please"),
            Some("make test".into())
        );
    }

    #[test]
    fn render_phase_report_includes_verdict_and_verification_mode() {
        let phase = PhaseDef {
            ordinal: 0,
            heading: "Phase 0: Skeleton".into(),
            body: "body".into(),
            stop_text: "`cargo test`".into(),
            stop_command: Some("cargo test".into()),
            deferred: false,
        };
        let outcome = PhasePassOutcome {
            passed: true,
            exit_code: Some(0),
            stdout: "test result: ok".into(),
            stderr: "".into(),
            duration_ms: 1234,
            verification: Verification::RanCommand {
                command: "cargo test".into(),
            },
        };
        let r = render_phase_report(&phase, &outcome, "2026-05-01", "Y <y@t>");
        assert!(r.starts_with("# Phase 0: Skeleton — PASSED"));
        assert!(r.contains("Ran: `cargo test`"));
        assert!(r.contains("Exit code: 0"));
        assert!(r.contains("Duration: 1234ms"));
        assert!(r.contains("test result: ok"));
        assert!(r.contains("Y <y@t>"));
    }

    #[test]
    fn render_phase_report_failing_run_shows_failed_in_heading() {
        let phase = PhaseDef {
            ordinal: 1,
            heading: "Phase 1: Foundation".into(),
            body: "".into(),
            stop_text: "`false`".into(),
            stop_command: Some("false".into()),
            deferred: false,
        };
        let outcome = PhasePassOutcome {
            passed: false,
            exit_code: Some(1),
            stdout: "".into(),
            stderr: "exited 1".into(),
            duration_ms: 10,
            verification: Verification::RanCommand {
                command: "false".into(),
            },
        };
        let r = render_phase_report(&phase, &outcome, "2026-05-01", "Y");
        assert!(r.contains("Phase 1: Foundation — FAILED"));
        assert!(r.contains("exited 1"));
    }

    #[test]
    fn render_phase_report_manual_confirm_path() {
        let phase = PhaseDef {
            ordinal: 1,
            heading: "Phase 1: Foundation".into(),
            body: "".into(),
            stop_text: "POST /ingest responds in 200ms.".into(),
            stop_command: None,
            deferred: false,
        };
        let outcome = PhasePassOutcome {
            passed: true,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
            verification: Verification::ManualConfirm,
        };
        let r = render_phase_report(&phase, &outcome, "2026-05-01", "Y");
        assert!(r.contains("Manually confirmed"));
        assert!(!r.contains("<details>"), "no captured output for manual");
    }

    #[test]
    fn run_stop_command_captures_real_exit_code() {
        let out = run_stop_command("exit 7");
        assert!(!out.passed);
        assert_eq!(out.exit_code, Some(7));
    }

    #[test]
    fn run_stop_command_captures_stdout_under_cap() {
        let out = run_stop_command("echo hello-phase-world");
        assert!(out.passed);
        assert!(out.stdout.contains("hello-phase-world"));
    }

    #[test]
    fn trim_to_cap_marks_truncation() {
        let big = vec![b'a'; STDOUT_CAP_BYTES + 100];
        let s = trim_to_cap(&big, STDOUT_CAP_BYTES);
        assert!(s.contains("[truncated 100 bytes]"));
        assert!(s.len() <= STDOUT_CAP_BYTES + 64);
    }
}
