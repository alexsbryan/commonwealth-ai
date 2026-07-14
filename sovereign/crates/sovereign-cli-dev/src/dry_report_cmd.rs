// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code dry-report [--corpus-id <id>] [--scope <prefix>] [--min-lines N]
//!  [--threshold F]` — semantic-duplication report over the code embeddings.
//!
//! Thin CLI wrapper over `sovereign_tools::code::dry_report`. Resolves the
//! corpus the same way `arch-report` / `suggest-seams` do (explicit `--corpus-id`
//! or the sole indexed code corpus), then reuses the per-symbol embeddings that
//! already live in that corpus's LanceDB index. Read-only.

use std::path::PathBuf;

use sovereign_tools::code::dry_report::{
    build_dry_report, render_dry_report, DryInputs, DEFAULT_MIN_LINES, DEFAULT_NEAR_THRESHOLD,
};

pub(crate) async fn run(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut scope: Option<String> = None;
    let mut min_lines: usize = DEFAULT_MIN_LINES;
    let mut threshold: f32 = DEFAULT_NEAR_THRESHOLD;
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
            "--scope" => {
                i += 1;
                scope = args.get(i).cloned();
                if scope.is_none() {
                    eprintln!("error: --scope requires a value");
                    return 1;
                }
            }
            "--min-lines" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) => min_lines = n,
                    None => {
                        eprintln!("error: --min-lines requires an integer");
                        return 1;
                    }
                }
            }
            "--threshold" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<f32>().ok()) {
                    Some(f) if (0.0..=1.0).contains(&f) => threshold = f,
                    _ => {
                        eprintln!("error: --threshold requires a float in [0.0, 1.0]");
                        return 1;
                    }
                }
            }
            "-h" | "--help" => {
                println!(
                    "svrn code dry-report [--corpus-id <id>] [--scope <path-prefix>] \
                     [--min-lines N] [--threshold F]"
                );
                println!();
                println!("Find repeated code from the per-symbol embeddings already in the");
                println!("corpus index. Two tiers: exact clones (byte-identical, by content");
                println!("hash) and near clones (cosine ≥ threshold). Advisory — a human");
                println!("decides what to factor out.");
                println!();
                println!("  --scope <prefix>   restrict to a file-path prefix (e.g. a crate dir)");
                println!("  --min-lines N      ignore symbols shorter than N lines (default 8)");
                println!("  --threshold F      near-clone cosine cutoff, 0..1 (default 0.95)");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            other => {
                eprintln!("error: unexpected positional argument '{other}'");
                return 1;
            }
        }
        i += 1;
    }

    let indexes_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".sovereign")
        .join("indexes");
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

    let index_path = indexes_dir.join(&corpus_id);
    if !index_path.join("chunks.lance").exists() {
        eprintln!(
            "error: no chunk index at {} — run `svrn project init` first",
            index_path.join("chunks.lance").display()
        );
        return 1;
    }

    match build_dry_report(DryInputs {
        index_path: &index_path,
        corpus_id: &corpus_id,
        min_lines,
        near_threshold: threshold,
        scope: scope.as_deref(),
    })
    .await
    {
        Ok(report) => {
            println!("{}", render_dry_report(&report));
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
