// SPDX-License-Identifier: AGPL-3.0-or-later
//! P5.2b probe — multi-vector MaxSim storage prototype (gate G10,
//! `research/enrichment-spikes/README.md`).
//!
//! Answers, report-only: can the pinned lancedb (0.27.2 / lance 4.0) hold a
//! ColBERT-style multivector column (`List<FixedSizeList<f32, dim>>`) as a
//! corpus-sibling table, what does it cost on disk, and what do MaxSim
//! queries cost brute-force and IVF-PQ-indexed? Numbers are compared against
//! the RETRIEVAL_REDESIGN.md:261-266 sizing (~3-6 GB per 188k chunks at
//! 96-128d int8 + 50% token pooling).
//!
//! Vectors are synthetic (seeded xorshift, unit-normalized) at the documented
//! design shape — storage footprint and MaxSim compute depend on shape, not
//! semantics. Row 0 is planted to exactly contain query 0's vectors, so the
//! probe also verifies MaxSim ranking semantics end-to-end (planted row must
//! rank first with ~zero distance).
//!
//! Usage:
//!   cargo run --release -p corpus-engine --features treesitter \
//!     --example maxsim_probe -- <out_dir> [rows] [dim] [queries] [k]
//!
//! (`--release` because query latency is a measurand — the SP4 exception to
//! the debug-builds rule. Dev profile compiles lance unoptimized.)

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use arrow_array::builder::{FixedSizeListBuilder, Float32Builder, ListBuilder, StringBuilder};
use arrow_array::{Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::DistanceType;

const TOK_MIN: usize = 60;
const TOK_MAX: usize = 160; // mean ~110 vectors/row ≈ 200-token chunk after 50% pooling
const QUERY_TOKENS: usize = 32; // ColBERT-style query length
const EXTRAPOLATE_ROWS: usize = 188_000; // SEP pilot scale, RETRIEVAL_REDESIGN.md:266

struct XorShift(u64);
impl XorShift {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn next_f32(&mut self) -> f32 {
        // Uniform in [-1, 1)
        (self.next_u64() >> 40) as f32 / (1u64 << 23) as f32 * 2.0 - 1.0
    }
    fn unit_vec(&mut self, dim: usize) -> Vec<f32> {
        let mut v: Vec<f32> = (0..dim).map(|_| self.next_f32()).collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }
}

fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size_bytes(&p);
            } else if let Ok(md) = entry.metadata() {
                total += md.len();
            }
        }
    }
    total
}

fn percentile(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_ms.len() as f64 - 1.0) * p).round() as usize;
    sorted_ms[idx]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: maxsim_probe <out_dir> [rows=20000] [dim=128] [queries=20] [k=10]");
        std::process::exit(2);
    }
    let out_dir = std::path::PathBuf::from(&args[1]);
    let rows: usize = args.get(2).map_or(20_000, |s| s.parse().unwrap());
    let dim: usize = args.get(3).map_or(128, |s| s.parse().unwrap());
    let n_queries: usize = args.get(4).map_or(20, |s| s.parse().unwrap());
    let k: usize = args.get(5).map_or(10, |s| s.parse().unwrap());

    let table_dir = out_dir.join("maxsim_probe.lance");
    if table_dir.exists() {
        std::fs::remove_dir_all(&table_dir)?;
    }
    std::fs::create_dir_all(&out_dir)?;

    // Topic centroids give the corpus cluster structure — IVF partitioning is
    // meaningless over uniform random unit vectors (all centroids equidistant),
    // which would make indexed-recall numbers pure artifact. Each row draws
    // its token vectors around one of 256 topics; queries target a topic.
    const TOPICS: usize = 256;
    const TOPIC_WEIGHT: f32 = 0.7;
    let mut crng = XorShift(0xC3_A7_2019);
    let centroids: Vec<Vec<f32>> = (0..TOPICS).map(|_| crng.unit_vec(dim)).collect();
    let mix = |rng: &mut XorShift, topic: usize| -> Vec<f32> {
        let noise = rng.unit_vec(dim);
        let mut v: Vec<f32> = centroids[topic]
            .iter()
            .zip(&noise)
            .map(|(c, n)| c * TOPIC_WEIGHT + n * (1.0 - TOPIC_WEIGHT))
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        v.iter_mut().for_each(|x| *x /= norm);
        v
    };

    // Queries first: query 0's vectors are planted into row 0.
    let mut qrng = XorShift(0x5EED_CAFE);
    let queries: Vec<Vec<Vec<f32>>> = (0..n_queries)
        .map(|qi| (0..QUERY_TOKENS).map(|_| mix(&mut qrng, (qi * 13) % TOPICS)).collect())
        .collect();

    // ── Write ────────────────────────────────────────────────────────────
    let db = lancedb::connect(out_dir.to_str().unwrap()).execute().await?;
    let fsl_field = Field::new(
        "item",
        DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim as i32),
        true,
    );
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("vecs", DataType::List(Arc::new(fsl_field)), true),
    ]));
    let table = db
        .create_empty_table("maxsim_probe", schema.clone())
        .execute()
        .await?;
    let mut rng = XorShift(0xD15C_0B01);
    let batch_rows = 1_000;
    let t_write = Instant::now();
    let mut total_vectors = 0usize;
    let mut written = 0usize;
    while written < rows {
        let n = batch_rows.min(rows - written);
        let mut ids = StringBuilder::new();
        let mut vecs = ListBuilder::new(FixedSizeListBuilder::new(Float32Builder::new(), dim as i32));
        for r in written..written + n {
            ids.append_value(format!("chunk-{r:07}"));
            let n_tok = if r == 0 {
                QUERY_TOKENS
            } else {
                TOK_MIN + (rng.next_u64() as usize % (TOK_MAX - TOK_MIN))
            };
            let topic = rng.next_u64() as usize % TOPICS;
            for t in 0..n_tok {
                let v = if r == 0 { queries[0][t].clone() } else { mix(&mut rng, topic) };
                vecs.values().values().append_slice(&v);
                vecs.values().append(true);
            }
            vecs.append(true);
            total_vectors += n_tok;
        }
        let ids = Arc::new(ids.finish()) as Arc<dyn Array>;
        let vecs = Arc::new(vecs.finish()) as Arc<dyn Array>;
        let batch = RecordBatch::try_new(schema.clone(), vec![ids, vecs])?;
        table.add(vec![batch]).execute().await?;
        written += n;
    }
    let write_s = t_write.elapsed().as_secs_f64();
    let disk = dir_size_bytes(&table_dir);
    let raw = total_vectors as u64 * dim as u64 * 4;
    let per_row = disk as f64 / rows as f64;
    let extrapolated_gb = per_row * EXTRAPOLATE_ROWS as f64 / 1e9;

    println!("## write");
    println!("rows: {rows}  dim: {dim}  vectors/row mean: {:.1}", total_vectors as f64 / rows as f64);
    println!("write wall: {write_s:.1}s  ({:.0} rows/s)", rows as f64 / write_s);
    println!("disk: {:.2} GB  (raw f32 {:.2} GB, ratio {:.2})", disk as f64 / 1e9, raw as f64 / 1e9, disk as f64 / raw as f64);
    println!("per-row: {:.1} KB  → {EXTRAPOLATE_ROWS} rows ≈ {extrapolated_gb:.2} GB f32 (int8 ≈ {:.2} GB)", per_row / 1e3, extrapolated_gb / 4.0);

    // ── Query helper ─────────────────────────────────────────────────────
    async fn run_queries(
        table: &lancedb::Table,
        queries: &[Vec<Vec<f32>>],
        k: usize,
        nprobes: Option<usize>,
        refine: Option<u32>,
        label: &str,
    ) -> Result<Vec<Vec<(String, f32)>>, Box<dyn std::error::Error>> {
        // Warm-up
        let mut q = table.query().nearest_to(queries[0][0].clone())?;
        for v in &queries[0][1..] {
            q = q.add_query_vector(v.clone())?;
        }
        let mut q = q.column("vecs");
        if let Some(np) = nprobes {
            q = q.nprobes(np);
        }
        if let Some(rf) = refine {
            q = q.refine_factor(rf);
        }
        let _ = q
            .distance_type(DistanceType::Cosine)
            .limit(k)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;

        let mut all = Vec::new();
        let mut times = Vec::new();
        for qv in queries {
            let t = Instant::now();
            let mut q = table.query().nearest_to(qv[0].clone())?;
            for v in &qv[1..] {
                q = q.add_query_vector(v.clone())?;
            }
            let mut q = q.column("vecs");
            if let Some(np) = nprobes {
                q = q.nprobes(np);
            }
            if let Some(rf) = refine {
                q = q.refine_factor(rf);
            }
            let batches = q
                .distance_type(DistanceType::Cosine)
                .limit(k)
                .execute()
                .await?
                .try_collect::<Vec<_>>()
                .await?;
            times.push(t.elapsed().as_secs_f64() * 1e3);
            let mut hits = Vec::new();
            for b in &batches {
                let ids = b.column_by_name("id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
                let dist = b
                    .column_by_name("_distance")
                    .map(|c| c.as_any().downcast_ref::<arrow_array::Float32Array>().unwrap().clone());
                for i in 0..b.num_rows() {
                    hits.push((ids.value(i).to_string(), dist.as_ref().map_or(f32::NAN, |d| d.value(i))));
                }
            }
            all.push(hits);
        }
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("## query ({label})");
        println!(
            "n={}  mean {:.1} ms  p50 {:.1} ms  p90 {:.1} ms  max {:.1} ms",
            times.len(),
            times.iter().sum::<f64>() / times.len() as f64,
            percentile(&sorted, 0.5),
            percentile(&sorted, 0.9),
            percentile(&sorted, 1.0)
        );
        Ok(all)
    }

    let flat = run_queries(&table, &queries, k, None, None, "brute-force, no index").await?;
    let planted = &flat[0];
    let rank = planted.iter().position(|(id, _)| id == "chunk-0000000");
    println!(
        "planted-row check (query 0 ⊂ row 0): rank {:?} of top-{k}, top hit {:?}",
        rank.map(|r| r + 1),
        planted.first()
    );

    // ── IVF-PQ on the multivector column: supported? ─────────────────────
    let t_idx = Instant::now();
    let idx_result = table
        .create_index(
            &["vecs"],
            lancedb::index::Index::IvfPq(
                lancedb::index::vector::IvfPqIndexBuilder::default()
                    .num_partitions(((rows as f64).sqrt() as u32).clamp(8, 4096))
                    .distance_type(DistanceType::Cosine),
            ),
        )
        .replace(true)
        .execute()
        .await;
    match idx_result {
        Err(e) => println!("## index\nIVF-PQ on multivector column: FAILED — {e}"),
        Ok(()) => {
            println!("## index\nIVF-PQ on multivector column: OK, built in {:.1}s", t_idx.elapsed().as_secs_f64());
            let indexed = run_queries(&table, &queries, k, None, None, "IVF-PQ indexed, default nprobes").await?;
            let indexed_hi = run_queries(&table, &queries, k, Some(64), None, "IVF-PQ indexed, nprobes=64").await?;
            let indexed_rf = run_queries(&table, &queries, k, Some(64), Some(10), "IVF-PQ indexed, nprobes=64 rf=10").await?;
            for (label, ix_all) in [("default nprobes", &indexed), ("nprobes=64", &indexed_hi), ("nprobes=64 rf=10", &indexed_rf)] {
                // Top-k overlap vs brute-force (PQ recall proxy).
                let overlaps: Vec<f64> = flat
                    .iter()
                    .zip(ix_all.iter())
                    .map(|(f, ix)| {
                        let fset: std::collections::HashSet<&String> = f.iter().map(|(id, _)| id).collect();
                        ix.iter().filter(|(id, _)| fset.contains(id)).count() as f64 / k as f64
                    })
                    .collect();
                println!(
                    "top-{k} overlap vs brute-force ({label}): mean {:.2}  min {:.2}",
                    overlaps.iter().sum::<f64>() / overlaps.len() as f64,
                    overlaps.iter().cloned().fold(f64::INFINITY, f64::min)
                );
            }
        }
    }

    Ok(())
}
