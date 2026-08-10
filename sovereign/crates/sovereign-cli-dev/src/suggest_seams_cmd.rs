// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code suggest-seams <file>` — advisory god-file split proposals.
//!
//! Thin CLI wrapper over `sovereign_tools::code::suggest_seams`: resolves the
//! corpus + its SCIP graph (same resolution as `arch-report`), normalizes the
//! file path to the repo-relative form SCIP stores, and prints the report.
//! Read-only analysis — a human does the extraction.

use std::path::PathBuf;

use sovereign_tools::code::suggest_seams::{build_seam_report, render_seam_report, SeamInputs};

pub(crate) async fn run(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut file_arg: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                corpus_id = args.get(i).cloned();
                if corpus_id.is_none() {
                    eprintln!("error: --corpus-id requires a value");
                    return 1;
                }
            }
            "-h" | "--help" => {
                println!("svrn code suggest-seams <file> [--corpus-id <id>]");
                println!();
                println!("Propose submodule seams for an oversized file from the SCIP call graph:");
                println!("  proposed modules (per handler), the shared helpers that must stay in");
                println!("  mod.rs, merge candidates, oversized flags, and dead code.");
                println!();
                println!("<file> is repo-relative, exactly as SCIP stores it, e.g.");
                println!("  sovereign/crates/sovereign-cli-dev/src/project_cmd.rs");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => {
                if file_arg.is_none() {
                    file_arg = Some(positional.to_string());
                }
            }
        }
        i += 1;
    }

    let Some(file_arg) = file_arg else {
        eprintln!("error: a file path is required — `svrn code suggest-seams <file>`");
        return 1;
    };

    // Normalize to the repo-relative form SCIP stores. If the arg is absolute
    // and under cwd, strip cwd; otherwise pass through (assume repo-relative).
    let file_path = match std::env::current_dir() {
        Ok(cwd) => {
            let p = PathBuf::from(&file_arg);
            if p.is_absolute() {
                p.strip_prefix(&cwd)
                    .map(|r| r.to_string_lossy().into_owned())
                    .unwrap_or(file_arg.clone())
            } else {
                file_arg.clone()
            }
        }
        Err(_) => file_arg.clone(),
    };

    // Resolve the corpus: explicit, or the sole indexed code corpus.
    let indexes_dir = sovereign_cli_shared::dirs::sovereign_root().join("indexes");
    let corpus_id = match corpus_id {
        Some(c) => c,
        None => {
            let mut corpora: Vec<String> = std::fs::read_dir(&indexes_dir)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().join("scip_graph.db").exists())
                        .filter_map(|e| e.file_name().to_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            corpora.sort();
            match corpora.len() {
                1 => corpora.remove(0),
                0 => {
                    eprintln!(
                        "error: no code corpus under {} — run `svrn project init` first",
                        indexes_dir.display()
                    );
                    return 1;
                }
                _ => {
                    eprintln!(
                        "error: multiple code corpora — pass --corpus-id one of: {}",
                        corpora.join(", ")
                    );
                    return 1;
                }
            }
        }
    };

    let db_path = indexes_dir.join(&corpus_id).join("scip_graph.db");
    if !db_path.exists() {
        eprintln!(
            "error: no SCIP graph at {} — run `svrn project init` first",
            db_path.display()
        );
        return 1;
    }

    match build_seam_report(SeamInputs {
        db_path: &db_path,
        corpus_id: &corpus_id,
        file_path: &file_path,
    })
    .await
    {
        Ok(report) => {
            println!("{}", render_seam_report(&report));
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
