// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas migrate-all` — the reusable ATLAS_STORAGE_V2 full-port.
//!
//! One idempotent command that migrates EVERY atlas-bearing corpus on this
//! machine to the v2 system, so the port is repeatable on any dev machine with
//! a single invocation:
//!
//!   - **atom corpora** -> v2 store (`atoms.lance` + `edges.csr`, the direct-read
//!     reader) + `atoms_ann.lance` (ANN seeding) for those that already carry an
//!     embeddings cache + the per-corpus `atlas/.read_v2` flip (so the daemon /
//!     desktop read v2 instead of the rkyv archive).
//!   - **wiki-class corpora** (those with a SQLite link graph) -> columnar
//!     `articles.lance` + `edges.lance` (the structural wiki end-state). Wiki has
//!     no atom embeddings, so it gets NO atoms.lance / ANN / read_v2 — its reader
//!     is `ColumnarWikipediaGraph`, picked by `open_wikipedia_graph` when present.
//!
//! Idempotent: skips a store/columnar already current vs its source, and an ANN
//! table that already exists for an unchanged store. Re-runnable and safe to
//! interrupt. The ANN step uses the PRODUCTION grounding filter
//! (`AtlasContextFilter::default()`, env-aware) so the table matches exactly what
//! the daemon seeds from; it only touches corpora that already have an embeddings
//! cache (it never bulk-embeds the resident set). The `read_v2` flip is reversible
//! (delete the marker) and `load_from_disk` falls back to rkyv if a store is
//! absent/unreadable, so a flip can never strand an atlas.

use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::ann_store::ann_table_present;
use corpus_engine::enrichment::atlas::store::{build_and_write_store, store_needs_build};
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use corpus_engine::WikipediaGraph;

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use crate::eval_cmd::runner::{self, AtlasLoadFilter};
use sovereign_core::atlas_context::build_persistent_ann_seed_table;

pub async fn run(args: &[String]) -> i32 {
    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("atlas migrate-all: {e}");
            return 2;
        }
    };
    let mut flip = true; // the migration flips read_v2 by default
    let mut skip_wiki = false;
    let mut only: Option<String> = None;
    for a in &rest {
        match a.as_str() {
            "--flip" => flip = true,
            "--no-flip" => flip = false,
            "--skip-wiki" => skip_wiki = true,
            "-h" | "--help" => {
                print_help();
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
            other => only = Some(other.to_string()),
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

    let corpora: Vec<String> = if let Some(c) = only {
        vec![c]
    } else {
        match std::fs::read_dir(&indexes_dir) {
            Ok(rd) => {
                let mut v: Vec<String> = rd
                    .flatten()
                    .filter(|e| e.path().join(ATLAS_DIRNAME).join("atoms.json").exists())
                    .filter_map(|e| {
                        e.path()
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(String::from)
                    })
                    .filter(|n| !n.starts_with('.') && !n.starts_with('_'))
                    .collect();
                v.sort();
                v
            }
            Err(e) => {
                eprintln!("atlas migrate-all: read_dir {}: {e}", indexes_dir.display());
                return 1;
            }
        }
    };
    if corpora.is_empty() {
        eprintln!(
            "atlas migrate-all: no atlas-bearing corpora under {}",
            indexes_dir.display()
        );
        return 0;
    }
    eprintln!(
        "atlas migrate-all: {} atlas-bearing corpora; flip={flip} skip_wiki={skip_wiki}",
        corpora.len()
    );

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atlas migrate-all: build session: {e}");
            return 1;
        }
    };
    // PRODUCTION grounding filter — the ANN table must cover exactly the atom
    // universe the manager seeds from (its `AtlasContextFilter::default()`).
    let prod = sovereign_tools::atlas_context_manager::AtlasContextFilter::default();
    let filter = AtlasLoadFilter {
        min_description_chars: prod.min_description_chars,
        depth_allowlist: prod.depth_allowlist.clone(),
        max_entries: prod.max_entries,
        include_claims: prod.include_claims,
        include_tensions: prod.include_tensions,
        include_configurations: prod.include_configurations,
    };

    println!(
        "{:<46} {:>7} {:>8} {:>5}  track",
        "corpus", "store", "ann", "flip"
    );
    println!("{}", "-".repeat(82));
    let (mut stores, mut anns, mut flips, mut wikis, mut errs) =
        (0usize, 0usize, 0usize, 0usize, 0usize);

    for corpus_id in &corpora {
        let atlas_dir = indexes_dir.join(corpus_id).join(ATLAS_DIRNAME);

        // Wiki-class: a SQLite link graph means the columnar end-state, not atoms.
        let wiki_db = WikipediaGraph::default_db_path(&indexes_dir, corpus_id);
        if wiki_db.exists() {
            if skip_wiki {
                println!(
                    "{corpus_id:<46} {:>7} {:>8} {:>5}  wiki (skipped)",
                    "-", "-", "-"
                );
                continue;
            }
            let columnar = atlas_dir.join("articles.lance");
            if columnar.exists() && newer_than(&columnar, &wiki_db) {
                println!(
                    "{corpus_id:<46} {:>7} {:>8} {:>5}  wiki (current)",
                    "-", "-", "-"
                );
                continue;
            }
            match WikipediaGraph::open(&wiki_db, corpus_id) {
                Ok(g) => match g.export_columnar(&atlas_dir).await {
                    Ok(()) => {
                        wikis += 1;
                        println!(
                            "{corpus_id:<46} {:>7} {:>8} {:>5}  wiki columnar",
                            "-", "-", "-"
                        );
                    }
                    Err(e) => {
                        errs += 1;
                        println!("{corpus_id:<46}  ERROR wiki export: {e}");
                    }
                },
                Err(e) => {
                    errs += 1;
                    println!("{corpus_id:<46}  ERROR wiki open: {e}");
                }
            }
            continue;
        }

        // Atom track. 1) store (idempotent).
        let mut store_built = false;
        let store_state = if store_needs_build(&atlas_dir) {
            match build_and_write_store(&atlas_dir, corpus_id).await {
                Ok(_) => {
                    stores += 1;
                    store_built = true;
                    "built"
                }
                Err(e) => {
                    errs += 1;
                    println!("{corpus_id:<46}  ERROR store: {e}");
                    continue;
                }
            }
        } else {
            "current"
        };

        // 2) ANN — scope to embedding-bearing corpora (never bulk-embed the
        // structural set). `atoms.embeddings.bin` is no longer written
        // (ATLAS_STORAGE_V2 Phase B retired the embed cache), but the file
        // persists on disk as the legacy "this corpus was embedded" marker, and
        // an existing ANN table is the forward-looking equivalent — either signal
        // admits the corpus. Builds the stragglers (embedded but no table yet)
        // and leaves current tables as-is; fresh corpora get their table via
        // `svrn atlas backfill-ann`.
        let ann_state: &str = if !ann_table_present(&atlas_dir)
            && !atlas_dir.join("atoms.embeddings.bin").exists()
        {
            "n/a"
        } else if ann_table_present(&atlas_dir) && !store_built {
            "current"
        } else {
            // Load the seedable atoms. The production grounding filter
            // (min_description_chars=200, entities) matches the manager's seeding
            // universe, but it silently empties short-description prose corpora
            // (enron-class). Fall back ONCE to a relaxed description floor so those
            // still seed one-shot; a corpus that's STILL empty carries only
            // non-Entity surfaces (investigation graphs) and is genuinely
            // unseedable — skip it without an error rather than reporting failure.
            let strict = runner::load_atlas_context(&session, corpus_id, prod.top_k, &filter).await;
            let ctx = match strict {
                Ok(ctx) if !ctx.entries.is_empty() => Some(ctx),
                _ => {
                    let relaxed = AtlasLoadFilter {
                        min_description_chars: 1,
                        depth_allowlist: filter.depth_allowlist.clone(),
                        max_entries: filter.max_entries,
                        include_claims: filter.include_claims,
                        include_tensions: filter.include_tensions,
                        include_configurations: filter.include_configurations,
                    };
                    runner::load_atlas_context(&session, corpus_id, prod.top_k, &relaxed)
                        .await
                        .ok()
                        .filter(|c| !c.entries.is_empty())
                }
            };
            match ctx {
                Some(ctx) => match build_persistent_ann_seed_table(&atlas_dir, &ctx).await {
                    Ok(_) => {
                        anns += 1;
                        "built"
                    }
                    Err(e) => {
                        errs += 1;
                        eprintln!("  {corpus_id}: ann build: {e}");
                        "err"
                    }
                },
                // No seedable atoms even at the relaxed floor (non-Entity surfaces).
                None => "none",
            }
        };

        // 3) flip read_v2 (reversible; rkyv stays the fallback).
        let flip_state: &str = if !flip {
            "-"
        } else {
            let marker = atlas_dir.join(".read_v2");
            if marker.exists() {
                "on"
            } else {
                match std::fs::File::create(&marker) {
                    Ok(_) => {
                        flips += 1;
                        "set"
                    }
                    Err(e) => {
                        errs += 1;
                        eprintln!("  {corpus_id}: flip: {e}");
                        "err"
                    }
                }
            }
        };

        println!("{corpus_id:<46} {store_state:>7} {ann_state:>8} {flip_state:>5}  atom");
    }

    println!(
        "\nmigrate-all: {stores} stores built, {anns} ANN tables, {flips} flipped, \
         {wikis} wiki columnar, {errs} errors (over {} corpora)",
        corpora.len()
    );
    i32::from(errs > 0)
}

/// True if `a`'s mtime is at least `b`'s — the cheap "already current" gate.
fn newer_than(a: &Path, b: &Path) -> bool {
    let mt = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mt(a), mt(b)) {
        (Some(x), Some(y)) => x >= y,
        _ => false,
    }
}

fn print_help() {
    println!("svrn atlas migrate-all — reusable ATLAS_STORAGE_V2 full-port (idempotent)\n");
    println!("  sovereign atlas migrate-all                 migrate every atlas-bearing corpus + flip read_v2");
    println!(
        "  sovereign atlas migrate-all --no-flip       build v2 artifacts but do NOT flip read_v2"
    );
    println!("  sovereign atlas migrate-all --skip-wiki     skip wiki-class (columnar) corpora");
    println!("  sovereign atlas migrate-all <corpus_id>     migrate one corpus");
    println!(
        "\natom corpora -> atoms.lance + edges.csr (+ atoms_ann.lance if embedded) + .read_v2"
    );
    println!("wiki-class   -> articles.lance + edges.lance (columnar; no atoms.lance/ANN/read_v2)");
}
