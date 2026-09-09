// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the runner PRINTS: one instrument's outcome, the dry-run selection,
//! and the durable `summary.json`.

use std::collections::BTreeMap;
use std::path::Path;

use kernel_types::quality::{Instrument, Trigger};
use kernel_types::{render_rows, Judgement};

use super::exec::InstrumentRun;
use super::fingerprint::Fingerprint;
use super::select::run_order;

/// Print one instrument's outcome, with the tail of its output on a red.
///
/// The tail rather than a grep for `✗|FAIL|^error`: every gate in this repo
/// ends its output with its own fix command, and matching on wording no gate
/// promised to keep is the defect `scripts/lib/ci-bench-verdict.sh` is 130
/// lines of.
pub(super) fn report_one(inst: &Instrument, run: &InstrumentRun) {
    println!(
        "── {}  [{}] {}   ({}s)",
        run.judgement.verdict(),
        inst.enforcement.label(),
        inst.id,
        run.secs
    );
    if run.judgement.verdict() != kernel_types::Verdict::Passed && !run.tail.is_empty() {
        for line in run.tail.lines() {
            println!("     {line}");
        }
    }
}

/// The verdict table, with the `node` column beside it (cw-lift 5e).
///
/// **`render_rows` is still the one renderer.** It owns the subject / label /
/// age / reason columns and their widths, and this function does not
/// reimplement any of it — it zips the SAME `rows` slice against the lines
/// that renderer produced and appends one field. A second table here is what
/// would make a distributed verdict stop being diffable against a local one,
/// which is the whole reason the column exists (ARCH §10.6).
///
/// The column is printed on BOTH paths and for every row, with `—` where the
/// row names no node. A column that appeared only on distributed runs would
/// make the two tables different SHAPES, and the acceptance gate for this
/// rung is a diff between them.
pub(super) fn render_table(rows: &[Judgement], nodes: &[Option<String>]) -> String {
    let table = render_rows(rows);
    let mut out = String::new();
    for (i, line) in table.lines().enumerate() {
        match nodes.get(i) {
            Some(node) => out.push_str(&format!("{line}   [{}]\n", node_label(node.as_deref()))),
            // More rendered lines than rows means `render_rows` changed shape
            // under this function. Print the line unchanged rather than
            // dropping it: a row missing from the table is the one failure
            // this rung is not allowed to have.
            None => out.push_str(&format!("{line}\n")),
        }
    }
    out
}

/// A node key, shortened for a terminal, or the named absence.
///
/// Never "local" and never this node's key: a row with no node had no actor
/// take part in it, and inventing one would be a substitution (ARCH §18.3).
pub(super) fn node_label(node: Option<&str>) -> String {
    match node {
        Some(k) if k.len() > 12 => format!("{}…", &k[..12]),
        Some(k) => k.to_string(),
        None => "—".to_string(),
    }
}

/// The `--dry-run` render: what WOULD run, in the order it would run, with
/// the budget arithmetic visible.
///
/// This is the cheapest instrument for the selection itself (ARCH §18.1). A
/// selection that silently picks the wrong set produces a green venue that
/// checked nothing, and the only way to watch that fail is to print it.
pub(super) fn print_selection(
    venue: &str,
    trigger: &Trigger,
    lanes: &[&Instrument],
    deselected: &[&str],
    budget_secs: u64,
) {
    println!(
        "trigger {venue}: budget {budget_secs}s · concurrency {} · min runway {}s · \
         overrun {} · on_fail {} · on_could_not_judge {}",
        trigger.concurrency,
        trigger.min_runway_secs,
        trigger.overrun.label(),
        trigger.on_fail.label(),
        trigger.on_could_not_judge.label()
    );
    for p in &trigger.prepare {
        print!("  prepare: {}", p.command.join(" "));
        match &p.substitute {
            Some((from, to)) => {
                println!("   → rewrites `{}` to `{}`", from.join(" "), to.join(" "))
            }
            None => println!(),
        }
    }
    println!();
    // The SAME decider the run uses, never a second sort that could disagree
    // with it (ARCH §10.6). A dry run that prints an order the real run does
    // not take is worse than no dry run.
    let order = run_order(lanes, trigger.concurrency);
    let reserved: u64 = lanes.iter().map(|l| l.reservation_secs()).sum();
    println!(
        "{} selected, {}s reserved of {budget_secs}s ({} slot(s), so the wall floor is ~{}s)",
        lanes.len(),
        reserved,
        trigger.concurrency,
        reserved / trigger.concurrency.max(1) as u64
    );
    println!();
    println!(
        "  {:<28} {:<9} {:<10} {:>6}  {}",
        "id", "enforce", "claim", "est", "command"
    );
    for l in order {
        println!(
            "  {:<28} {:<9} {:<10} {:>5}s  {}",
            l.id,
            l.enforcement.label(),
            l.claim.label(),
            l.reservation_secs(),
            l.command
        );
    }
    if !deselected.is_empty() {
        println!();
        println!(
            "  not selected — no changed path matches their `when_changed`: {}",
            deselected.join(", ")
        );
    }
}

/// The durable `quality-check/v1` table.
///
/// `submitted_by` is the actor key that SUBMITTED this run's work on a
/// `--distribute` run. `None` on a local run and that is the honest reading,
/// not a gap: a local run signs nothing, so no actor submitted it. Paired
/// with each row's `node`, it is the whole input to the offload share
/// (`scripts/cw-work-offload-share.sh`), and both are written by ONE path so
/// a local run and a distributed one are read by one formula.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_summary(
    path: &Path,
    stamp: &str,
    venue: &str,
    fp: &Fingerprint,
    lanes: &[&Instrument],
    results: &BTreeMap<String, InstrumentRun>,
    total_secs: u64,
    budget_secs: u64,
    comparable: usize,
    submitted_by: Option<&str>,
) -> std::io::Result<()> {
    let lane_rows: Vec<serde_json::Value> = lanes
        .iter()
        .filter_map(|l| results.get(&l.id).map(|r| (l, r)))
        .map(|(l, r)| {
            serde_json::json!({
                "id": l.id,
                // `kind` is the SHAPE and `claim` is what sort of claim the
                // verdict is. Both are written because a reader needs both:
                // the merged registry stopped one field answering two
                // questions, and the summary must not put it back.
                "kind": l.kind.label(),
                "claim": l.claim.label(),
                "enforcement": l.enforcement.label(),
                "verdict": r.judgement.verdict().as_str(),
                "reason": r.judgement.reason().as_str(),
                "secs": r.secs,
                "est_secs": l.reservation_secs(),
                "exit_code": r.exit_code,
                // THE COVARIATE. Recorded on every row, gating nothing —
                // what replaced the `host-quiet` precondition on 2026-09-08
                // (ARCH §18.2). `null` where the platform reports no load
                // average; a zero there would read as an idle host.
                "load_start": r.before.load,
                "load_end": r.after.load,
                // A row whose START uptime exceeds its END uptime ran across
                // a daemon restart. That row is not slow, it is interrupted —
                // the models were evicted and re-loaded under it — and
                // without this pair it is indistinguishable from a contended
                // one.
                "daemon_uptime_secs_start": r.before.daemon_uptime_secs,
                "daemon_uptime_secs_end": r.after.daemon_uptime_secs,
                // WHICH NODE PRODUCED THIS ROW (cw-lift 5e). `null` where no
                // actor took part — a locally spawned lane, or a distributed
                // unit the cohort never placed. The share the
                // `cw-work-ci-offload` bar scores counts rows where this is
                // non-null AND differs from the document's `submitted_by`,
                // over EVERY row: a shard that was dropped stays in the
                // denominator, so silently losing one costs the share rather
                // than flattering it.
                "node": r.node,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": "quality-check/v1",
        "stamp": stamp,
        "trigger": venue,
        // See the parameter docs: `null` on a local run is "nobody submitted
        // this", not "we could not tell".
        "submitted_by": submitted_by,
        "fingerprint": {
            "hex": fp.hex,
            "primary": fp.primary,
            "fast": fp.fast,
            "embed": fp.embed,
            "smoke_subsets": fp.smoke_subsets,
            "banks": fp.banks,
        },
        "budget_secs": budget_secs,
        "total_secs": total_secs,
        "lanes_with_comparable_baseline": comparable,
        "lanes": lane_rows,
    });
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&doc)?))
}

/// `YYYYmmdd-HHMMSS` in local time — the run directory a human names when
/// asking a colleague to look at a table.
pub(super) fn stamp_now() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::super::exec::Covariates;
    use super::*;
    use kernel_types::quality::Registry;
    use kernel_types::{Judgement, Reason};

    fn fp() -> Fingerprint {
        Fingerprint {
            hex: "deadbeef".into(),
            primary: "stem-a".into(),
            fast: "stem-b".into(),
            embed: "stem-c".into(),
            smoke_subsets: Vec::new(),
            banks: BTreeMap::new(),
        }
    }

    /// **Load lands on EVERY row, including the ones that never ran.**
    ///
    /// This is the whole of what replaced `host-quiet` (ARCH §18.2), and the
    /// row it is easiest to forget is the one that abstained — which is
    /// exactly the row a reader is looking at when they want to know what
    /// the machine was doing. The failing input this test names: a row built
    /// by `InstrumentRun::did_not_start`, the constructor every non-running
    /// path goes through.
    #[test]
    fn a_row_that_never_ran_still_carries_the_load_and_the_daemon_uptime() {
        let reg = Registry::parse(super::super::tests::TABLE).expect("parses");
        let lane = reg
            .instruments
            .iter()
            .find(|i| i.id == "chat-ask")
            .expect("the fixture lane");
        let mut results = BTreeMap::new();
        results.insert(
            lane.id.clone(),
            InstrumentRun::did_not_start(
                Judgement::could_not_judge(
                    lane.id.clone(),
                    Reason::literal("precondition unmet: nothing is listening"),
                ),
                0,
                Covariates {
                    load: Some(31.5),
                    daemon_uptime_secs: Some(12),
                },
            ),
        );
        let dir = std::env::temp_dir().join(format!("qc-summary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("summary.json");
        write_summary(
            &path,
            "20260908-000000",
            "check",
            &fp(),
            &[lane],
            &results,
            0,
            1800,
            0,
            None,
        )
        .expect("writes");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("reads")).expect("json");
        let row = &doc["lanes"][0];
        assert_eq!(row["verdict"], "could-not-judge");
        assert_eq!(row["load_start"], 31.5, "{row}");
        assert_eq!(row["load_end"], 31.5, "{row}");
        assert_eq!(row["daemon_uptime_secs_start"], 12, "{row}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host that reports no load average writes `null`, never `0.0`.
    /// A zero there reads as an idle machine, which is the flattering
    /// direction and the one §18.3 forbids.
    #[test]
    fn an_unreadable_covariate_is_null_and_not_a_zero() {
        let reg = Registry::parse(super::super::tests::TABLE).expect("parses");
        let lane = reg
            .instruments
            .iter()
            .find(|i| i.id == "docs-gate")
            .expect("the fixture gate");
        let mut results = BTreeMap::new();
        results.insert(
            lane.id.clone(),
            InstrumentRun::did_not_start(
                Judgement::never_ran(lane.id.clone(), Reason::literal("cannot run")),
                0,
                Covariates::default(),
            ),
        );
        let dir = std::env::temp_dir().join(format!("qc-summary-null-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("summary.json");
        write_summary(&path, "s", "prepush", &fp(), &[lane], &results, 0, 60, 0, None)
            .expect("writes");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("reads")).expect("json");
        let row = &doc["lanes"][0];
        assert!(row["load_start"].is_null(), "{row}");
        assert!(row["daemon_uptime_secs_end"].is_null(), "{row}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
