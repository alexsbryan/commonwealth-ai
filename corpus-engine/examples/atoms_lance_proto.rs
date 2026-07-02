// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase-0 spike for ATLAS_STORAGE_V2: convert a real atlas `atoms.json` into a
//! columnar `atoms.lance` dataset and measure the numbers that decide whether
//! the v2 store is worth it — on disk size vs the v1 rkyv (1204 MB) and the
//! 758 MB JSON, open time + resident RSS (the inference co-residency concern),
//! the columnar type-filter latency (vs v1's 44 ms full-record scan), a point
//! lookup, and the Lance thread footprint.
//!
//! Lance is ALREADY a workspace dep (chunks.lance), so this adds no new engine
//! — it reuses the runtime the daemon already runs.
//!
//! Usage (release — debug JSON parse is ~38s):
//!   cargo run --release -p corpus-engine --example atoms_lance_proto -- \
//!       ~/.sovereign/indexes/wikipedia/atlas  /tmp/atlas-lance-proto
//!
//! NOTE: wikipedia has no atom embeddings on disk, so this measures the
//! *structural* columnar story (the wikipedia win). The embedding column is a
//! separate, SEP-relevant axis — its bound is 1.67M × dims × 4 B raw (~6.8 GB
//! at 1024-dim) before Lance IVF-PQ compression.

use std::sync::Arc;
use std::time::Instant;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{Array, Float32Array, RecordBatch, StringArray, UInt8Array};
use corpus_engine::enrichment::atlas::AtomEnvelope;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

fn rss_mb() -> u64 {
    let pid = std::process::id().to_string();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|kb| kb / 1024)
        .unwrap_or(0)
}

fn thread_count() -> usize {
    let pid = std::process::id().to_string();
    std::process::Command::new("ps")
        .args(["-M", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().count().saturating_sub(1))
        .unwrap_or(0)
}

fn dir_size_bytes(path: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_size_bytes(&p);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

/// One atom projected to the representative scalar/text columns (the size
/// drivers). `kind` is the discriminant; the rest are per-variant fields or "".
struct Row {
    str_id: String,
    kind: u8,
    name: String,
    label: String,
    content: String,
    description: String,
    excerpt: String,
    subtype: String,
    salience: f32,
    confidence: f32,
    ev_chunk_id: String,
    ev_preview: String,
}

fn project(atom: &AtomEnvelope) -> Row {
    use corpus_engine::enrichment::atlas::ChunkRef;
    let first_ev = |c: Option<&ChunkRef>| -> (String, String) {
        match c {
            Some(r) => (
                r.chunk_id.clone(),
                r.passage_preview.clone().unwrap_or_default(),
            ),
            None => (String::new(), String::new()),
        }
    };
    let mut r = Row {
        str_id: atom.id().as_str().to_string(),
        kind: 0,
        name: String::new(),
        label: String::new(),
        content: String::new(),
        description: String::new(),
        excerpt: String::new(),
        subtype: String::new(),
        salience: 0.0,
        confidence: 0.0,
        ev_chunk_id: String::new(),
        ev_preview: String::new(),
    };
    match atom {
        AtomEnvelope::Entity(e) => {
            r.kind = 0;
            r.name = e.canonical_name.clone();
            r.description = e.description.clone();
            r.subtype = e.entity_type.as_str_repr().to_string();
            r.salience = e.salience;
            let (c, p) = first_ev(Some(&e.first_appearance));
            r.ev_chunk_id = c;
            r.ev_preview = p;
        }
        AtomEnvelope::Event(ev) => {
            r.kind = 1;
            r.description = ev.description.clone();
            let (c, p) = first_ev(ev.evidence.first());
            r.ev_chunk_id = c;
            r.ev_preview = p;
        }
        AtomEnvelope::State(s) => {
            r.kind = 2;
            let (c, p) = first_ev(s.evidence.first());
            r.ev_chunk_id = c;
            r.ev_preview = p;
        }
        AtomEnvelope::Relation(rel) => {
            r.kind = 3;
            r.label = rel.label.clone();
            let (c, p) = first_ev(rel.evidence.first());
            r.ev_chunk_id = c;
            r.ev_preview = p;
        }
        AtomEnvelope::Claim(c) => {
            r.kind = 4;
            r.content = c.content.clone();
            r.excerpt = c.quotable_excerpt.clone().unwrap_or_default();
            r.confidence = c.confidence.unwrap_or(0.5);
            let (ci, p) = first_ev(c.evidence.first());
            r.ev_chunk_id = ci;
            r.ev_preview = p;
        }
        AtomEnvelope::Question(_) => r.kind = 5,
        AtomEnvelope::Configuration(cfg) => {
            r.kind = 6;
            r.label = cfg.label.clone();
            r.description = cfg.description.clone();
        }
        AtomEnvelope::ArgumentReconstruction(a) => {
            r.kind = 7;
            r.name = a.name.clone();
        }
        AtomEnvelope::Position(_) => r.kind = 8,
        AtomEnvelope::Opposition(_) => r.kind = 9,
        AtomEnvelope::Asset(_) => r.kind = 10,
    }
    r
}

/// Deterministic synthetic vector for row `idx` — size + ANN-mechanism are
/// value-agnostic, so this avoids needing the embed model. Some structure
/// (not all-equal) so IVF-PQ k-means has something to cluster.
fn synth_embedding(idx: usize, dims: usize) -> Vec<f32> {
    (0..dims)
        .map(|j| {
            let h = (idx
                .wrapping_mul(2654435761)
                .wrapping_add(j.wrapping_mul(40503)))
                % 9973;
            (h as f32 / 9973.0) - 0.5
        })
        .collect()
}

fn schema(embed_dims: Option<usize>) -> Arc<Schema> {
    let mut fields = vec![
        Field::new("str_id", DataType::Utf8, false),
        Field::new("kind", DataType::UInt8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("label", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("description", DataType::Utf8, false),
        Field::new("excerpt", DataType::Utf8, false),
        Field::new("subtype", DataType::Utf8, false),
        Field::new("salience", DataType::Float32, false),
        Field::new("confidence", DataType::Float32, false),
        Field::new("ev_chunk_id", DataType::Utf8, false),
        Field::new("ev_preview", DataType::Utf8, false),
    ];
    if let Some(d) = embed_dims {
        fields.push(Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                d as i32,
            ),
            true,
        ));
    }
    Arc::new(Schema::new(fields))
}

fn batch(rows: &[Row], sch: &Arc<Schema>, embed_dims: Option<usize>, base: usize) -> RecordBatch {
    use arrow_array::{types::Float32Type, FixedSizeListArray};
    let col_str = |f: &dyn Fn(&Row) -> &str| {
        Arc::new(StringArray::from(
            rows.iter().map(|r| f(r)).collect::<Vec<_>>(),
        )) as arrow_array::ArrayRef
    };
    let mut cols: Vec<arrow_array::ArrayRef> = vec![
        col_str(&|r| r.str_id.as_str()),
        Arc::new(UInt8Array::from(
            rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        )),
        col_str(&|r| r.name.as_str()),
        col_str(&|r| r.label.as_str()),
        col_str(&|r| r.content.as_str()),
        col_str(&|r| r.description.as_str()),
        col_str(&|r| r.excerpt.as_str()),
        col_str(&|r| r.subtype.as_str()),
        Arc::new(Float32Array::from(
            rows.iter().map(|r| r.salience).collect::<Vec<_>>(),
        )),
        Arc::new(Float32Array::from(
            rows.iter().map(|r| r.confidence).collect::<Vec<_>>(),
        )),
        col_str(&|r| r.ev_chunk_id.as_str()),
        col_str(&|r| r.ev_preview.as_str()),
    ];
    if let Some(d) = embed_dims {
        let emb = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            (0..rows.len()).map(|i| Some(synth_embedding(base + i, d).into_iter().map(Some))),
            d as i32,
        );
        cols.push(Arc::new(emb));
    }
    RecordBatch::try_new(sch.clone(), cols).expect("record batch")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let first = args
        .next()
        .expect("usage: [--read] <atlas_dir|out_dir> <out_dir>");

    // Clean reader-only mode: open an already-built atoms.lance in a FRESH
    // process (no parse/write) so the RSS is purely the Lance reader footprint
    // — the inference co-residency number.
    if first == "--read" {
        let out_dir = std::path::PathBuf::from(args.next().expect("usage: --read <out_dir>"));
        println!(
            "READER-ONLY  threads(start)={} RSS(start)={} MB",
            thread_count(),
            rss_mb()
        );
        let t = Instant::now();
        let db = lancedb::connect(out_dir.to_str().unwrap())
            .execute()
            .await?;
        let tbl = db.open_table("atoms").execute().await?;
        println!(
            "OPEN: {} ms | RSS {} MB | threads {}",
            t.elapsed().as_millis(),
            rss_mb(),
            thread_count()
        );
        let t2 = Instant::now();
        let b: Vec<RecordBatch> = tbl
            .query()
            .select(Select::Columns(vec!["kind".into()]))
            .only_if("kind = 0")
            .execute()
            .await?
            .try_collect()
            .await?;
        let ents: usize = b.iter().map(|b| b.num_rows()).sum();
        println!(
            "TYPE-FILTER kind=Entity: {} ms | {ents} | RSS {} MB",
            t2.elapsed().as_millis(),
            rss_mb()
        );
        let t3 = Instant::now();
        let g: Vec<RecordBatch> = tbl
            .query()
            .only_if("str_id = 'entity-0001'")
            .execute()
            .await?
            .try_collect()
            .await?;
        let hit: usize = g.iter().map(|b| b.num_rows()).sum();
        println!(
            "POINT-LOOKUP: {} ms | {hit} row | RSS {} MB | threads {}",
            t3.elapsed().as_millis(),
            rss_mb(),
            thread_count()
        );
        return Ok(());
    }

    let atlas_dir = std::path::PathBuf::from(first);
    let out_dir = std::path::PathBuf::from(args.next().expect("usage: <atlas_dir> <out_dir>"));
    // Optional: --embed <dims> co-locates a synthetic vector column + builds an
    // IVF-PQ index (the SEP/dense vector axis); --cap <N> bounds atom count.
    let rest: Vec<String> = args.collect();
    let mut embed_dims: Option<usize> = None;
    let mut cap: Option<usize> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--embed" => embed_dims = it.next().and_then(|s| s.parse().ok()),
            "--cap" => cap = it.next().and_then(|s| s.parse().ok()),
            _ => {}
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir)?;

    println!(
        "threads(start)={} RSS(start)={} MB  embed_dims={:?} cap={:?}",
        thread_count(),
        rss_mb(),
        embed_dims,
        cap
    );

    // 1. Parse the real atoms.json (the v1 canonical source).
    let t0 = Instant::now();
    let atoms = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)?;
    let n = atoms.atoms.len();
    println!(
        "parse atoms.json: {} ms | {n} atoms | RSS {} MB",
        t0.elapsed().as_millis(),
        rss_mb()
    );

    // 2. Write atoms.lance in batches (bounds peak memory).
    let sch = schema(embed_dims);
    let db = lancedb::connect(out_dir.to_str().unwrap())
        .execute()
        .await?;
    let tbl = db
        .create_empty_table("atoms", sch.clone())
        .execute()
        .await?;
    let t1 = Instant::now();
    const BATCH: usize = 200_000;
    let limit = cap.unwrap_or(usize::MAX);
    let mut written = 0usize;
    let mut buf: Vec<Row> = Vec::with_capacity(BATCH.min(limit));
    for atom in atoms.atoms.iter().take(limit) {
        buf.push(project(atom));
        if buf.len() == BATCH {
            tbl.add(vec![batch(&buf, &sch, embed_dims, written)])
                .execute()
                .await?;
            written += buf.len();
            buf.clear();
        }
    }
    if !buf.is_empty() {
        tbl.add(vec![batch(&buf, &sch, embed_dims, written)])
            .execute()
            .await?;
        written += buf.len();
    }
    let write_ms = t1.elapsed().as_millis();
    let lance_path = out_dir.join("atoms.lance");
    let size_mb = dir_size_bytes(&lance_path) / (1 << 20);
    println!(
        "write atoms.lance: {write_ms} ms | {written} rows | SIZE {size_mb} MB | RSS {} MB",
        rss_mb()
    );

    // 2b. Embedding axis: raw column size (arithmetic) + IVF-PQ index + ANN.
    if let Some(d) = embed_dims {
        let raw_mb = (written as u64 * d as u64 * 4) / (1 << 20);
        let ti = Instant::now();
        let num_partitions = ((written as f64).sqrt() as u32).clamp(8, 4096);
        tbl.create_index(
            &["embedding"],
            lancedb::index::Index::IvfPq(
                lancedb::index::vector::IvfPqIndexBuilder::default()
                    .num_partitions(num_partitions)
                    .distance_type(lancedb::DistanceType::Cosine),
            ),
        )
        .replace(true)
        .execute()
        .await?;
        let idx_mb = dir_size_bytes(&lance_path.join("_indices")) / (1 << 20);
        let total_mb = dir_size_bytes(&lance_path) / (1 << 20);
        println!(
            "EMBED dims={d} n={written}: raw vectors={raw_mb} MB | IVF-PQ index={idx_mb} MB ({} ms, {num_partitions} part) | total atoms.lance={total_mb} MB",
            ti.elapsed().as_millis()
        );
        // ANN seeding mechanism: query returns ATOM rows (str_id) directly —
        // the resolve_atom_id_from_entry killer.
        let qv = synth_embedding(42, d);
        let ta = Instant::now();
        let hits: Vec<RecordBatch> = tbl
            .query()
            .nearest_to(qv)
            .map_err(|e| format!("nearest_to: {e}"))?
            .select(Select::Columns(vec!["str_id".into(), "kind".into()]))
            .limit(5)
            .execute()
            .await?
            .try_collect()
            .await?;
        let ids: Vec<String> = hits
            .iter()
            .filter_map(|b| b.column(0).as_any().downcast_ref::<StringArray>())
            .flat_map(|a| {
                (0..a.len())
                    .map(|i| a.value(i).to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        println!(
            "ANN seed (synthetic query): {} ms | returned atom ids directly: {:?}",
            ta.elapsed().as_millis(),
            &ids[..ids.len().min(5)]
        );
    }
    drop(atoms); // free the parsed JSON before the read measurements

    // 3. Fresh open (cold) — the inference co-residency numbers.
    let t2 = Instant::now();
    let db2 = lancedb::connect(out_dir.to_str().unwrap())
        .execute()
        .await?;
    let tbl2 = db2.open_table("atoms").execute().await?;
    let open_ms = t2.elapsed().as_millis();
    println!(
        "OPEN atoms.lance: {open_ms} ms | RSS {} MB | threads {}",
        rss_mb(),
        thread_count()
    );

    // 4. Columnar type-filter: count Entities (kind=0) reading only `kind`.
    let rss_before_scan = rss_mb();
    let t3 = Instant::now();
    let batches: Vec<RecordBatch> = tbl2
        .query()
        .select(Select::Columns(vec!["kind".into()]))
        .only_if("kind = 0")
        .execute()
        .await?
        .try_collect()
        .await?;
    let entities: usize = batches.iter().map(|b| b.num_rows()).sum();
    println!(
        "TYPE-FILTER kind=Entity: {} ms | {entities} entities | RSS {}->{} MB",
        t3.elapsed().as_millis(),
        rss_before_scan,
        rss_mb()
    );

    // 5. Point lookup by str_id (first atom's id).
    let first_id = {
        let b: Vec<RecordBatch> = tbl2
            .query()
            .select(Select::Columns(vec!["str_id".into()]))
            .limit(1)
            .execute()
            .await?
            .try_collect()
            .await?;
        b.first()
            .and_then(|b| {
                b.column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .map(|a| a.value(0).to_string())
            })
            .unwrap_or_default()
    };
    let t4 = Instant::now();
    let got: Vec<RecordBatch> = tbl2
        .query()
        .only_if(format!("str_id = '{first_id}'"))
        .execute()
        .await?
        .try_collect()
        .await?;
    let hit: usize = got.iter().map(|b| b.num_rows()).sum();
    println!(
        "POINT-LOOKUP str_id={first_id:?}: {} ms | {hit} row(s) | RSS {} MB | threads {}",
        t4.elapsed().as_millis(),
        rss_mb(),
        thread_count()
    );

    println!(
        "\nSUMMARY  size={size_mb} MB (v1 rkyv=1204 MB, atoms.json=758 MB)  open={open_ms} ms  type_filter={} ms",
        t3.elapsed().as_millis()
    );
    Ok(())
}
