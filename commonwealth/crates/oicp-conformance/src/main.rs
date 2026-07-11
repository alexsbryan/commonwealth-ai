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
    if let Some(dir) = &parsed.baseline {
        let latest = format!("{}/latest.json", dir.trim_end_matches('/'));
        match read_report(&latest) {
            Some(base) => {
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
            None => eprintln!("baseline: no prior at {latest} (first run — nothing to diff)"),
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

    if !report.is_conformant() || regressed || should_failed {
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

fn read_report(path: &str) -> Option<Report> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
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
