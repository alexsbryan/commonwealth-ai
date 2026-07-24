// SPDX-License-Identifier: AGPL-3.0-or-later
//! Micro-benchmark: per-call latency of the columnar wiki graph's
//! point lookups (record / neighbors wide / neighbors narrow), to
//! attribute the 1-2.2s PPR walk observed 2026-07-17.
//!
//!   cargo run -p corpus-engine --example bench_wiki_graph -- \
//!     ~/.sovereign/indexes/wikipedia/atlas

use corpus_engine::ColumnarWikipediaGraph;
use std::path::PathBuf;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: bench_wiki_graph <atlas-dir>");
    let g = ColumnarWikipediaGraph::open(&dir)
        .await
        .map_err(|e| format!("open: {e}"))?;

    let seeds = [
        "Manhattan Project",
        "World War II",
        "Industrial Revolution",
        "Niels Bohr",
        "Buddhism",
    ];
    // Warm
    let _ = g.record("Manhattan Project").await;

    let t = Instant::now();
    for s in &seeds {
        let _ = g.record(s).await;
    }
    println!(
        "record x5:          {:>8.1?}  ({:.0?}/call)",
        t.elapsed(),
        t.elapsed() / 5
    );

    let t = Instant::now();
    for s in &seeds {
        let n = g.neighbors(s, 512).await;
        assert!(!n.is_empty(), "no neighbors for {s}");
    }
    println!(
        "neighbors(512) x5:  {:>8.1?}  ({:.0?}/call)",
        t.elapsed(),
        t.elapsed() / 5
    );

    let t = Instant::now();
    for s in &seeds {
        let _ = g.neighbors(s, 24).await;
    }
    println!(
        "neighbors(24) x5:   {:>8.1?}  ({:.0?}/call)",
        t.elapsed(),
        t.elapsed() / 5
    );
    Ok(())
}
