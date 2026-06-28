// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign atlas build-archive` — build the zero-copy `atoms.rkyv`
//! archive for one corpus (or `--all` installed corpora) **off the query
//! thread**, so the first query mmaps it instead of paying the
//! convert-on-load parse of `atoms.json`. See `docs/specs/ATLAS_STORAGE.md`
//! Phase 1.5. This is the lifecycle escape hatch for corpora that already
//! shipped/built their `atoms.json` before the archive existed (the build
//! path's `write_atlas_full` sidecar and the post-install hook cover new
//! and freshly-installed corpora).

use std::path::PathBuf;
use std::time::Instant;

use corpus_engine::enrichment::atlas::archive::{archive_needs_build, build_and_write_archive};

pub async fn run(args: &[String]) -> i32 {
    let mut all = false;
    let mut force = false;
    let mut corpus: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--all" => all = true,
            "--force" => force = true,
            "-h" | "--help" => {
                print_help();
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag: {other}");
                return 1;
            }
            other => corpus = Some(other.to_string()),
        }
    }
    if !all && corpus.is_none() {
        eprintln!("usage: sovereign atlas build-archive <corpus_id> | --all [--force]");
        return 1;
    }

    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
        .join("indexes");

    let targets: Vec<String> = if let Some(c) = corpus {
        vec![c]
    } else {
        match std::fs::read_dir(&indexes_dir) {
            Ok(rd) => rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.path().file_name().and_then(|n| n.to_str()).map(String::from))
                .filter(|n| !n.starts_with('.') && !n.starts_with('_'))
                .collect(),
            Err(e) => {
                eprintln!("error: read_dir {}: {e}", indexes_dir.display());
                return 1;
            }
        }
    };

    // A single named corpus is an explicit request — always (re)build it
    // (this is how a pre-existing corpus migrates a stale-version archive,
    // which the mtime-only freshness check can't detect). `--all` respects
    // freshness so it doesn't needlessly rebuild every corpus.
    println!("{:<40} {:>9} {:>8}  {}", "corpus", "ms", "MB", "status");
    println!("{}", "─".repeat(72));
    let mut built = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;
    for corpus_id in targets {
        let atlas_dir = indexes_dir.join(&corpus_id).join("atlas");
        if !atlas_dir.join("atoms.json").exists() {
            if !all {
                println!("{corpus_id:<40} {:>9} {:>8}  no atoms.json", "-", "-");
            }
            continue;
        }
        let build_it = force || !all || archive_needs_build(&atlas_dir);
        if !build_it {
            println!("{corpus_id:<40} {:>9} {:>8}  current (--force to rebuild)", "-", "-");
            skipped += 1;
            continue;
        }
        let t0 = Instant::now();
        match build_and_write_archive(&atlas_dir, &corpus_id) {
            Ok(p) => {
                let mb = std::fs::metadata(&p).map(|m| m.len() / (1 << 20)).unwrap_or(0);
                println!(
                    "{corpus_id:<40} {:>9} {:>8}  built",
                    t0.elapsed().as_millis(),
                    mb
                );
                built += 1;
            }
            Err(e) => {
                println!("{corpus_id:<40} {:>9} {:>8}  error: {e}", "-", "-");
                errors += 1;
            }
        }
    }
    println!("\n{built} built, {skipped} current, {errors} error(s)");
    i32::from(errors > 0)
}

fn print_help() {
    println!("sovereign atlas build-archive — build atoms.rkyv off the query thread\n");
    println!("Usage:");
    println!("  sovereign atlas build-archive <corpus_id>   build one corpus (always rebuilds)");
    println!("  sovereign atlas build-archive --all         build all installed corpora (stale only)");
    println!("  sovereign atlas build-archive --all --force rebuild every corpus's archive");
}
