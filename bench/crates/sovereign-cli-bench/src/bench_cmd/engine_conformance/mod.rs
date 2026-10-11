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
mod project;
mod record;
mod run;
mod server;
mod verdict;

#[cfg(test)]
mod run_tests;
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
            "svrn bench engine-conformance judge --records <jsonl>... [--inventory <toml>] [--out <dir>]",
        ),
        HelpSection::Subcommands(&[
            (
                "run",
                "Replay the case bank on one target through its daemon's /internal/engine/replay (the operator listener), asking the llama-server at --server what it did for a remote target. Writes one record per case; a case the driver cannot run yet is skipped by name. Exit 0 ran, 1 nothing replayed.",
            ),
            (
                "judge",
                "Apply each row's checks to the records, one verdict per row and target pair, beside the prediction. Exit 0 all as predicted, 3 a verdict differs from its prediction, 4 nothing judged.",
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
    let mut out = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match (arg.as_str(), it.next()) {
            ("--inventory", Some(v)) => inventory_path = v.into(),
            ("--records", Some(v)) => records_paths.push(PathBuf::from(v)),
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
    tracing::debug!(
        rows = inventory.rows.len(),
        records = records.len(),
        "engine_conformance: judging"
    );
    let verdicts = verdict::judge(&inventory, &records);
    if let Some(dir) = out {
        if let Err(e) = write_verdicts(&dir, &verdicts) {
            eprintln!("error: {e}");
            return 2;
        }
    }
    print_table(&verdicts);
    exit_code(&verdicts)
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
