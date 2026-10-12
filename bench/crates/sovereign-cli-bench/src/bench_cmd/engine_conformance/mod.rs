// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench engine-conformance`: the engine conformance battery.
//!
//! The rows, checks and predicted verdicts live in
//! `bench/lanes/engine-swap/conformance.toml`, and the method in
//! `PREREG_CONFORMANCE_20261010.md` beside it. `run` replays the case bank
//! over one target and writes [`record::CaseRecord`] lines. `judge` reads them
//! and the inventory, and prints one verdict per row and target pair beside
//! the prediction.
//!
//! Exit codes for `judge`: 0 when every judged verdict is the one predicted,
//! 3 when one is not (to be investigated before any verdict is reported), 4
//! when nothing could be judged, 2 on a usage or input error.

mod case;
mod check;
mod inventory;
mod mutate;
mod project;
mod record;
mod run;
mod server;
mod verdict;

#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod scenario_tests;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use sovereign_cli_base::help::{self, Help, HelpSection};

use inventory::Inventory;
use record::CaseRecord;
use verdict::RowVerdict;

const HELP: Help = Help {
    command: "svrn bench engine-conformance",
    summary: "Judge engine conformance records against the pre-registered inventory.",
    sections: &[
        HelpSection::Usage(
            "svrn bench engine-conformance run --target <label> --daemon <url> [--server <url>] --cases <jsonl> --out <jsonl> [--only <case-id>]...",
        ),
        HelpSection::Usage(
            "svrn bench engine-conformance judge --records <jsonl>... [--inventory <toml>] [--scenario <jsonl>] [--out <dir>] [--mutate]",
        ),
        HelpSection::Subcommands(&[
            (
                "run",
                "Replay the case bank on one target through its daemon's /internal/engine/replay (the operator listener), asking the llama-server at --server what it did for a remote target. Writes one record per case; a case the driver cannot run yet is skipped by name. Exit 0 ran, 1 nothing replayed.",
            ),
            (
                "judge",
                "Apply each row's checks to the records, one verdict per row and target pair, beside the prediction. A target run twice is judged only where its two runs agree (A/A). A row's scenario check takes its verdict from --scenario (one {row, verdict, detail} line per row, as bench/lanes/engine-swap/llg-parity writes), and stays could-not-judge for a row the file does not name. Exit 0 all as predicted, 3 a verdict differs from its prediction, 4 nothing judged. With --mutate, instead perturb one passing cell per check and judge it again: exit 0 every mutation caught, 3 one was missed.",
            ),
        ]),
    ],
};

const DEFAULT_INVENTORY: &str = "bench/lanes/engine-swap/conformance.toml";

/// Entry point.
pub async fn cmd_engine_conformance(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("run") => run::cmd_run(&args[1..]).await,
        Some("judge") => cmd_judge(&args[1..]),
        _ => {
            help::print(&HELP);
            2
        }
    }
}

fn cmd_judge(args: &[String]) -> i32 {
    let mut inventory_path = PathBuf::from(DEFAULT_INVENTORY);
    let mut records_paths = Vec::new();
    let mut scenario_path = None;
    let mut out = None;
    let mut mutate = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--mutate" {
            mutate = true;
            continue;
        }
        match (arg.as_str(), it.next()) {
            ("--inventory", Some(v)) => inventory_path = v.into(),
            ("--records", Some(v)) => records_paths.push(PathBuf::from(v)),
            ("--scenario", Some(v)) => scenario_path = Some(PathBuf::from(v)),
            ("--out", Some(v)) => out = Some(PathBuf::from(v)),
            _ => {
                eprintln!("error: unexpected argument `{arg}`");
                help::print(&HELP);
                return 2;
            }
        }
    }
    let inventory = match Inventory::load(&inventory_path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let mut records = Vec::new();
    for path in &records_paths {
        match read_jsonl::<CaseRecord>(path) {
            Ok(mut r) => records.append(&mut r),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
    }
    let scenarios = match scenario_path.as_deref().map(read_scenarios).transpose() {
        Ok(s) => s.unwrap_or_default(),
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    tracing::debug!(
        rows = inventory.rows.len(),
        records = records.len(),
        scenarios = scenarios.len(),
        "engine_conformance: judging"
    );
    if mutate {
        return cmd_mutate(&inventory, &records, out.as_deref());
    }
    let verdicts = verdict::judge(&inventory, &records, &scenarios);
    if let Some(dir) = out {
        if let Err(e) = write_verdicts(&dir, &verdicts) {
            eprintln!("error: {e}");
            return 2;
        }
    }
    print_table(&verdicts);
    exit_code(&verdicts)
}

/// `judge --mutate`: watch every check fail on a perturbed copy of a cell
/// that passed.
fn cmd_mutate(inventory: &Inventory, records: &[CaseRecord], out: Option<&Path>) -> i32 {
    let mutations = mutate::run(inventory, records);
    if let Some(dir) = out {
        let body: Result<String, _> = mutations
            .iter()
            .map(|m| serde_json::to_string(m).map(|s| s + "\n"))
            .collect();
        let written = body.map_err(|e| e.to_string()).and_then(|b| {
            std::fs::create_dir_all(dir)
                .and_then(|_| std::fs::write(dir.join("mutations.jsonl"), b))
                .map_err(|e| e.to_string())
        });
        if let Err(e) = written {
            eprintln!("error: writing mutations: {e}");
            return 2;
        }
    }
    for m in &mutations {
        let mark = match m.verdict {
            "caught" => "  ",
            "missed" => "!!",
            _ => "··",
        };
        println!(
            "{mark} {:<40} {:<10} {:<14} {}",
            m.row,
            m.verdict,
            m.case.as_deref().unwrap_or("-"),
            m.detail
        );
    }
    let count = |v: &str| mutations.iter().filter(|m| m.verdict == v).count();
    println!(
        "\ncaught {}  missed {}  no-subject {}",
        count("caught"),
        count("missed"),
        count("no-subject")
    );
    if count("missed") > 0 {
        3
    } else {
        0
    }
}

/// One line of a `--scenario` file.
#[derive(serde::Deserialize)]
struct ScenarioLine {
    row: String,
    verdict: String,
    #[serde(default)]
    detail: String,
}

/// Read a `--scenario` file into a verdict per row. A verdict outside
/// passed, failed and could-not-judge, or a row named twice, is refused.
fn read_scenarios(path: &Path) -> Result<verdict::Scenarios, String> {
    let mut scenarios = verdict::Scenarios::new();
    for line in read_jsonl::<ScenarioLine>(path)? {
        let detail = format!("scenario: {}", line.detail);
        let cell = match line.verdict.as_str() {
            "passed" => check::CellVerdict::Passed,
            "failed" => check::CellVerdict::Failed {
                cause: check::FailCause::Differs,
                detail,
            },
            "could-not-judge" => check::CellVerdict::CouldNotJudge(detail),
            other => {
                return Err(format!(
                    "{}: row {}: verdict `{other}` is not passed, failed or could-not-judge",
                    path.display(),
                    line.row
                ))
            }
        };
        tracing::debug!(target: "engine_conformance", row = %line.row, verdict = %line.verdict, "scenario verdict read");
        if scenarios.insert(line.row.clone(), cell).is_some() {
            return Err(format!(
                "{}: row {} appears twice",
                path.display(),
                line.row
            ));
        }
    }
    Ok(scenarios)
}

/// Read a JSONL file of `T`, one per non-blank line, naming the line that
/// does not parse.
fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| {
            serde_json::from_str(l).map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))
        })
        .collect()
}

fn write_verdicts(dir: &Path, verdicts: &[RowVerdict]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join("verdicts.jsonl");
    let body: String = verdicts
        .iter()
        .map(|v| serde_json::to_string(v).map(|s| s + "\n"))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
    tracing::info!(path = %path.display(), "engine_conformance: verdicts written");
    Ok(())
}

fn print_table(verdicts: &[RowVerdict]) {
    for v in verdicts {
        let mark = match v.as_predicted {
            Some(true) => "  ",
            Some(false) => "!!",
            None => "··",
        };
        println!(
            "{mark} {:<40} {:<14} {:<16} {:<8} predicted {:<13} {}/{} failed  {}",
            v.row,
            v.target,
            v.verdict,
            v.cause.unwrap_or(""),
            v.predicted,
            v.failed,
            v.cases,
            v.detail.as_deref().unwrap_or("")
        );
    }
    let count = |s: &str| verdicts.iter().filter(|v| v.verdict == s).count();
    let off = verdicts
        .iter()
        .filter(|v| v.as_predicted == Some(false))
        .count();
    println!(
        "\npassed {}  failed {}  could-not-judge {}  never-ran {}  ·  not as predicted {off}",
        count("passed"),
        count("failed"),
        count("could-not-judge"),
        count("never-ran")
    );
}

fn exit_code(verdicts: &[RowVerdict]) -> i32 {
    if verdicts.iter().any(|v| v.as_predicted == Some(false)) {
        3
    } else if verdicts.iter().all(|v| v.as_predicted.is_none()) {
        4
    } else {
        0
    }
}
