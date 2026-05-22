//! `sovereign atlas enable-incremental` — flip the per-corpus
//! `atlas_incremental_enabled` flag in `_corpus_meta.json`.
//!
//! Lights up the Move 6 P5.b/c post-update hook in
//! `CorpusUpdater::apply_update` for the named corpus (or every
//! installed atlas with `--all`). Pre-flight check: refuses to
//! enable a corpus whose `atoms.json` still carries sequential-id
//! atoms (`sovereign atlas migrate-ids` must run first) since the
//! hook's `apply_atom_delta` would leave legacy atoms orphaned.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::writer::read_atlas_atoms;
use corpus_engine::index::CorpusIndex;

pub async fn run(args: &[String]) -> i32 {
    let mut corpus_filter: Option<String> = None;
    let mut all = false;
    let mut disable = false;
    let mut force = false;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--corpus" => match iter.next() {
                Some(v) => corpus_filter = Some(v.clone()),
                None => {
                    eprintln!("--corpus requires an argument");
                    return 1;
                }
            },
            "--all" => all = true,
            "--disable" => disable = true,
            "--force" => force = true,
            "--help" | "-h" => {
                println!(
                    "sovereign atlas enable-incremental [--corpus <id>] [--all] [--disable] [--force]\n\
                    \n\
                    Move 6 P5.b/c: opt a corpus into the post-update incremental\n\
                    atlas hook. After this flag is set, CorpusUpdater::apply_update\n\
                    will compute per-doc atlas deltas instead of leaving the atlas\n\
                    stale (watched-folder commits, monthly delta-ingest).\n\
                    \n\
                    --corpus <id>   Flip flag for one corpus.\n\
                    --all           Flip flag for every installed atlas.\n\
                    --disable       Set flag to false (default: true).\n\
                    --force         Skip the content-hash pre-flight (dangerous).\n\
                    \n\
                    Pre-flight: refuses corpora whose atoms.json still has\n\
                    sequential-id atoms (run `sovereign atlas migrate-ids` first).\n\
                    Override with --force only if you know what you're doing."
                );
                return 0;
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 1;
            }
        }
    }

    if corpus_filter.is_none() && !all {
        eprintln!("error: pass --corpus <id> or --all");
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

    let entries = match std::fs::read_dir(&indexes_dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("error: read_dir {}: {e}", indexes_dir.display());
            return 1;
        }
    };

    let target_state = !disable;
    let action = if disable { "disabled" } else { "enabled" };

    println!(
        "{:<36} {:>10} {}",
        "corpus", "atoms", "result"
    );
    println!("{}", "─".repeat(72));

    let mut touched = 0usize;
    let mut skipped = 0usize;
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
        if let Some(f) = &corpus_filter {
            if corpus_id != *f {
                continue;
            }
        }

        let atlas_dir = corpus_path.join("atlas");
        let atoms_present = atlas_dir.join("atoms.json").is_file();

        // Pre-flight: when enabling, require content-hash atoms (or
        // --force, or an empty/missing atoms.json which the hook
        // will populate on the first apply).
        if target_state && !force && atoms_present {
            match read_atlas_atoms(&atlas_dir) {
                Ok(file) if !file.atoms.is_empty()
                    && !file.atoms.iter().all(|env| env.id().is_content_hash()) =>
                {
                    println!(
                        "{:<36} {:>10} ✗ sequential-id atoms; run migrate-ids first",
                        corpus_id,
                        file.atoms.len()
                    );
                    skipped += 1;
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("  ✗ {corpus_id}: read atoms.json: {e}");
                    errors += 1;
                    continue;
                }
            }
        }

        let index = match CorpusIndex::open(&corpus_path).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("  ✗ {corpus_id}: open index: {e}");
                errors += 1;
                continue;
            }
        };
        match index.set_atlas_incremental_enabled(target_state) {
            Ok(()) => {
                let atom_count = if atoms_present {
                    read_atlas_atoms(&atlas_dir)
                        .map(|f| f.atoms.len())
                        .unwrap_or(0)
                } else {
                    0
                };
                println!("{:<36} {:>10} ✓ {action}", corpus_id, atom_count);
                touched += 1;
            }
            Err(e) => {
                eprintln!("  ✗ {corpus_id}: set flag: {e}");
                errors += 1;
            }
        }

    }

    println!();
    println!(
        "Result: {touched} corpus(es) {action}; {skipped} skipped (pre-flight); {errors} errors"
    );

    if errors > 0 {
        1
    } else {
        0
    }
}
