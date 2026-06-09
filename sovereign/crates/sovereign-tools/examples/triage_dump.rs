// SPDX-License-Identifier: AGPL-3.0-or-later
//! Run `build_triage_candidates` against an installed corpus's atlas
//! and print the resulting top-N with tier annotations.
//!
//! Usage: cargo run -p sovereign-tools --example triage_dump -- \
//!     <indexes_dir> <corpus_id> [budget]
//!
//! Default indexes_dir = `~/.sovereign/indexes`. Default budget = 50.
//! Useful for empirically validating the Vital Articles tier prior on
//! a freshly-built structural atlas without spinning up the daemon.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::vital_tier;
use sovereign_tools::atlas_postinstall::{build_triage_candidates, TriageOutcome};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let indexes_dir = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_indexes_dir);
    let corpus_id = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "wiki-l5-struct".to_string());
    let budget: usize = args
        .get(3)
        .map(|s| s.parse().expect("budget must be an integer"))
        .unwrap_or(50);

    println!(
        "indexes_dir = {}\ncorpus_id   = {}\nbudget      = {}\n",
        indexes_dir.display(),
        corpus_id,
        budget
    );

    let outcome = build_triage_candidates(&corpus_id, indexes_dir.clone(), budget).await;
    let path = match outcome {
        TriageOutcome::Built {
            path,
            in_corpus_picked,
            elapsed_secs,
        } => {
            println!(
                "built {} picks in {:.2}s → {}",
                in_corpus_picked,
                elapsed_secs,
                path.display()
            );
            path
        }
        TriageOutcome::NoAtlas => {
            eprintln!(
                "no atlas at {}/{}/atlas/atoms.json",
                indexes_dir.display(),
                corpus_id
            );
            std::process::exit(1);
        }
        TriageOutcome::Failed { reason } => {
            eprintln!("triage failed: {reason}");
            std::process::exit(1);
        }
    };

    let raw = std::fs::read_to_string(&path).expect("read triage output");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("parse triage output");
    println!(
        "\ntier breakdown: L1={} L2={} L3={} L4={} L5={} off-list={}",
        v["tier_breakdown"]["l1"],
        v["tier_breakdown"]["l2"],
        v["tier_breakdown"]["l3"],
        v["tier_breakdown"]["l4"],
        v["tier_breakdown"]["l5"],
        v["tier_breakdown"]["off_list"]
    );
    println!("\ntop {budget} (tier · canonical_name):");
    for (i, name) in v["top_in_corpus_by_centrality"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let s = name.as_str().unwrap();
        let tier = vital_tier(s)
            .map(|t| format!("L{t}"))
            .unwrap_or_else(|| "  -".into());
        println!("  {:>4}. {tier}  {s}", i + 1);
    }
}

fn default_indexes_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".sovereign").join("indexes")
}
