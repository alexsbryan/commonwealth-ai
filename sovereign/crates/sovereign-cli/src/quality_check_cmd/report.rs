// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the runner PRINTS: one instrument's outcome, the dry-run selection,
//! and the durable `summary.json`.

use std::collections::BTreeMap;
use std::path::Path;

use kernel_types::quality::{Instrument, Trigger};

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
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": "quality-check/v1",
        "stamp": stamp,
        "trigger": venue,
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
