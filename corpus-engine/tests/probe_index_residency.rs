// SPDX-License-Identifier: AGPL-3.0-or-later
//! PROBE B (order `mesh-scale-t0`, `MESH_SCALE_100_USERS_1000_CORPORA.md` §8).
//!
//! One question, one number: **what does an installed corpus cost in
//! resident memory once a background sweep has opened it?** That number
//! exists nowhere in the tree today, which is why §7.2 could only say
//! "measure per-handle memory first" and had to refuse the index-handle
//! LRU on principle rather than on arithmetic.
//!
//! `#[ignore]`d — it is a measurement, not a gate. It reads its inputs
//! from the environment so the driver script owns the throwaway home:
//!
//!   PROBE_INDEX_DIR   directory of installed indexes (REQUIRED)
//!   PROBE_MODE        `pinned` (pre-fix `open_index`) | `transient` (post-fix)
//!
//! Run it through `scripts/probe-b-index-residency.sh`, which builds the
//! throwaway home, runs BOTH arms, and prints the delta. Never point it
//! at the operator's live `~/.svrnmesh/indexes/`.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};

/// Resident set size in KiB, from `/proc/self/status`. Linux-only, which
/// is where the probe runs; the test skips rather than lying elsewhere.
fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.trim().trim_end_matches(" kB").trim().parse().ok();
        }
    }
    None
}

fn probe_embed_fn(dims: usize) -> EmbedFn {
    Arc::new(move |_text: &str| {
        let v = vec![0.01_f32; dims];
        Box::pin(async move { Ok(v) })
    })
}

#[tokio::test]
#[ignore = "probe: run via scripts/probe-b-index-residency.sh against a THROWAWAY index dir"]
async fn probe_b_index_handle_residency() {
    let Ok(dir) = std::env::var("PROBE_INDEX_DIR") else {
        panic!("PROBE_INDEX_DIR is required — this probe must never be pointed at a live home");
    };
    let index_dir = PathBuf::from(&dir);
    assert!(
        !index_dir.ends_with(".svrnmesh/indexes"),
        "refusing to run against what looks like the operator's live index dir: {dir}"
    );
    let mode = std::env::var("PROBE_MODE").unwrap_or_else(|_| "transient".into());
    assert!(
        mode == "pinned" || mode == "transient",
        "PROBE_MODE must be `pinned` or `transient`, got `{mode}`"
    );
    let dims: usize = std::env::var("PROBE_EMBED_DIMS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);

    let recipes = index_dir.join("_probe_recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = CorpusEngine::new(recipes, index_dir.clone(), probe_embed_fn(dims));

    let rss_start = rss_kib().expect("this probe needs /proc (Linux)");
    let listed = engine.installed_indexes().await.expect("installed_indexes");
    let rss_after_list = rss_kib().unwrap();
    let corpora = listed.len();
    assert!(corpora > 0, "no installed corpora under {dir} — nothing to measure");

    // ── The sweep ───────────────────────────────────────────────
    // Exactly what `corpus_maintenance::sweep_once` does to every
    // installed corpus, in the two modes under comparison.
    let sweep_started = std::time::Instant::now();
    let mut opened = 0usize;
    let mut failed = 0usize;
    for info in &listed {
        let opened_index = if mode == "pinned" {
            engine.open_index(&info.path).await
        } else {
            engine.open_index_transient(&info.path).await
        };
        match opened_index {
            Ok(idx) => {
                opened += 1;
                let _ = idx.unindexed_rows_estimate().await;
            }
            Err(_) => failed += 1,
        }
    }
    let sweep_ms = sweep_started.elapsed().as_millis() as u64;
    let rss_after_sweep = rss_kib().unwrap();
    let resident_handles = engine.index_cache_len();

    // ── Per-query wall time ─────────────────────────────────────
    // Three queries so a single sample is not reported as a result
    // (§18.5); the driver script reports the bracket, not the mean.
    let query_vec = vec![0.01_f32; dims];
    let mut query_ms: Vec<u64> = Vec::new();
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let mut hits = 0usize;
        for info in &listed {
            if let Ok(idx) = engine.open_index(&info.path).await {
                if let Ok(r) = idx.search(&query_vec, "governance", 5).await {
                    hits += r.len();
                }
            }
        }
        query_ms.push(t.elapsed().as_millis() as u64);
        let _ = hits;
    }
    let rss_after_queries = rss_kib().unwrap();

    // Machine-readable line for the driver script to collect.
    println!(
        "PROBE_B_RESULT mode={mode} corpora={corpora} opened={opened} failed={failed} \
         rss_start_kib={rss_start} rss_after_list_kib={rss_after_list} \
         rss_after_sweep_kib={rss_after_sweep} rss_after_queries_kib={rss_after_queries} \
         sweep_ms={sweep_ms} resident_handles={resident_handles} \
         query_ms_min={} query_ms_max={}",
        query_ms.iter().min().unwrap(),
        query_ms.iter().max().unwrap(),
    );
    println!(
        "PROBE_B_DERIVED mode={mode} sweep_rss_delta_kib={} per_handle_kib={}",
        rss_after_sweep.saturating_sub(rss_after_list),
        if resident_handles > 0 {
            rss_after_sweep.saturating_sub(rss_after_list) / resident_handles as u64
        } else {
            0
        },
    );
}
