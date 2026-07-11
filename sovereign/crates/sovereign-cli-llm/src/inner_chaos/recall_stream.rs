// SPDX-License-Identifier: AGPL-3.0-or-later
//! `--recall-stream` — the streaming-insert validation for the
//! incremental memory tree (Phase 4 of the tiered-retrieval memory
//! port). The `--recall` harness seeds a static pool once per thread,
//! so it can never exercise *incremental* re-clustering; this mode
//! does, with a three-tree oracle isolating two independent axes:
//!
//! 1. **Incremental-correctness** (incremental ≈ batch): seed ~40% of
//!    the fixture, batch-build the base tree, stream the remaining
//!    ~60% one-by-one through `mem_tree::insert_memory`, then compare
//!    per-plant retrieval ranks against a fresh store where all seeds
//!    were batch-built at once (the oracle). Pass: ≥7/8 plants
//!    identical rank, 8/8 within one rank.
//! 2. **Cost**: cumulative LLM calls across the stream must be far
//!    below N × full-rebuild; reported as calls-per-insert together
//!    with the per-op trigger histogram (the glassbox traces).
//!
//! A flat (T1-only) probe on the oracle store BEFORE its atlas is
//! built gives the third tree — the baseline that shows what the tier
//! adds under streaming, on the same final pool.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use serde::Serialize;
use sovereign_core::traits::MemoryScope;
use sovereign_tools::mem_tree::InsertTrace;

use super::personas::resolve_bench_dir;
use super::recall::{build_seed_set, load_recall_fixture, RecallRunOptions};
use super::runner::{build_thread_session, seed_memories, unix_seconds, WITNESS_SKILL};
use crate::voice_eval::scenarios::SeedMemory;

const PROBE_K: usize = 10;
/// Fraction of the seed set batch-built as the base tree; the rest
/// streams through `insert_memory`.
const BASE_FRACTION_PCT: usize = 40;

#[derive(Debug, Clone, Serialize)]
pub struct StreamPlantRanks {
    pub plant_id: String,
    /// Rank (1-based) in top-K under flat T1 on the full pool; None =
    /// not retrieved.
    pub flat_rank: Option<usize>,
    /// Rank under the incrementally-maintained tree.
    pub incremental_rank: Option<usize>,
    /// Rank under the full-batch oracle tree over the same pool.
    pub batch_rank: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamRecallReport {
    pub seeds_total: usize,
    pub seeds_base: usize,
    pub seeds_streamed: usize,
    pub per_plant: Vec<StreamPlantRanks>,
    /// Plants whose incremental and batch ranks are identical
    /// (both-None counts as identical).
    pub incremental_batch_identical: usize,
    /// Plants within one rank of the oracle (None treated as K+1).
    pub incremental_batch_within_one: usize,
    pub incremental_batch_match_rate: f64,
    /// LLM calls spent across the whole stream (per the traces).
    pub cumulative_llm_calls: usize,
    pub calls_per_insert: f64,
    /// Trigger-ladder histogram: op name → fire count.
    pub op_histogram: BTreeMap<String, usize>,
    /// Full glassbox traces, one per streamed insert — the data the
    /// knobs (θ₀, λ, T, δ, τ_c) get tuned against.
    pub traces: Vec<InsertTrace>,
}

pub async fn run_recall_stream(opts: &RecallRunOptions) -> Result<(), String> {
    let bench_dir = resolve_bench_dir(opts.bench_dir.as_ref())?;
    let fixture_path = opts
        .fixture_path
        .clone()
        .unwrap_or_else(|| bench_dir.join("recall_fixture.toml"));
    let fixture = load_recall_fixture(&fixture_path)?;
    let seed_set = build_seed_set(&fixture);
    let skills_dir = crate::voice_eval::runner::resolve_skills_dir(opts.skills_dir.as_ref())
        .map_err(|e| e.to_string())?;
    let scope = MemoryScope::Scoped(WITNESS_SKILL.to_string());

    // Deterministic interleaved split (i % 5 ∈ {0,1} → base) so base
    // and stream both carry plants, distractors, and filler — a split
    // by key prefix would put every plant in one bucket.
    let mut base: BTreeMap<String, SeedMemory> = BTreeMap::new();
    let mut stream: Vec<(String, SeedMemory)> = Vec::new();
    for (i, (k, v)) in seed_set.iter().enumerate() {
        if i % 5 < BASE_FRACTION_PCT / 20 {
            base.insert(k.clone(), v.clone());
        } else {
            stream.push((k.clone(), v.clone()));
        }
    }
    println!(
        "inner-chaos RECALL-STREAM — {} seeds: {} base (batch tree), {} streamed (incremental)",
        seed_set.len(),
        base.len(),
        stream.len()
    );

    // ── Tree 1: base batch + incremental stream ──────────────────
    let (inc_session, _tmp_a) = build_thread_session(
        &skills_dir,
        opts.daemon_base.as_deref(),
        opts.chat_model.as_deref(),
        opts.temperature,
    )
    .await?;
    seed_memories(inc_session.store.as_ref(), &base, Some(WITNESS_SKILL))
        .await
        .map_err(|e| format!("base seed failed: {e}"))?;
    let t = Instant::now();
    let base_nodes = sovereign_tools::mem_atlas::build_memory_atlas(
        &inc_session.inference,
        inc_session.store.as_ref(),
        &scope,
    )
    .await
    .map_err(|e| format!("base atlas build failed: {e}"))?;
    println!(
        "base tree: {base_nodes} nodes over {} memories in {}s",
        base.len(),
        t.elapsed().as_secs()
    );

    let mut traces: Vec<InsertTrace> = Vec::new();
    let stream_started = Instant::now();
    for (i, (key, seed)) in stream.iter().enumerate() {
        let singleton: BTreeMap<String, SeedMemory> = BTreeMap::from([(key.clone(), seed.clone())]);
        seed_memories(inc_session.store.as_ref(), &singleton, Some(WITNESS_SKILL))
            .await
            .map_err(|e| format!("stream seed {key} failed: {e}"))?;
        let mem_id = format!("inner-chaos-{key}");
        let pool = inc_session
            .store
            .get_all_memories_for_scope(&scope)
            .await
            .map_err(|e| format!("pool read failed: {e}"))?;
        let Some(memory) = pool.into_iter().find(|m| m.id == mem_id) else {
            return Err(format!("streamed memory {mem_id} not found after seed"));
        };
        match sovereign_tools::mem_tree::insert_memory(
            &inc_session.inference,
            inc_session.store.as_ref(),
            &scope,
            &memory,
        )
        .await
        {
            Ok(trace) => traces.push(trace),
            Err(e) => eprintln!("  insert {key} failed: {e}"),
        }
        if (i + 1) % 20 == 0 {
            let llm: usize = traces.iter().map(|t| t.llm_calls).sum();
            println!(
                "  streamed {}/{} ({}s, {llm} LLM calls so far)",
                i + 1,
                stream.len(),
                stream_started.elapsed().as_secs()
            );
        }
    }
    let incremental_ranks = probe_ranks(&inc_session, &fixture, &scope).await;

    // ── Trees 2+3: fresh store — flat probe, then full batch oracle ─
    let (oracle_session, _tmp_b) = build_thread_session(
        &skills_dir,
        opts.daemon_base.as_deref(),
        opts.chat_model.as_deref(),
        opts.temperature,
    )
    .await?;
    seed_memories(
        oracle_session.store.as_ref(),
        &seed_set,
        Some(WITNESS_SKILL),
    )
    .await
    .map_err(|e| format!("oracle seed failed: {e}"))?;
    let flat_ranks = probe_ranks(&oracle_session, &fixture, &scope).await;
    let t = Instant::now();
    let oracle_nodes = sovereign_tools::mem_atlas::build_memory_atlas(
        &oracle_session.inference,
        oracle_session.store.as_ref(),
        &scope,
    )
    .await
    .map_err(|e| format!("oracle atlas build failed: {e}"))?;
    println!(
        "oracle tree: {oracle_nodes} nodes over {} memories in {}s",
        seed_set.len(),
        t.elapsed().as_secs()
    );
    let batch_ranks = probe_ranks(&oracle_session, &fixture, &scope).await;

    // ── Report + assertions ───────────────────────────────────────
    let mut per_plant = Vec::new();
    let mut identical = 0usize;
    let mut within_one = 0usize;
    let as_delta_rank = |r: Option<usize>| r.unwrap_or(PROBE_K + 1) as i64;
    for plant in &fixture.plants {
        let inc = incremental_ranks.get(&plant.id).copied().flatten();
        let bat = batch_ranks.get(&plant.id).copied().flatten();
        let flat = flat_ranks.get(&plant.id).copied().flatten();
        if inc == bat {
            identical += 1;
        }
        if (as_delta_rank(inc) - as_delta_rank(bat)).abs() <= 1 {
            within_one += 1;
        }
        per_plant.push(StreamPlantRanks {
            plant_id: plant.id.clone(),
            flat_rank: flat,
            incremental_rank: inc,
            batch_rank: bat,
        });
    }
    let cumulative_llm_calls: usize = traces.iter().map(|t| t.llm_calls).sum();
    let mut op_histogram: BTreeMap<String, usize> = BTreeMap::new();
    for t in &traces {
        *op_histogram.entry(format!("{:?}", t.op)).or_insert(0) += 1;
    }
    let n_plants = fixture.plants.len();
    let report = StreamRecallReport {
        seeds_total: seed_set.len(),
        seeds_base: base.len(),
        seeds_streamed: stream.len(),
        per_plant,
        incremental_batch_identical: identical,
        incremental_batch_within_one: within_one,
        incremental_batch_match_rate: identical as f64 / n_plants.max(1) as f64,
        cumulative_llm_calls,
        calls_per_insert: cumulative_llm_calls as f64 / stream.len().max(1) as f64,
        op_histogram,
        traces,
    };

    println!(
        "\n  {:<28} {:>6} {:>12} {:>7}",
        "plant", "flat", "incremental", "batch"
    );
    for p in &report.per_plant {
        let f = |r: Option<usize>| r.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
        println!(
            "  {:<28} {:>6} {:>12} {:>7}",
            p.plant_id,
            f(p.flat_rank),
            f(p.incremental_rank),
            f(p.batch_rank)
        );
    }
    println!(
        "\n  incremental==batch: {identical}/{n_plants} identical, {within_one}/{n_plants} within-1"
    );
    println!(
        "  cost: {cumulative_llm_calls} LLM calls over {} inserts ({:.2} calls/insert; \
         full rebuild would be ~{} calls/insert)",
        report.seeds_streamed, report.calls_per_insert, oracle_nodes
    );
    println!("  triggers: {:?}", report.op_histogram);

    let stamp = unix_seconds().to_string();
    let report_path = std::path::PathBuf::from(format!(
        "test-artifacts/inner-chaos-recall-stream-{stamp}.report.json"
    ));
    write_stream_json(&report_path, &report)?;
    println!("  report JSON at {}", report_path.display());

    // Assertions (spec): incremental must track the batch oracle.
    if identical < n_plants.saturating_sub(1) || within_one < n_plants {
        return Err(format!(
            "incremental-vs-batch divergence: {identical}/{n_plants} identical \
             (need ≥{}), {within_one}/{n_plants} within-1 (need {n_plants})",
            n_plants.saturating_sub(1)
        ));
    }
    // Consolidation NOOPs and attaches are free; the ladder holds when
    // the amortized cost stays well under a per-insert rebuild.
    if report.calls_per_insert >= 1.0 {
        return Err(format!(
            "cost regression: {:.2} LLM calls/insert — the ladder is firing \
             expensive ops on the common path",
            report.calls_per_insert
        ));
    }
    Ok(())
}

/// Probe all plants through the REAL recall path, returning 1-based
/// ranks in top-K (None = not retrieved).
async fn probe_ranks(
    session: &crate::chat_cmd::bootstrap::ChatSession,
    fixture: &super::recall::RecallFixture,
    scope: &MemoryScope,
) -> BTreeMap<String, Option<usize>> {
    let mut out = BTreeMap::new();
    for plant in &fixture.plants {
        let want = format!("inner-chaos-plant-{}", plant.id);
        let top = sovereign_core::memory::recall_relevant_memories_embed(
            session.inference.as_ref(),
            session.store.as_ref(),
            scope,
            &plant.oblique_callback,
            PROBE_K,
        )
        .await
        .unwrap_or_default();
        out.insert(
            plant.id.clone(),
            top.iter().position(|m| m.id == want).map(|r| r + 1),
        );
    }
    out
}

fn write_stream_json(path: &Path, report: &StreamRecallReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create report dir {}: {e}", parent.display()))?;
    }
    let json =
        serde_json::to_string_pretty(report).map_err(|e| format!("serialize report: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write report {}: {e}", path.display()))
}
