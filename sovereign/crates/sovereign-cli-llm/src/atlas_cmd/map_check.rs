// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas map-check <corpus>` — the preflight: which navigation rows can
//! fire on this corpus at all (map-conversion rung 4, 2026-09-08).
//!
//! Rows × inventory, one verdict per row through the walk's own decider
//! (`KindSet::fit`): `fits`, `inert:seeds`, `inert:edges`. A corpus with no
//! map on disk is said so (`no-map`), with what the walk falls back to — the
//! pipeline's declared map from its config, or the pre-registered table —
//! and the command that converts it. Needs no daemon and opens no store
//! except a wiki-class one, whose census lives nowhere else.
//!
//! `svrn atlas kind` answers "which row would this QUESTION run"; this
//! answers "which rows can this CORPUS run", before any question is asked.

use corpus_engine::enrichment::atlas::{read_atlas_ontology, ATLAS_DIRNAME};

use crate::enrich_cmd::paths;

use super::kind::{inventory_for, policy_for};

fn usage() -> i32 {
    eprintln!("usage: svrn atlas map-check <corpus> [--json]");
    eprintln!("  rows of the corpus's navigation map × what its atlases carry: fits / inert:seeds / inert:edges,");
    eprintln!("  and no-map when nothing on disk declares the rows (with what the walk falls back to).");
    2
}

#[derive(serde::Serialize)]
struct RowVerdict {
    kind: &'static str,
    on: bool,
    verdict: &'static str,
    missing: String,
}

#[derive(serde::Serialize)]
struct Report {
    corpus: String,
    map: String,
    map_on_disk: bool,
    atlases: usize,
    rows: Vec<RowVerdict>,
}

pub async fn run(args: &[String]) -> i32 {
    let mut corpus: Option<String> = None;
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "-h" | "--help" => return usage(),
            flag if flag.starts_with('-') => {
                eprintln!("atlas map-check: unknown flag {flag}");
                return usage();
            }
            c => corpus = Some(c.to_string()),
        }
    }
    let Some(corpus) = corpus else {
        return usage();
    };

    let (policy, map) = policy_for(Some(&corpus), None);
    let map_on_disk =
        read_atlas_ontology(&paths::index_root(&corpus).join(ATLAS_DIRNAME)).is_some();
    let (inventory, atlases) = inventory_for(&corpus);

    let rows: Vec<RowVerdict> = policy
        .rows()
        .map(|(kind, row)| {
            let fit = inventory.fit(row);
            let missing = match &fit {
                corpus_engine::enrichment::atlas::RowFit::Fits => String::new(),
                corpus_engine::enrichment::atlas::RowFit::Inert(i) => i.clause(),
            };
            RowVerdict {
                kind: kind.as_str(),
                on: !row.exemplars.is_empty(),
                verdict: fit.verdict(),
                missing,
            }
        })
        .collect();

    if json {
        let report = Report {
            corpus: corpus.clone(),
            map: map.clone(),
            map_on_disk,
            atlases,
            rows,
        };
        match serde_json::to_string_pretty(&report) {
            Ok(t) => println!("{t}"),
            Err(e) => {
                eprintln!("atlas map-check: {e}");
                return 1;
            }
        }
        return 0;
    }

    println!("corpus:  {corpus}");
    println!("map:     {map}");
    if !map_on_disk {
        println!(
            "         no-map: nothing under {}/atlas declares the rows — `svrn atlas migrate-all {corpus}` writes the pipeline's map",
            paths::index_root(&corpus).display()
        );
    }
    if atlases == 0 || inventory.is_empty() {
        println!("census:  none — no atlas census could be read for `{corpus}`; rows cannot be judged");
    } else {
        println!(
            "census:  {atlases} atlas(es); atoms {}; edges {}",
            inventory
                .atoms
                .iter()
                .map(|(k, n)| format!("{}:{n}", k.label()))
                .collect::<Vec<_>>()
                .join(" "),
            inventory
                .edges
                .iter()
                .map(|(k, n)| format!("{}:{n}", k.label()))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    println!();
    println!("{:<12} {:<4} {:<12} {}", "row", "on", "verdict", "missing");
    for r in &rows {
        println!(
            "{:<12} {:<4} {:<12} {}",
            r.kind,
            if r.on { "yes" } else { "off" },
            r.verdict,
            r.missing
        );
    }
    let fits = rows.iter().filter(|r| r.on && r.verdict == "fits").count();
    let on = rows.iter().filter(|r| r.on).count();
    println!();
    println!("map-check: {fits}/{on} rows on this map can fire on `{corpus}`");
    0
}
