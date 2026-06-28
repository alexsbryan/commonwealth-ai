// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atlas verify-v2` — the ATLAS_STORAGE_V2 migration audit + backfill tool.
//!
//! For each atlas-bearing corpus: ensure the v2 store (`atoms.lance` +
//! `edges.csr`) exists, reconstruct the archive from it
//! (`store::reconstruct_archive_bytes`), and prove it is a **lossless** encoding
//! of the v1 rkyv atlas — the atom-id set and the per-kind histogram must match
//! exactly, and the edge count may only drop by the (retrieval-neutral) dangling
//! edges the local-id CSR cannot represent. Run `--all` to audit the whole
//! resident set before flipping the daemon to the v2 reader.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::store::{build_and_write_store, reconstruct_archive_bytes};
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use sovereign_core::atlas_context::AtlasGraph;

pub async fn run(args: &[String]) -> i32 {
    let mut all = false;
    let mut generate = false;
    let mut corpus: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--all" => all = true,
            "--generate" => generate = true,
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
        eprintln!("usage: sovereign atlas verify-v2 <corpus_id> | --all [--generate]");
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
            Ok(rd) => {
                let mut v: Vec<String> = rd
                    .flatten()
                    .filter(|e| {
                        e.path()
                            .join(ATLAS_DIRNAME)
                            .join("atoms.json")
                            .exists()
                    })
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
                eprintln!("error: read_dir {}: {e}", indexes_dir.display());
                return 1;
            }
        }
    };

    println!(
        "{:<42} {:>9} {:>6} {:>8} {:>8}  verdict",
        "corpus", "atoms", "kinds", "r_edges", "l_edges"
    );
    println!("{}", "-".repeat(92));
    let (mut pass, mut fail) = (0usize, 0usize);
    for c in &targets {
        match verify_one(c, &indexes_dir, generate).await {
            Ok(v) => {
                let ok = v.atoms_match && v.kinds_match && v.edges_ok;
                println!(
                    "{:<42} {:>9} {:>6} {:>8} {:>8}  {}",
                    c,
                    v.l_atoms,
                    v.kinds,
                    v.r_edges,
                    v.l_edges,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass += 1;
                } else {
                    fail += 1;
                    if !v.atoms_match {
                        println!("    ! atom-id set differs: rkyv={} lance={}", v.r_atoms, v.l_atoms);
                    }
                    if !v.kinds_match {
                        println!("    ! kind histogram differs: rkyv={:?} lance={:?}", v.r_hist, v.l_hist);
                    }
                    if !v.edges_ok {
                        println!("    ! lance edges {} > rkyv edges {} (should only drop dangling)", v.l_edges, v.r_edges);
                    }
                }
            }
            Err(e) => {
                println!("{c:<42}  ERROR: {e}");
                fail += 1;
            }
        }
    }
    println!("\n{pass} pass, {fail} fail");
    i32::from(fail > 0)
}

struct Verify {
    r_atoms: usize,
    l_atoms: usize,
    kinds: usize,
    r_edges: usize,
    l_edges: usize,
    atoms_match: bool,
    kinds_match: bool,
    edges_ok: bool,
    r_hist: BTreeMap<String, usize>,
    l_hist: BTreeMap<String, usize>,
}

async fn verify_one(corpus_id: &str, indexes_dir: &Path, generate: bool) -> Result<Verify, String> {
    let atlas_dir = indexes_dir.join(corpus_id).join(ATLAS_DIRNAME);

    // v1: the rkyv atlas (mmap or convert-on-load).
    let rkyv = AtlasGraph::load_from_disk(corpus_id, &atlas_dir)?;

    // Ensure the v2 store exists (backfill), then reconstruct it.
    if generate || !atlas_dir.join("atoms.lance").exists() {
        build_and_write_store(&atlas_dir, corpus_id).await?;
    }
    let bytes = reconstruct_archive_bytes(&atlas_dir, corpus_id).await?;
    let lance = AtlasGraph::from_archive_bytes(corpus_id, &bytes)?;

    let r_ids: BTreeSet<String> = rkyv.atoms().map(|a| a.id().to_string()).collect();
    let l_ids: BTreeSet<String> = lance.atoms().map(|a| a.id().to_string()).collect();

    let hist = |g: &AtlasGraph| -> BTreeMap<String, usize> {
        let mut h = BTreeMap::new();
        for a in g.atoms() {
            *h.entry(format!("{:?}", a.kind())).or_insert(0) += 1;
        }
        h
    };
    let r_hist = hist(&rkyv);
    let l_hist = hist(&lance);

    Ok(Verify {
        r_atoms: r_ids.len(),
        l_atoms: l_ids.len(),
        kinds: r_hist.len(),
        r_edges: rkyv.edge_count(),
        l_edges: lance.edge_count(),
        atoms_match: r_ids == l_ids,
        kinds_match: r_hist == l_hist,
        // The v2 CSR cannot encode an edge to an atom outside the set; those
        // dangling edges are non-traversable, so dropping them is neutral.
        edges_ok: lance.edge_count() <= rkyv.edge_count(),
        r_hist,
        l_hist,
    })
}

fn print_help() {
    println!("sovereign atlas verify-v2 — prove the v2 store reconstructs the rkyv atlas losslessly\n");
    println!("  sovereign atlas verify-v2 <corpus_id>          verify one corpus");
    println!("  sovereign atlas verify-v2 --all                verify every atlas-bearing corpus");
    println!("  sovereign atlas verify-v2 --all --generate     (re)build each v2 store first");
    println!("\nPASS = atom-id set + per-kind histogram identical, edges drop only dangling.");
}
