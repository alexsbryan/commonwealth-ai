// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn quality lane <id>` — one lane of the curated breakage check.
//!
//! The RUNNER is `sovereign-cli`'s `quality_check_cmd`: it owns the lane
//! table, the budget, the preconditions and the four-verdict roll-up, and it
//! deliberately touches no model. The LANES are here, because every one of
//! them drives inference, ingests a corpus or runs a judge — and this crate
//! is the one that can.
//!
//! # The contract with the runner
//!
//! In, as environment (declared in `quality/env-flags.toml`):
//!
//! | var | meaning |
//! |---|---|
//! | `SOVEREIGN_QUALITY_FINGERPRINT` | the stack fingerprint; names the baseline dir and the lane's corpus |
//! | `SOVEREIGN_QUALITY_OUT_DIR` | `target/quality-check/<stamp>` for this run's artifacts |
//! | `SOVEREIGN_QUALITY_BUDGET_SECS` | seconds this lane may take before the runner kills it |
//! | `SOVEREIGN_QUALITY_BASELINE_DIR` | where `<fingerprint>/latest.json` lives |
//! | `SOVEREIGN_QUALITY_MINT` | `1` — and only then may a lane write a baseline |
//!
//! Out: the lane's own named rows on stdout, then **one trailing line** — a
//! `kernel_types::Judgement` in the wire form
//! `sovereign_cli_shared::lane_verdict` owns. That line is the only thing
//! the runner reads. A lane that dies before printing it is `never-ran`,
//! which is the honest verdict and the one an exit code cannot express.
//!
//! Run by hand and it behaves identically — no fingerprint means no
//! baseline comparison, which is a could-not-judge on the baseline rows and
//! changes nothing about the absolute ones.

pub(crate) mod chat_ask;
pub(crate) mod throughput;

use std::path::PathBuf;

use kernel_types::{honesty_footer, render_rows, Judgement, Reason, Verdict};
use sovereign_cli_shared::lane_verdict;

/// What the runner told this lane about the run it belongs to.
///
/// Every field is optional because a lane must run by hand — an operator
/// debugging `chat-ask` should not have to fake five environment variables.
/// What changes without them is what can be COMPARED, never what is
/// asserted.
pub(crate) struct LaneCtx {
    /// The stack fingerprint. `None` when run outside the runner.
    pub fingerprint: Option<String>,
    pub out_dir: Option<PathBuf>,
    pub baseline_dir: Option<PathBuf>,
    /// True only under `svrn quality check --mint`.
    pub mint: bool,
}

impl LaneCtx {
    pub fn from_env() -> LaneCtx {
        let nonblank = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let ctx = LaneCtx {
            fingerprint: nonblank("SOVEREIGN_QUALITY_FINGERPRINT"),
            out_dir: nonblank("SOVEREIGN_QUALITY_OUT_DIR").map(PathBuf::from),
            baseline_dir: nonblank("SOVEREIGN_QUALITY_BASELINE_DIR").map(PathBuf::from),
            mint: matches!(nonblank("SOVEREIGN_QUALITY_MINT").as_deref(), Some("1")),
        };
        tracing::debug!(
            fingerprint = ?ctx.fingerprint,
            mint = ctx.mint,
            "quality lane: context from env"
        );
        ctx
    }

    /// The 8-char fingerprint stem lane-local names hang from. `adhoc` when
    /// there is none — a NAMED absence, so a hand run's corpus and a
    /// runner's corpus can never be confused for each other.
    pub fn stem(&self) -> String {
        self.fingerprint
            .as_deref()
            .map(|f| f.chars().take(8).collect())
            .unwrap_or_else(|| "adhoc".to_string())
    }
}

/// A lane's rows, and the one line the runner reads.
pub(crate) struct LaneReport {
    lane: String,
    rows: Vec<Judgement>,
}

impl LaneReport {
    pub fn new(lane: impl Into<String>) -> LaneReport {
        LaneReport {
            lane: lane.into(),
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, j: Judgement) {
        tracing::debug!(
            lane = %self.lane,
            row = %j.subject(),
            verdict = %j.verdict(),
            reason = %j.reason(),
            "quality lane: row"
        );
        self.rows.push(j);
    }

    pub fn passed(&mut self, subject: &str, why: String) {
        self.push(Judgement::passed(subject, reason(why)));
    }
    pub fn failed(&mut self, subject: &str, why: String) {
        self.push(Judgement::failed(subject, reason(why)));
    }
    pub fn cannot_judge(&mut self, subject: &str, why: String) {
        self.push(Judgement::could_not_judge(subject, reason(why)));
    }

    /// The rows so far. A lane's row-shaping functions are where its rules
    /// live, so a test asserts on the VERDICTS they produced rather than on
    /// the prose they printed — the coupling `scripts/lib/ci-bench-verdict.sh`
    /// is made of.
    #[cfg(test)]
    pub fn rows_for_test(&self) -> &[Judgement] {
        &self.rows
    }

    /// Print the rows, the honesty footer, and the trailing verdict line.
    /// Returns the process exit code.
    ///
    /// Run alone, a lane is also a gate, so a not-passed roll-up exits
    /// non-zero. Run by the runner, the exit code is ignored in favour of
    /// the trailing line — which is why a lane never has to encode the
    /// four verdicts into two.
    pub fn finish(self) -> i32 {
        println!();
        print!("{}", render_rows(&self.rows));
        if let Some(f) = honesty_footer(&self.rows) {
            println!();
            println!("  {f}");
        }
        println!();
        let roll = Judgement::roll_up(&self.lane, &self.rows).as_of(lane_verdict::now());
        let verdict = roll.verdict();
        lane_verdict::print(&roll);
        i32::from(verdict != Verdict::Passed)
    }
}

/// A reason is never a placeholder here: every call site formats a sentence
/// with a measured number in it. The fallback exists so a formatting bug
/// cannot panic a 30-minute run.
pub(crate) fn reason(text: String) -> Reason {
    Reason::new(text).unwrap_or_else(|| Reason::literal("the lane reported no detail"))
}

pub async fn run(args: &[String]) -> i32 {
    let lane = args.first().map(String::as_str).unwrap_or("");
    match lane {
        "chat-ask" => chat_ask::run(&args[1..]).await,
        "throughput" => throughput::run(&args[1..]).await,
        "--help" | "-h" | "" => {
            println!("Usage: svrn quality lane <id>");
            println!();
            println!("  chat-ask   The focus lane: ingest the Architecture Tour, ask two");
            println!("             questions three warm times each, and assert the route,");
            println!("             the per-stage ceilings, the gate outcome and usefulness.");
            println!();
            println!("  throughput The engine's own numbers: scripts/throughput_probe.py");
            println!("             over four arms, plus two plain end-to-end turns.");
            println!();
            println!("Normally driven by `svrn quality check`, which reads the trailing");
            println!("Judgement line each lane prints last.");
            i32::from(lane.is_empty())
        }
        other => {
            // Refused, never defaulted to a lane the operator did not ask
            // for (ARCH §18.3).
            eprintln!("svrn quality lane: no lane `{other}`. Declared: chat-ask, throughput");
            2
        }
    }
}
