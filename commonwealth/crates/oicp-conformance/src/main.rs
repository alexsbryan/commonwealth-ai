// SPDX-License-Identifier: AGPL-3.0-or-later
// Contract-adjacent binary: liftable by third parties — public items need
// docs (count-ratcheted by lint-gate, never a hard deny).
#![warn(missing_docs)]
//! `oicp-conformance` — a standalone certifier for OICP v0.4 hosts.
//!
//! Point it at `--host <url>` and it fetches the capabilities manifest, then
//! exercises the inference, constraint, embed, knowledge, and ingest surfaces,
//! emitting one `{id, level, status}` result per check. Feature-gated checks
//! `skip` (never `fail`) when the host doesn't advertise the feature, so the
//! same binary certifies a minimal v0.3 host and a full v0.4 one.
//!
//! Its dependency budget — `oicp-types` + serde/reqwest/tokio, no clap, no
//! jsonschema — is the point: a third party building against OICP can lift this
//! crate wholesale to certify their own implementation.

mod args;
mod checks;
mod report;

use std::process::ExitCode;

use report::{regressions, CheckStatus, Level, Report};

#[tokio::main]
async fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match args::parse(argv) {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };

    let host = checks::Host::new(&parsed.host, parsed.token.clone());
    let mut all = checks::run_all(&host, &parsed).await;

    // `--check <prefix>` filters to a subset (useful when iterating on one area).
    if let Some(prefix) = &parsed.check_prefix {
        all.retain(|c| c.id.starts_with(prefix.as_str()));
    }

    let report = Report {
        host: parsed.host.clone(),
        oicp_version: oicp_types::OICP_VERSION.to_string(),
        checks: all,
    };

    print_summary(&report);

    if let Some(path) = &parsed.report {
        if let Err(e) = write_json(path, &report) {
            eprintln!("warning: could not write report to {path}: {e}");
        } else {
            eprintln!("report written to {path}");
        }
    }

    // Baseline gate.
    let mut regressed = false;
    // A baseline we cannot read is NOT a pass and NOT a regression — it is a
    // gate that did not run. Kept separate from `regressed` so the exit code
    // can say which of the two happened.
    let mut baseline_unreadable = false;
    if let Some(dir) = &parsed.baseline {
        let latest = format!("{}/latest.json", dir.trim_end_matches('/'));
        match read_report(&latest) {
            Ok(Some(base)) => {
                let regs = regressions(&base, &report);
                if regs.is_empty() {
                    eprintln!("baseline: no regressions vs {latest}");
                } else {
                    regressed = true;
                    eprintln!("baseline: {} REGRESSION(S) vs {latest}:", regs.len());
                    for r in &regs {
                        eprintln!("  ✗ {} : {:?} → {:?}", r.id, r.was, r.now);
                    }
                }
            }
            Ok(None) => eprintln!("baseline: no prior at {latest} (first run — nothing to diff)"),
            Err(e) => {
                baseline_unreadable = true;
                eprintln!("baseline: COULD NOT JUDGE — {e}");
                eprintln!("  the ratchet did not run. This is not a pass.");
                eprintln!("  re-mint it on a known-good host: --baseline {dir} --update-baseline");
            }
        }
        if parsed.update_baseline {
            update_baseline(dir, &report);
        }
    } else if parsed.update_baseline {
        eprintln!("warning: --update-baseline needs --baseline <dir>; ignored");
    }

    let should_failed = parsed.strict
        && report
            .checks
            .iter()
            .any(|c| c.level == Level::Should && c.status == CheckStatus::Fail);

    // 2 is "could not judge" throughout this binary (see the usage-error arm
    // above), and it must be distinguishable from 1: a corrupt baseline needs
    // a human to re-mint it, a regression needs a human to read a diff.
    if baseline_unreadable {
        ExitCode::from(2)
    } else if !report.is_conformant() || regressed || should_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_summary(report: &Report) {
    let (pass, fail, skip) = report.counts();
    eprintln!(
        "\noicp-conformance — {} (OICP {})",
        report.host, report.oicp_version
    );
    for c in &report.checks {
        let mark = match c.status {
            CheckStatus::Pass => "✓",
            CheckStatus::Fail => "✗",
            CheckStatus::Skip => "–",
        };
        let level = match c.level {
            Level::Must => "must",
            Level::Should => "should",
            Level::Feature => "feat",
        };
        eprintln!("  {mark} [{level:<6}] {:<28} {}", c.id, c.detail);
    }
    eprintln!(
        "\n{pass} passed, {fail} failed, {skip} skipped — {}",
        if report.is_conformant() {
            "CONFORMANT"
        } else {
            "NON-CONFORMANT (a `must` failed)"
        }
    );
}

fn write_json(path: &str, report: &Report) -> std::io::Result<()> {
    let body = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}

/// Read a prior report, keeping ABSENT and UNREADABLE apart.
///
/// `Ok(None)` is a real first run. `Err` is a baseline that exists and cannot
/// be understood — a corrupt file, a truncated write, a schema this binary is
/// too old to parse. Collapsing the two (both were `None` until 2026-08-31)
/// disarms the ratchet silently: the run prints "first run — nothing to diff"
/// and exits 0 while every regression it was built to catch sails through.
/// Four verdicts, not two (ARCH_PRINCIPLES §18.2); absence is reported, never
/// defaulted (§18.3).
fn read_report(path: &str) -> Result<Option<Report>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {path}: {e}")),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("cannot parse {path}: {e}"))
}

/// Write `latest.json` plus a dated snapshot so history is auditable.
fn update_baseline(dir: &str, report: &Report) {
    let dir = dir.trim_end_matches('/');
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("warning: could not create baseline dir {dir}: {e}");
        return;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let latest = format!("{dir}/latest.json");
    let dated = format!("{dir}/baseline-{stamp}.json");
    for path in [&latest, &dated] {
        if let Err(e) = write_json(path, report) {
            eprintln!("warning: could not write baseline {path}: {e}");
        }
    }
    eprintln!("baseline updated: {latest} (+ {dated})");
}

#[cfg(test)]
mod tests {
    use super::read_report;

    /// The defect this pins, measured 2026-08-31: `read_report` used to be
    /// `read_to_string(..).ok()?` + `from_str(..).ok()`, so a baseline that
    /// EXISTED but could not be parsed came back `None` — identical to no
    /// baseline at all. The run then printed "first run — nothing to diff"
    /// and exited 0. Every regression the ratchet exists to catch went
    /// through, and the only visible symptom was a reassuring sentence.
    #[test]
    fn an_unparseable_baseline_is_not_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("oicp-rr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latest.json");

        // ABSENT — a real first run.
        let _ = std::fs::remove_file(&path);
        assert!(
            matches!(read_report(path.to_str().unwrap()), Ok(None)),
            "a missing baseline must read as a legitimate first run"
        );

        // PRESENT BUT UNPARSEABLE — the gate did not run.
        std::fs::write(&path, "{ this is not a report }").unwrap();
        let err = read_report(path.to_str().unwrap())
            .expect_err("an unparseable baseline must NOT read as 'no baseline'");
        assert!(
            err.contains("cannot parse"),
            "the error must name what went wrong, got: {err}"
        );

        // PRESENT AND VALID — the ordinary path still works.
        std::fs::write(
            &path,
            r#"{"host":"h","oicp_version":"0.4.0","checks":[]}"#,
        )
        .unwrap();
        assert!(
            matches!(read_report(path.to_str().unwrap()), Ok(Some(_))),
            "a valid baseline must still parse"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
