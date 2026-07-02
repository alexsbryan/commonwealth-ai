// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas build-doc-index` — Move 6 Phase 1 backfill.
//!
//! Derives the per-corpus `doc_to_atoms.json` sidecar from each
//! atlas's `atoms.json`. Idempotent: re-running on a corpus that
//! already has a sidecar rewrites it with the current atoms set
//! (so the migration also works as a refresh after manual edits).

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::doc_to_atoms;

pub async fn run(args: &[String]) -> i32 {
    let mut filter: Option<String> = None;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--corpus" => match iter.next() {
                Some(v) => filter = Some(v.clone()),
                None => {
                    eprintln!("--corpus requires an argument");
                    return 1;
                }
            },
            "--all" => {} // default
            "--help" | "-h" => {
                println!(
                    "svrn atlas build-doc-index [--corpus <id>] [--all]\n\
                    \n\
                    Move 6 P1: derive doc_to_atoms.json sidecar from each\n\
                    installed atlas's atoms.json. Maps source-doc handles\n\
                    to the atoms produced from each doc, used by the\n\
                    atoms-delta primitive (P2) for incremental updates.\n\
                    \n\
                    --corpus <id>   Limit to one corpus by id.\n\
                    --all           Build for every installed atlas (default)."
                );
                return 0;
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 1;
            }
        }
    }

    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
        .join("indexes");

    let entries = match std::fs::read_dir(&indexes_dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("error: read_dir {}: {e}", indexes_dir.display());
            return 1;
        }
    };

    println!("{:<32} {:>8} {:>10}", "corpus", "docs", "atoms");
    println!("{}", "─".repeat(56));

    let mut total_docs = 0usize;
    let mut total_atoms = 0usize;
    let mut errors = 0usize;

    for entry in entries.flatten() {
        let corpus_path = entry.path();
        if !corpus_path.is_dir() {
            continue;
        }
        let corpus_id = match corpus_path.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.starts_with('.') && !n.starts_with('_') => n.to_string(),
            _ => continue,
        };
        if let Some(f) = &filter {
            if corpus_id != *f {
                continue;
            }
        }
        let atlas_dir = corpus_path.join("atlas");
        if !atlas_dir.join("atoms.json").is_file() {
            continue;
        }

        match doc_to_atoms::build_and_write(&atlas_dir) {
            Ok(file) => {
                let atoms_count: usize = file.by_doc.values().map(|v| v.len()).sum();
                println!("{:<32} {:>8} {:>10}", corpus_id, file.len(), atoms_count);
                total_docs += file.len();
                total_atoms += atoms_count;
            }
            Err(e) => {
                eprintln!("  ✗ {}: {e}", corpus_id);
                errors += 1;
            }
        }
    }

    println!();
    println!(
        "Total: {} docs indexed across {} atoms; {} errors",
        total_docs, total_atoms, errors
    );

    if errors > 0 {
        1
    } else {
        0
    }
}
