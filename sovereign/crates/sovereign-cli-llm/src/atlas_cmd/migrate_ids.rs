// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas migrate-ids` — Move 6 Phase 0 migration command.
//!
//! Walks installed atlases and rewrites atoms.json + edges.json +
//! cross_corpus_edges.json from sequential ids to content-hash ids.
//! Idempotent. See `corpus-engine/src/enrichment/atlas/migrate_ids.rs`
//! for the migration logic.

use corpus_engine::enrichment::atlas::migrate_ids::{migrate_atlas_ids, MigrationSummary};

pub async fn run(args: &[String]) -> i32 {
    let mut filter: Option<String> = None;
    let mut dry_run = false;
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
            "--dry-run" => dry_run = true,
            "--help" | "-h" => {
                println!(
                    "svrn atlas migrate-ids [--corpus <id>] [--all] [--dry-run]\n\
                    \n\
                    Move 6 P0: rewrite atoms.json + edges.json + cross_corpus_edges.json\n\
                    from sequential entity-NNNN ids to content-hash entity-<hex> ids.\n\
                    Idempotent: re-running on a migrated atlas is a no-op.\n\
                    \n\
                    --corpus <id>   Limit to one corpus by id.\n\
                    --all           Migrate every installed atlas (default).\n\
                    --dry-run       Preview migration without writing files."
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
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
        .join("indexes");

    let entries = match std::fs::read_dir(&indexes_dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("error: read_dir {}: {e}", indexes_dir.display());
            return 1;
        }
    };

    println!(
        "{:<32} {:>8} {:>10} {:>10} {:>10}",
        "corpus", "atoms", "edges", "cc_edges", "status"
    );
    println!("{}", "─".repeat(74));

    let mut total = MigrationSummary::default();
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
        if !atlas_dir.is_dir() {
            continue;
        }
        // Skip atlases whose atoms.json is missing/empty/malformed.
        // Migration on a stub atlas has nothing to do; treating it
        // as a hard error blocks the whole --all pass.
        let atoms_path = atlas_dir.join("atoms.json");
        match std::fs::metadata(&atoms_path) {
            Err(_) => {
                println!(
                    "{:<32} {:>8} {:>10} {:>10} {:>10}",
                    corpus_id, 0, 0, 0, "skip (no atoms.json)"
                );
                continue;
            }
            Ok(m) if m.len() < 8 => {
                println!(
                    "{:<32} {:>8} {:>10} {:>10} {:>10}",
                    corpus_id, 0, 0, 0, "skip (empty atoms.json)"
                );
                continue;
            }
            Ok(_) => {}
        }

        match migrate_atlas_ids(&atlas_dir, &corpus_id, dry_run) {
            Ok(summary) => {
                let status = if summary.atoms_migrated == 0 {
                    if summary.atoms_already_content_hash > 0 {
                        "no-op (migrated)"
                    } else {
                        "no-op (empty)"
                    }
                } else if dry_run {
                    "would-migrate"
                } else {
                    "ok"
                };
                println!(
                    "{:<32} {:>8} {:>10} {:>10} {:>10}",
                    corpus_id,
                    summary.atoms_migrated,
                    summary.edges_rewritten,
                    summary.cross_corpus_edges_rewritten,
                    status,
                );
                if summary.atoms_deduped > 0 {
                    eprintln!(
                        "  ⓘ {} duplicate atoms collapsed on {} ({} pre-dedup hash matches)",
                        summary.atoms_deduped,
                        corpus_id,
                        summary.collisions_detected.len()
                    );
                }
                total.atoms_migrated += summary.atoms_migrated;
                total.atoms_already_content_hash += summary.atoms_already_content_hash;
                total.edges_rewritten += summary.edges_rewritten;
                total.cross_corpus_edges_rewritten += summary.cross_corpus_edges_rewritten;
                total.atoms_deduped += summary.atoms_deduped;
            }
            Err(e) => {
                eprintln!("  ✗ {}: {e}", corpus_id);
                errors += 1;
            }
        }
    }

    println!();
    println!(
        "Total: {} atoms migrated, {} duplicates collapsed, {} already content-hash, {} edges rewritten, {} cross-corpus edges rewritten, {} errors",
        total.atoms_migrated,
        total.atoms_deduped,
        total.atoms_already_content_hash,
        total.edges_rewritten,
        total.cross_corpus_edges_rewritten,
        errors,
    );

    if errors > 0 {
        1
    } else {
        0
    }
}
