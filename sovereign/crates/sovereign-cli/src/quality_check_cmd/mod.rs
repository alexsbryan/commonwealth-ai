// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn quality check` — the curated 30-minute read on whether the resident
//! stack is BROKEN. Not whether it drifted; that is the nightly's job.
//!
//! # Why a second runner when `sovereign-ci-bench.sh` exists
//!
//! Three measured reasons, none of them "the old one is ugly":
//!
//! 1. **ci-bench keeps nothing.** `target/ci-bench` is empty: the lane table
//!    is echoed to a terminal and discarded, so "was this lane slower last
//!    week" has no answer on this host. Every run here writes
//!    `target/quality-check/<stamp>/summary.json` with per-lane seconds.
//! 2. **It reconstructs verdicts by grepping lane prose.**
//!    `scripts/lib/ci-bench-verdict.sh` is 130 lines of `grep -qE` against
//!    wording no lane promised to keep — including the daemon's own
//!    unreachability strings. Here the lane SAYS its verdict, as a
//!    [`Judgement`] on its last stdout line
//!    (`sovereign_cli_shared::lane_verdict`).
//! 3. **It is `--quick` in name only.** The lean tier is this command; the
//!    script keeps the full nightly.
//!
//! # The four verdicts are the whole design (ARCH §18.1, §18.2)
//!
//! - **passed** — the lane ran and every assertion held.
//! - **failed** — the lane ran and something did not.
//! - **could-not-judge** — a precondition was missing, the budget ran out
//!   before the lane could start, or the lane itself could not reach a
//!   verdict. Never a pass; a HARD lane goes red on it, because "suddenly
//!   nothing to judge" on a gated surface is a regression signal.
//! - **never-ran** — the lane produced no verdict line at all. The exit code
//!   is the reason. This is the one an exit-code-only runner cannot express,
//!   and it is the one a crashed lane earns.
//!
//! # What this command will not do
//!
//! It will not write a baseline. A run whose stack has no baseline for its
//! [`Fingerprint`] is `could-not-judge (first-run)` on its BASELINE-DERIVED
//! rows and writes nothing; `--mint` is the only door. Absolute rows —
//! pre-registered ceilings, a gate outcome, a usefulness bar — need no
//! baseline and are judged on the run in front of them, which is why the
//! `chat-ask` lane can fail on its very first run instead of reporting
//! first-run and teaching nobody anything.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use kernel_types::quality::{
    Enforcement, Instrument, Overrun, Registry, RunsIn, Trigger, VenueAction,
};
use kernel_types::{honesty_footer, render_rows, Judgement, Reason};
use sovereign_cli_shared::lane_verdict;

mod exec;
mod fingerprint;
mod report;
mod select;

use exec::{
    check_precondition, describe_precondition, finish, resolve_program, spawn_instrument, InFlight,
    InstrumentRun,
};
use fingerprint::{compute_fingerprint, Fingerprint};
use report::{print_selection, report_one, stamp_now, write_summary};
use select::{baseline_dir, changed_paths, comparable_baselines, run_order, selected_by_change};

/// How often the runner checks on a lane subprocess.
const POLL: Duration = Duration::from_millis(250);

// ─── The registry (ONE table, ARCH §10.6) ───────────────────────────

/// Where the instrument registry lives, relative to the repo root.
///
/// It used to be `quality/check-lanes.toml`, a SECOND table answering "what
/// runs here" beside `quality/instruments.toml` — two schemas whose fields
/// collided on `kind`, `enforcement`, cost and `baseline` with different
/// meanings on each side. They merged on 2026-09-07 and this constant is what
/// is left: one file, one parser (`kernel_types::quality::Registry`), read by
/// this runner, `xtask instrument-gate`, `svrn quality map` and `svrn
/// posture`.
const REGISTRY: &str = "quality/instruments.toml";

/// The venue `svrn quality check` runs when no `--trigger` is given.
const DEFAULT_VENUE: RunsIn = RunsIn::Check;

// ─── The stack fingerprint ──────────────────────────────────────────

// ─── Preconditions ──────────────────────────────────────────────────

// ─── Running one instrument ─────────────────────────────────────────

// ─── The command ────────────────────────────────────────────────────

struct Args {
    /// `--lane <id>` — narrow the trigger's selection to these ids.
    ids: Vec<String>,
    /// `--trigger <venue>`. `None` means [`DEFAULT_VENUE`].
    trigger: Option<String>,
    /// `--budget-secs <n>` overrides the trigger's declared budget.
    budget_secs: Option<u64>,
    mint: bool,
    table: Option<PathBuf>,
    /// `--dry-run` prints the selection and runs nothing.
    dry_run: bool,
    /// `--list` prints one selected id per line and runs nothing.
    ///
    /// The machine-readable half of `--dry-run`. It exists because a shell
    /// venue needs the id set to build its own state — `run-if-stale.sh` keys
    /// a staleness marker per lane — and parsing the human table would be a
    /// second reader of a rendering nobody promised to keep, which is the
    /// defect `scripts/lib/ci-bench-verdict.sh` is 130 lines of.
    list: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        ids: Vec::new(),
        trigger: None,
        budget_secs: None,
        mint: false,
        table: None,
        dry_run: false,
        list: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--lane" | "--id" => {
                let v = args.get(i + 1).ok_or("--lane needs an instrument id")?;
                out.ids.push(v.clone());
                i += 1;
            }
            "--trigger" => {
                let v = args.get(i + 1).ok_or("--trigger needs a venue")?;
                out.trigger = Some(v.clone());
                i += 1;
            }
            "--budget-secs" => {
                let v = args.get(i + 1).ok_or("--budget-secs needs a number")?;
                out.budget_secs = Some(
                    v.parse()
                        .map_err(|_| format!("--budget-secs: `{v}` is not a number"))?,
                );
                i += 1;
            }
            "--mint" => out.mint = true,
            "--dry-run" => out.dry_run = true,
            "--list" => out.list = true,
            "--lane-table" | "--registry" => {
                let v = args.get(i + 1).ok_or("--registry needs a path")?;
                out.table = Some(PathBuf::from(v));
                i += 1;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
        i += 1;
    }
    Ok(out)
}

/// `svrn quality <subcommand>` — the verb's own router.
///
/// It lives beside the runner rather than in `main.rs` because `main.rs` is
/// a DISPATCHER: 1,500 lines of verb table and exec hops, sitting on
/// ARCH §3.1's slack. A subcommand split belongs with the subcommand.
///
/// `exec_lane` is passed in rather than named here: the LANES live in
/// `sovereign-cli-llm` (each drives inference, ingests a corpus or runs a
/// judge) and this crate reaches that sibling by exec, which is the
/// dispatcher's business, not the runner's.
pub async fn run_verb(args: &[String], exec_lane: impl Fn(&str, &[String]) -> i32) -> i32 {
    match args.first().map(String::as_str) {
        Some("check") => run(&args[1..]).await,
        Some("lane") => exec_lane("quality-lane", &args[1..]),
        // An unknown subcommand is REFUSED, never defaulted to `check`
        // (ARCH §18.3): running a 30-minute suite because someone typo'd a
        // lane name is not a courtesy.
        Some(other) if other != "--help" && other != "-h" => {
            eprintln!("svrn quality: unknown subcommand `{other}`. Try: svrn quality check");
            2
        }
        _ => {
            println!("Usage: svrn quality check [--trigger <venue>] [--lane <id>]... [--dry-run]");
            println!("       svrn quality lane <id>");
            println!();
            println!("  check   Run one VENUE's selection from quality/instruments.toml and");
            println!("          write the table to target/quality-check/<stamp>/summary.json");
            println!("  lane    Run ONE lane directly, printing its own rows. The");
            println!("          runner above drives the same command per lane.");
            0
        }
    }
}

pub async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        crate::util::help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let Some(repo) = crate::posture_cmd::find_repo_root() else {
        eprintln!("error: `svrn quality check` reads {REGISTRY} — run it from a source checkout");
        return 2;
    };
    let table_path = parsed.table.clone().unwrap_or_else(|| repo.join(REGISTRY));
    let text = match std::fs::read_to_string(&table_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", table_path.display());
            return 2;
        }
    };
    // A malformed row REFUSES the whole file rather than being skipped: one
    // silently dropped from a check is one whose absence reads as a pass.
    let registry = match Registry::parse(&text) {
        Ok(r) => r,
        Err(errs) => {
            eprintln!("error: {} is not valid:", table_path.display());
            for e in &errs {
                eprintln!("  ✗ {e}");
            }
            return 2;
        }
    };

    // ── Selection ───────────────────────────────────────────────────
    let venue_word = parsed
        .trigger
        .clone()
        .unwrap_or_else(|| DEFAULT_VENUE.label());
    let Some(venue) = RunsIn::parse(&venue_word) else {
        eprintln!("error: `{venue_word}` is not a venue. See `runs_in` in {REGISTRY}.");
        return 2;
    };
    let Some(trigger) = registry.trigger(&venue).cloned() else {
        eprintln!(
            "error: {REGISTRY} declares no [[trigger]] for `{venue_word}`. A venue with no run \
             policy has no budget, no concurrency and no failure disposition — it is not \
             runnable, and guessing one here would invent the very thing the table exists to \
             declare."
        );
        return 2;
    };
    let in_venue = registry.for_venue(&venue);
    for want in &parsed.ids {
        if !in_venue.iter().any(|l| &l.id == want) {
            eprintln!(
                "error: no instrument `{want}` runs in `{venue_word}`. Declared there: {}",
                in_venue
                    .iter()
                    .map(|l| l.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return 2;
        }
    }
    let changed = changed_paths();
    let mut deselected: Vec<&str> = Vec::new();
    let lanes: Vec<&Instrument> = in_venue
        .iter()
        .copied()
        .filter(|l| {
            if !parsed.ids.is_empty() && !parsed.ids.contains(&l.id) {
                return false;
            }
            if selected_by_change(l, changed.as_ref()) {
                true
            } else {
                deselected.push(&l.id);
                false
            }
        })
        .collect();

    let budget_secs = parsed.budget_secs.unwrap_or(trigger.budget_secs);

    if parsed.list {
        for l in &lanes {
            println!("{}", l.id);
        }
        // An EMPTY listing is not a listing. A venue whose selection is empty
        // has nothing to run, and a caller that read zero lines as "no lanes
        // today" would install a trigger that fires into nothing — the exact
        // shape `wizard-verify.sh` took for ten days. Exit 4 AND a reason: a
        // bare exit code is an absence reported as a number, which is the
        // thin end of reporting it not at all (ARCH §18.3).
        if lanes.is_empty() {
            eprintln!(
                "no instrument runs in `{venue_word}`{} — this venue would fire into nothing",
                if deselected.is_empty() {
                    String::new()
                } else {
                    format!(" after `when_changed` deselected {}", deselected.join(", "))
                }
            );
            return 4;
        }
        return 0;
    }
    if parsed.dry_run {
        print_selection(&venue_word, &trigger, &lanes, &deselected, budget_secs);
        return 0;
    }
    if lanes.is_empty() {
        // Same claim as `sovereign-test.sh`'s exit 4: nothing ran, so nothing
        // was verified, and that is never a pass.
        eprintln!(
            "nothing selected for `{venue_word}` — verified nothing.{}",
            if deselected.is_empty() {
                String::new()
            } else {
                format!(
                    " {} instrument(s) were deselected by `when_changed`: {}",
                    deselected.len(),
                    deselected.join(", ")
                )
            }
        );
        return 4;
    }

    let base = sovereign_cli_shared::urls::daemon_base_url();
    let fingerprint = match compute_fingerprint(&repo, &lanes, &base).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    // Only a venue whose instruments compare against a per-stack baseline
    // needs the fingerprint printed first; for a ratchet venue it is noise
    // about models nothing here read.
    let comparable = comparable_baselines(&repo, &lanes, &fingerprint);
    if lanes.iter().any(|l| baseline_dir(l).is_some()) {
        print!("{}", fingerprint.render());
        println!(
            "{comparable} of {} lanes have a comparable baseline for this stack{}",
            lanes.len(),
            if parsed.mint {
                " · --mint: lanes may write one"
            } else {
                ""
            }
        );
        println!();
    }
    if !deselected.is_empty() {
        // NAMED, never silent. An instrument left out because nothing it
        // gates changed is SELECTION, and the reader gets to see it.
        println!(
            "not selected — no changed path matches their `when_changed`: {}",
            deselected.join(", ")
        );
        println!();
    }

    let stamp = stamp_now();
    let out_dir = repo.join("target/quality-check").join(&stamp);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: cannot create {}: {e}", out_dir.display());
        return 2;
    }

    // ── Prepare, once, before anything runs ─────────────────────────
    //
    // A failed prepare step does NOT abort: its substitution is simply not
    // applied, every instrument runs its declared command, and the venue is
    // SLOW rather than blind.
    let mut applied: Vec<usize> = Vec::new();
    for (i, p) in trigger.prepare.iter().enumerate() {
        let t0 = Instant::now();
        let ok = std::process::Command::new(resolve_program(&p.command[0]))
            .args(&p.command[1..])
            .current_dir(&repo)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        tracing::debug!(step = ?p.command, ok, secs = t0.elapsed().as_secs(), "quality check: prepare");
        if ok {
            applied.push(i);
        } else {
            eprintln!(
                "warning: prepare step `{}` failed — its instruments will run their declared \
                 command instead (slower, not weaker)",
                p.command.join(" ")
            );
        }
    }

    let started = Instant::now();
    let budget = Duration::from_secs(budget_secs);
    let mut results: BTreeMap<String, InstrumentRun> = BTreeMap::new();

    let queue = run_order(&lanes, trigger.concurrency);

    let mut flight: Vec<InFlight> = Vec::new();
    let mut next = 0usize;
    loop {
        // Start whatever fits.
        while flight.len() < trigger.concurrency && next < queue.len() {
            let inst = queue[next];
            next += 1;
            let remaining = budget.saturating_sub(started.elapsed()).as_secs();
            if remaining <= trigger.min_runway_secs {
                println!(
                    "── SKIP(budget)  [{}] {}",
                    inst.enforcement.label(),
                    inst.id
                );
                results.insert(
                    inst.id.clone(),
                    InstrumentRun {
                        judgement: Judgement::could_not_judge(
                            inst.id.clone(),
                            Reason::new(format!(
                                "out of budget — {remaining}s left of {budget_secs}s, and this \
                                 wants ~{}s",
                                inst.reservation_secs()
                            ))
                            .expect("never a placeholder"),
                        ),
                        secs: 0,
                        exit_code: None,
                        tail: String::new(),
                    },
                );
                continue;
            }
            // Preconditions. An unmet one is could-not-judge NAMING it, and
            // it does not run — an unmet precondition tells you nothing about
            // the code under test.
            let mut unmet: Vec<String> = Vec::new();
            for p in &inst.preconditions {
                if !check_precondition(p, &base).await {
                    unmet.push(describe_precondition(p));
                }
            }
            if !unmet.is_empty() {
                println!(
                    "── SKIP(precondition)  [{}] {} — {}",
                    inst.enforcement.label(),
                    inst.id,
                    unmet.join("; ")
                );
                results.insert(
                    inst.id.clone(),
                    InstrumentRun {
                        judgement: Judgement::could_not_judge(
                            inst.id.clone(),
                            Reason::new(format!("precondition unmet: {}", unmet.join("; ")))
                                .expect("never a placeholder"),
                        ),
                        secs: 0,
                        exit_code: None,
                        tail: String::new(),
                    },
                );
                continue;
            }
            // THE CAP, and why it is not always the budget. A venue that
            // KILLS on overrun gives an instrument whatever runway is left;
            // one that REPORTS gives it none at all, because stopping the
            // workspace compile five seconds past a soft budget turns the
            // gate that matters most into an abstention nobody earned
            // (ARCH §18.2). Watched: the first selection-driven pre-push run
            // killed `sovereign-lint-scoped` at 59s of a 60s budget.
            let cap = match trigger.overrun {
                Overrun::Kill => remaining,
                Overrun::Report => u64::MAX,
            };
            let argv = trigger.rewrite(&inst.argv(), &applied);
            println!(
                "── RUN   [{}] {}   (budget left {remaining}s, est {}s)",
                inst.enforcement.label(),
                inst.id,
                inst.reservation_secs()
            );
            match spawn_instrument(inst, &argv, &repo, &out_dir, &fingerprint, parsed.mint, cap) {
                Ok(f) => flight.push(f),
                Err(run) => {
                    report_one(inst, &run);
                    results.insert(inst.id.clone(), run);
                }
            }
        }
        if flight.is_empty() {
            if next >= queue.len() {
                break;
            }
            continue;
        }
        // Poll what is running.
        let mut done: Option<usize> = None;
        for (i, f) in flight.iter_mut().enumerate() {
            match f.child.try_wait() {
                Ok(Some(_)) => {
                    done = Some(i);
                    break;
                }
                Ok(None) => {
                    if f.started.elapsed() >= Duration::from_secs(f.cap_secs) {
                        let _ = f.child.kill();
                        let _ = f.child.wait();
                        done = Some(i);
                        break;
                    }
                }
                Err(_) => {
                    done = Some(i);
                    break;
                }
            }
        }
        match done {
            None => tokio::time::sleep(POLL).await,
            Some(i) => {
                let mut f = flight.remove(i);
                let status = match f.child.try_wait() {
                    Ok(Some(s)) => s,
                    _ => match f.child.wait() {
                        Ok(s) => s,
                        Err(e) => {
                            let run = InstrumentRun {
                                judgement: Judgement::could_not_judge(
                                    f.inst.id.clone(),
                                    Reason::new(format!("cannot wait on the process: {e}"))
                                        .expect("never a placeholder"),
                                ),
                                secs: f.started.elapsed().as_secs(),
                                exit_code: None,
                                tail: String::new(),
                            };
                            report_one(f.inst, &run);
                            results.insert(f.inst.id.clone(), run);
                            continue;
                        }
                    },
                };
                let inst = f.inst;
                let run = finish(f, status);
                report_one(inst, &run);
                results.insert(inst.id.clone(), run);
            }
        }
    }

    // ── The table ───────────────────────────────────────────────────
    let total_secs = started.elapsed().as_secs();
    let rows: Vec<Judgement> = lanes
        .iter()
        .filter_map(|l| results.get(&l.id).map(|r| r.judgement.clone()))
        .collect();
    println!();
    print!("{}", render_rows(&rows));
    if let Some(footer) = honesty_footer(&rows) {
        println!();
        println!("  {footer}");
    }
    println!();
    println!(
        "  total {total_secs}s of a {budget_secs}s budget · logs + summary: {}",
        out_dir.display()
    );

    let summary_path = out_dir.join("summary.json");
    if let Err(e) = write_summary(
        &summary_path,
        &stamp,
        &venue_word,
        &fingerprint,
        &lanes,
        &results,
        total_secs,
        budget_secs,
        comparable,
    ) {
        // The durable table is the reason this command exists. Losing it is
        // not a footnote.
        eprintln!("error: cannot write {}: {e}", summary_path.display());
        return 2;
    }

    // ── The exit code ───────────────────────────────────────────────
    //
    // Two questions, two fields. `enforcement` is the INSTRUMENT's: may this
    // row fail a run. `on_fail` / `on_could_not_judge` are the VENUE's: and
    // does the venue then stop. `pre-commit` runs real checks and still exits
    // 0; `pre-push` blocks on a gate that said no and only WARNS on one that
    // could not run on this host.
    let mut blocking: Vec<&str> = Vec::new();
    let mut reported: Vec<&str> = Vec::new();
    for l in &lanes {
        let Some(r) = results.get(&l.id) else {
            continue;
        };
        if l.enforcement != Enforcement::Hard {
            continue;
        }
        let action = match r.judgement.verdict() {
            kernel_types::Verdict::Passed => continue,
            kernel_types::Verdict::CouldNotJudge => trigger.on_could_not_judge,
            _ => trigger.on_fail,
        };
        match action {
            VenueAction::Block => blocking.push(&l.id),
            VenueAction::Report => reported.push(&l.id),
        }
    }
    if !reported.is_empty() {
        println!();
        println!(
            "  {} hard instrument(s) not passed, REPORTED not blocking here: {}",
            reported.len(),
            reported.join(", ")
        );
    }
    if !blocking.is_empty() {
        println!();
        println!("  {} blocking: {}", blocking.len(), blocking.join(", "));
        return 1;
    }
    0
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn quality check",
    summary: "Run one VENUE's selection from quality/instruments.toml — four verdicts, persisted.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn quality check [--trigger <venue>] [--lane <id>]... [--budget-secs <n>] [--mint] [--dry-run]",
        ),
        crate::util::help::HelpSection::Flags(&[
            (
                "--trigger <venue>",
                "Which venue's selection to run: check (default), prepush, precommit, smoke:<n>, ci:<job>, run-if-stale. The venue must have a [[trigger]] row.",
            ),
            (
                "--lane <id>",
                "Narrow the selection to this instrument (repeatable). An id that does not run in the venue is refused, never ignored.",
            ),
            (
                "--dry-run",
                "Print the selection, the run order and the budget arithmetic; run nothing.",
            ),
            (
                "--list",
                "Print one selected instrument id per line and run nothing. Exit 4 if the selection is empty.",
            ),
            (
                "--budget-secs <n>",
                "Override the trigger's declared wall budget. An instrument with less runway than the trigger's min is could-not-judge, not a pass.",
            ),
            (
                "--mint",
                "Permit lanes to write a baseline for this stack fingerprint. Without it a first run writes none.",
            ),
            (
                "--registry <path>",
                "Read a different registry (default quality/instruments.toml).",
            ),
        ]),
        crate::util::help::HelpSection::Examples(&[
            ("svrn quality check", "the eight check lanes, 30-minute budget"),
            ("svrn quality check --lane chat-ask", "the focus lane alone"),
            (
                "svrn quality check --trigger prepush --dry-run",
                "what the push gate would run, in order, against its 60s budget",
            ),
        ]),
    ],
};

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use kernel_types::quality::VerdictSource;

    /// A registry with one ratchet and one lane, and a trigger for each — the
    /// smallest thing that exercises BOTH verdict sources and both venues.
    pub(super) const TABLE: &str = r#"
censused_surfaces = [".github/workflows"]

[[instrument]]
id = "docs-gate"
kind = "gate"
claim = "invariant"
command = "cargo xtask docs-gate"
cost_secs = 2.2
enforcement = "hard"
fidelity = "F0"
baseline = { kind = "none" }
negative_control = "none"
runs_in = ["prepush"]
when_changed = "\\.rs$"
doc = "ARCH §1.1"

[[instrument]]
id = "chat-ask"
kind = "bench"
claim = "judged"
command = "svrn quality lane chat-ask"
argv = ["svrn", "quality", "lane", "chat-ask"]
cost_secs = 251.0
est_secs = 300
enforcement = "hard"
fidelity = "F3"
verdict = "judgement-line"
preconditions = ["port-listening:9741", "slot-decodes:primary"]
baseline = { kind = "fingerprint", path = "sovereign/bench/quality-check/baselines/chat-ask" }
bank = "sovereign/bench/quality-check/chat-ask.toml"
negative_control = "none"
runs_in = ["check"]
doc = "the focus lane"

[[trigger]]
id = "prepush"
budget_secs = 60
concurrency = 2
min_runway_secs = 1
on_fail = "block"
on_could_not_judge = "report"
[[trigger.prepare]]
command = ["cargo", "build", "--quiet", "-p", "xtask"]
substitute_from = ["cargo", "xtask"]
substitute_to = ["target/debug/xtask"]

[[trigger]]
id = "check"
budget_secs = 1800
concurrency = 1
min_runway_secs = 60
on_fail = "block"
on_could_not_judge = "block"
"#;

    pub(super) fn reg() -> Registry {
        Registry::parse(TABLE).expect("parses")
    }

    #[test]
    fn args_parse_and_reject() {
        let a = parse_args(&[
            "--trigger".into(),
            "prepush".into(),
            "--lane".into(),
            "chat-ask".into(),
            "--budget-secs".into(),
            "600".into(),
            "--mint".into(),
            "--dry-run".into(),
        ])
        .unwrap();
        assert_eq!(a.trigger.as_deref(), Some("prepush"));
        assert_eq!(a.ids, vec!["chat-ask"]);
        assert_eq!(a.budget_secs, Some(600));
        assert!(a.mint);
        assert!(a.dry_run);
        assert!(!a.list);
        assert!(parse_args(&["--list".into()]).unwrap().list);
        // No trigger means the check lanes — the behaviour before this flag
        // existed, kept.
        assert_eq!(parse_args(&[]).unwrap().trigger, None);
        assert_eq!(DEFAULT_VENUE.label(), "check");
        assert!(parse_args(&["--lane".into()]).is_err());
        assert!(parse_args(&["--trigger".into()]).is_err());
        assert!(parse_args(&["--budget-secs".into(), "soon".into()]).is_err());
        assert!(parse_args(&["--wat".into()]).is_err());
        // The default is NOT mint. A run that writes a baseline it was not
        // asked to write is the defect this command was built against.
        assert!(!parse_args(&[]).unwrap().mint);
    }

    /// THE REGISTRY THIS REPO ACTUALLY SHIPS parses, every declared trigger
    /// selects something, and the venues the shell harnesses delegate to are
    /// all present. A schema that only parses its own fixtures is a schema
    /// nobody has watched read the real file.
    #[test]
    fn the_shipped_registry_parses_and_every_trigger_selects_something() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../quality/instruments.toml");
        let text = std::fs::read_to_string(&path).expect("quality/instruments.toml");
        let r = match Registry::parse(&text) {
            Ok(r) => r,
            Err(e) => panic!("{}: {e:#?}", path.display()),
        };
        assert!(!r.triggers.is_empty(), "no [[trigger]] rows");
        for t in &r.triggers {
            let sel = r.for_venue(&t.id);
            assert!(
                !sel.is_empty(),
                "trigger `{}` selects nothing — it would report green having run \
                 nothing (ARCH §18.1)",
                t.id.label()
            );
        }
        // The eight lanes of `svrn quality check`, out of the one table.
        let check = r.for_venue(&RunsIn::Check);
        assert_eq!(check.len(), 8, "the check venue must hold its eight lanes");
        for l in &check {
            assert_eq!(
                l.verdict,
                VerdictSource::JudgementLine,
                "lane `{}` must SAY its verdict — an exit code cannot express \
                 could-not-judge",
                l.id
            );
        }
        // `runs_in = []` is legal in the schema and is the finding this
        // registry exists to surface. This rung closed the last nine.
        assert!(
            r.nowhere().is_empty(),
            "instruments that run nowhere: {:?}",
            r.nowhere().iter().map(|i| &i.id).collect::<Vec<_>>()
        );
    }
}
