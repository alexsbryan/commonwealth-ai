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
    let mut as_goal = false;
    let mut max_lines: usize = 1200;
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
            "--goal" => as_goal = true,
            "--max-lines" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<usize>().ok()) {
                    Some(n) => max_lines = n,
                    None => {
                        eprintln!("error: --max-lines requires a number");
                        return 1;
                    }
                }
            }
            "-h" | "--help" => {
                println!("svrn code suggest-seams <file> [--corpus-id <id>] [--goal] [--max-lines N]");
                println!();
                println!("Propose submodule seams for an oversized file from the SCIP call graph:");
                println!("  proposed modules (per handler), the shared helpers that must stay in");
                println!("  mod.rs, merge candidates, oversized flags, and dead code.");
                println!();
                println!("--goal renders the report as a paste-ready goal for the solve split verb");
                println!("  (`svrn solve <workdir> \"$(svrn code suggest-seams <file> --goal)\" \\");
                println!("     --verb split --max-lines N`) instead of the human-readable report.");
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
    let corpus_id = match crate::converge_cmd::resolve_corpus(corpus_id, &indexes_dir) {
        Ok(c) => c,
        Err(code) => return code,
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
            if as_goal {
                let goal = sovereign_tools::code::suggest_seams::render_split_goal(
                    &report, max_lines,
                );
                // Paste-ready: the caller wraps it in quotes for the solve verb,
                // so strip nothing — newlines are the concern map's structure.
                println!("{goal}");
            } else {
                println!("{}", render_seam_report(&report));
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
