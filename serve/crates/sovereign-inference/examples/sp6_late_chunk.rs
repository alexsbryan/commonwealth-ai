// SPDX-License-Identifier: AGPL-3.0-or-later
//! SP6 — late-chunking feasibility spike (enrichment spike bundle, gate G6).
//!
//! Answers three questions against the vendored llama-cpp-4 0.4.2 binding
//! (the 0.2.x binding failed this — null buffer from `embeddings_ith` under
//! pooled layout):
//!
//! 1. **Binding verdict** — does `PoolingType::None` + per-token
//!    `embeddings_ith` reads work on `qwen-embedding-0.6b`?
//! 2. **Memory ceiling** — peak RSS per long-context window W ∈ {8k,16k,32k}
//!    (n_seq_max=1, single-sequence doc windows).
//! 3. **Recall delta** — late-chunk span pooling vs the production status quo
//!    on a recall golden (sep bench questions, hit@k on expected-article
//!    membership).
//!
//! Honesty rule from the gate table: the status-quo baseline is
//! LAST-token-pooled chunks (the gguf's `qwen3.pooling_type = 3`), embedded
//! through the same gguf-native pooled path production uses (no explicit
//! `with_pooling_type`, 1024-token truncation, `<|endoftext|>` appended,
//! `AddBos::Always`). The late arm mean-pools token embeddings per chunk
//! span (plus a last-token-per-span variant). Both arms share identical
//! query embeddings, L2-normalized application-side per the family quirks.
//!
//! Fixtures come from `research/enrichment-spikes/scripts/sp6_prep.py`.
//! Chunk spans are located in-harness by exact byte match with a
//! whitespace-normalized fallback; the unlocatable count is itself part of
//! the finding (production late chunking needs offsets threaded through
//! `TextChunk` — this measures how lossy reconstruction is without them).
//!
//! Run:
//!   cargo run -p sovereign-inference --example sp6_late_chunk -- \
//!       --model sovereign/models/qwen-embedding-0.6b.gguf \
//!       --data-dir research/enrichment-spikes/data \
//!       --out research/enrichment-spikes/runs/sp6 \
//!       [--windows 8192,16384,32768] [--backend metal|cpu] [--smoke]

use std::fmt::Write as _;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use llama_cpp_4::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_4::context::LlamaContext;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel, Special};
use llama_cpp_4::token::LlamaToken;

const QUERY_INSTRUCTION: &str =
    "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: ";
const EOS: &str = "<|endoftext|>";
/// Production per-input truncation (embed_slot.rs max_input_tokens).
const MAX_CHUNK_TOKENS: usize = 1024;

fn main() {
    let args = Args::parse();
    let out_dir = args.out.clone();
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let backend = Arc::new(LlamaBackend::init().expect("LlamaBackend::init"));
    let gpu_layers = if args.cpu { 0 } else { 999 };
    let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
    let model = Arc::new(
        LlamaModel::load_from_file(&backend, &args.model, &model_params).expect("load model"),
    );
    let n_embd = model.n_embd() as usize;
    println!(
        "model loaded: dims={} layers={} size_mb={} backend={}",
        n_embd,
        model.n_layer(),
        model.model_size() / (1024 * 1024),
        if args.cpu { "cpu" } else { "metal" }
    );
    let rss_after_load = peak_rss_mb();
    println!("peak RSS after model load: {rss_after_load} MB");

    // ── Phase 0: binding verdict ────────────────────────────────────────
    let binding = binding_probe(&backend, &model, n_embd);
    println!("binding probe: {binding}");
    if !binding.starts_with("OK") {
        write_results(
            &out_dir,
            serde_json::json!({ "binding_verdict": binding, "aborted": true }),
        );
        eprintln!("token-level reads broken on this binding — aborting (verdict recorded)");
        std::process::exit(1);
    }

    // ── Fixtures ────────────────────────────────────────────────────────
    let mut docs = read_jsonl(&args.data_dir.join("sp6_docs.jsonl"), |v| Doc {
        slug: v["slug"].as_str().unwrap().to_string(),
        text: v["text"].as_str().unwrap().to_string(),
    });
    let chunks = read_jsonl(&args.data_dir.join("sp6_chunks.jsonl"), |v| ChunkRec {
        slug: v["slug"].as_str().unwrap().to_string(),
        text: v["text"].as_str().unwrap().to_string(),
    });
    let queries = read_jsonl(&args.data_dir.join("sp6_queries.jsonl"), |v| Query {
        qid: v["qid"].as_str().unwrap().to_string(),
        question: v["question"].as_str().unwrap().to_string(),
        expected: v["expected_slugs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect(),
    });
    if args.smoke {
        docs.truncate(3);
    }
    let doc_slugs: std::collections::HashSet<&str> = docs.iter().map(|d| d.slug.as_str()).collect();
    let chunks: Vec<ChunkRec> = chunks
        .into_iter()
        .filter(|c| doc_slugs.contains(c.slug.as_str()))
        .collect();
    let queries: Vec<Query> = queries
        .into_iter()
        .filter(|q| q.expected.iter().any(|s| doc_slugs.contains(s.as_str())))
        .collect();
    println!(
        "fixtures: {} docs, {} chunks, {} queries",
        docs.len(),
        chunks.len(),
        queries.len()
    );

    // ── Doc tokenization + byte offsets + chunk span location ───────────
    let t0 = Instant::now();
    let mut located: Vec<LocatedChunk> = Vec::new();
    let mut unlocatable = 0usize;
    let mut offset_mismatch_docs = 0usize;
    let mut doc_tokens: Vec<Vec<LlamaToken>> = Vec::new();
    let mut doc_offsets: Vec<Vec<usize>> = Vec::new(); // byte offset at each token start, +1 sentinel
    for (di, doc) in docs.iter().enumerate() {
        let tokens = model
            .str_to_token(&doc.text, AddBos::Always)
            .expect("tokenize doc");
        let mut offsets = Vec::with_capacity(tokens.len() + 1);
        let mut pos = 0usize;
        for &t in &tokens {
            offsets.push(pos);
            let bytes = model
                .token_to_bytes(t, Special::Plaintext)
                .unwrap_or_default();
            pos += bytes.len();
        }
        offsets.push(pos);
        if pos != doc.text.len() {
            offset_mismatch_docs += 1;
            eprintln!(
                "WARN: doc {} detok length {} != text length {} (spans may drift)",
                doc.slug,
                pos,
                doc.text.len()
            );
        }
        for (ci, c) in chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.slug == doc.slug)
        {
            // Production chunks carry a "{slug}\n\n" title prefix the source
            // doc does not have — locate the body when the full text misses.
            let span = locate(&doc.text, &c.text).or_else(|| {
                c.text
                    .split_once("\n\n")
                    .and_then(|(_, body)| locate(&doc.text, body))
            });
            match span {
                Some((b0, b1)) => {
                    // byte span -> token span [t0, t1)
                    let ts = offsets.partition_point(|&o| o <= b0).saturating_sub(1);
                    let te = offsets.partition_point(|&o| o < b1);
                    if te > ts {
                        located.push(LocatedChunk {
                            chunk_idx: ci,
                            doc_idx: di,
                            tok_start: ts,
                            tok_end: te.min(tokens.len()),
                        });
                    } else {
                        unlocatable += 1;
                    }
                }
                None => unlocatable += 1,
            }
        }
        doc_tokens.push(tokens);
        doc_offsets.push(offsets);
    }
    println!(
        "span location: {} located, {} unlocatable, {} docs with detok drift ({}s)",
        located.len(),
        unlocatable,
        offset_mismatch_docs,
        t0.elapsed().as_secs()
    );
    let scored_chunk_idxs: Vec<usize> = located.iter().map(|l| l.chunk_idx).collect();

    // ── Query embeddings (shared by all arms; gguf-native pooled path) ──
    let mut pooled_ctx = make_ctx(&backend, &model, 16384, 16, None, args.cpu);
    let query_vecs: Vec<Vec<f32>> = queries
        .iter()
        .map(|q| {
            let text = format!("{QUERY_INSTRUCTION}{}{EOS}", q.question);
            embed_pooled_one(&model, &mut pooled_ctx, &text)
        })
        .collect();
    println!("query embeddings: {} done", query_vecs.len());

    // ── Arm 1: status quo (per-chunk, gguf-native LAST pooling) ─────────
    let t0 = Instant::now();
    let mut truncated = 0usize;
    let mut sq_vecs: Vec<Vec<f32>> = vec![Vec::new(); chunks.len()];
    let mut batch_texts: Vec<(usize, Vec<LlamaToken>)> = Vec::new();
    for &ci in &scored_chunk_idxs {
        let text = format!("{}{EOS}", chunks[ci].text);
        let mut toks = model
            .str_to_token(&text, AddBos::Always)
            .expect("tokenize chunk");
        if toks.len() > MAX_CHUNK_TOKENS {
            toks.truncate(MAX_CHUNK_TOKENS);
            truncated += 1;
        }
        batch_texts.push((ci, toks));
    }
    for group in batch_texts.chunks(16) {
        pooled_ctx.clear_kv_cache();
        let mut batch = LlamaBatch::new(16384, 16);
        for (seq, (_, toks)) in group.iter().enumerate() {
            for (pos, &t) in toks.iter().enumerate() {
                batch
                    .add(t, pos as i32, &[seq as i32], true)
                    .expect("batch.add");
            }
        }
        pooled_ctx
            .decode(&mut batch)
            .expect("decode status-quo batch");
        for (seq, (ci, _)) in group.iter().enumerate() {
            let emb = pooled_ctx
                .embeddings_seq_ith(seq as i32)
                .expect("pooled embedding");
            sq_vecs[*ci] = l2_normalize(emb);
        }
    }
    let sq_secs = t0.elapsed().as_secs_f64();
    let rss_after_sq = peak_rss_mb();
    println!(
        "status-quo arm: {} chunks embedded in {:.1}s ({} truncated), peak RSS {} MB",
        scored_chunk_idxs.len(),
        sq_secs,
        truncated,
        rss_after_sq
    );
    drop(pooled_ctx);

    // ── Arm 2: late chunking per window W ───────────────────────────────
    let mut window_reports = Vec::new();
    let mut late_arms: Vec<(String, Vec<Vec<f32>>)> = Vec::new(); // (arm name, per-chunk vecs)
    for &w in &args.windows {
        let rss_before = peak_rss_mb();
        let t0 = Instant::now();
        let mut ctx = make_ctx(
            &backend,
            &model,
            w,
            1,
            Some(LlamaPoolingType::None),
            args.cpu,
        );
        let rss_after_ctx = peak_rss_mb();

        let mut mean_acc: Vec<(Vec<f32>, usize)> = vec![(vec![0.0; n_embd], 0); chunks.len()];
        let mut last_vec: Vec<Vec<f32>> = vec![Vec::new(); chunks.len()];
        let mut total_tokens = 0usize;
        for (di, tokens) in doc_tokens.iter().enumerate() {
            let spans: Vec<&LocatedChunk> = located.iter().filter(|l| l.doc_idx == di).collect();
            let mut wstart = 0usize;
            while wstart < tokens.len() {
                let wend = (wstart + w as usize).min(tokens.len());
                ctx.clear_kv_cache();
                let mut batch = LlamaBatch::new(w as usize, 1);
                for (i, &t) in tokens[wstart..wend].iter().enumerate() {
                    batch.add(t, i as i32, &[0], true).expect("batch.add");
                }
                ctx.decode(&mut batch).expect("decode window");
                total_tokens += wend - wstart;
                for l in &spans {
                    let s = l.tok_start.max(wstart);
                    let e = l.tok_end.min(wend);
                    if s >= e {
                        continue;
                    }
                    let (acc, count) = &mut mean_acc[l.chunk_idx];
                    for i in s..e {
                        let emb = ctx
                            .embeddings_ith((i - wstart) as i32)
                            .expect("token embedding");
                        for (a, &v) in acc.iter_mut().zip(emb) {
                            *a += v;
                        }
                    }
                    *count += e - s;
                    let last = l.tok_end - 1;
                    if last >= wstart && last < wend {
                        last_vec[l.chunk_idx] = l2_normalize(
                            ctx.embeddings_ith((last - wstart) as i32)
                                .expect("last-token embedding"),
                        );
                    }
                }
                wstart = wend;
            }
        }
        drop(ctx);
        let secs = t0.elapsed().as_secs_f64();
        let rss_after_run = peak_rss_mb();
        let mean_vecs: Vec<Vec<f32>> = mean_acc
            .into_iter()
            .map(|(mut acc, count)| {
                if count == 0 {
                    return Vec::new();
                }
                for a in &mut acc {
                    *a /= count as f32;
                }
                l2_normalize(&acc)
            })
            .collect();
        println!(
            "late arm W={w}: {total_tokens} tokens in {secs:.1}s ({:.0} tok/s), peak RSS {rss_before}->{rss_after_ctx}->{rss_after_run} MB",
            total_tokens as f64 / secs
        );
        window_reports.push(serde_json::json!({
            "window": w,
            "total_tokens": total_tokens,
            "wall_secs": secs,
            "tok_per_sec": total_tokens as f64 / secs,
            "peak_rss_mb_before": rss_before,
            "peak_rss_mb_after_ctx": rss_after_ctx,
            "peak_rss_mb_after_run": rss_after_run,
        }));
        late_arms.push((format!("late_mean_w{w}"), mean_vecs));
        late_arms.push((format!("late_last_w{w}"), last_vec));
    }

    // ── Scoring ─────────────────────────────────────────────────────────
    let mut arms: Vec<(String, &Vec<Vec<f32>>)> = vec![("status_quo".to_string(), &sq_vecs)];
    for (name, vecs) in &late_arms {
        arms.push((name.clone(), vecs));
    }
    let mut table = String::new();
    let mut recall_json = Vec::new();
    writeln!(
        table,
        "{:<20} {:>7} {:>7} {:>7}",
        "arm", "hit@5", "hit@10", "mrr"
    )
    .unwrap();
    for (name, vecs) in &arms {
        let (h5, h10, mrr) = score_arm(&queries, &query_vecs, &chunks, &scored_chunk_idxs, vecs);
        writeln!(table, "{name:<20} {h5:>7.3} {h10:>7.3} {mrr:>7.3}").unwrap();
        recall_json.push(serde_json::json!({
            "arm": name, "hit_at_5": h5, "hit_at_10": h10, "mrr": mrr,
        }));
    }
    println!("\n{table}");

    write_results(
        &out_dir,
        serde_json::json!({
            "binding_verdict": binding,
            "config": {
                "model": args.model.display().to_string(),
                "backend": if args.cpu { "cpu" } else { "metal" },
                "windows": args.windows,
                "smoke": args.smoke,
                "docs": docs.len(),
                "chunks_total": chunks.len(),
                "chunks_scored": scored_chunk_idxs.len(),
                "queries": queries.len(),
            },
            "span_location": {
                "located": located.len(),
                "unlocatable": unlocatable,
                "detok_drift_docs": offset_mismatch_docs,
            },
            "status_quo": {
                "wall_secs": sq_secs,
                "truncated_chunks": truncated,
                "peak_rss_mb_after": rss_after_sq,
            },
            "late_windows": window_reports,
            "recall": recall_json,
            "peak_rss_mb_final": peak_rss_mb(),
        }),
    );
}

struct Doc {
    slug: String,
    text: String,
}
struct ChunkRec {
    slug: String,
    text: String,
}
struct Query {
    #[allow(dead_code)] // fixture shape; kept for future per-query reporting
    qid: String,
    question: String,
    expected: Vec<String>,
}
struct LocatedChunk {
    chunk_idx: usize,
    doc_idx: usize,
    tok_start: usize,
    tok_end: usize,
}

/// Token-level read probe: pooling None on a tiny context. Returns a verdict
/// string starting with "OK" on success.
fn binding_probe(backend: &LlamaBackend, model: &Arc<LlamaModel>, n_embd: usize) -> String {
    let mut ctx = make_ctx(backend, model, 512, 1, Some(LlamaPoolingType::None), false);
    let toks = model
        .str_to_token(
            "The quick brown fox jumps over the lazy dog.",
            AddBos::Always,
        )
        .expect("tokenize probe");
    let mut batch = LlamaBatch::new(512, 1);
    for (i, &t) in toks.iter().enumerate() {
        batch.add(t, i as i32, &[0], true).expect("batch.add");
    }
    if let Err(e) = ctx.decode(&mut batch) {
        return format!("FAIL: decode error under pooling None: {e}");
    }
    let mut norms = Vec::new();
    for i in 0..toks.len() {
        match ctx.embeddings_ith(i as i32) {
            Ok(emb) => {
                if emb.len() != n_embd {
                    return format!(
                        "FAIL: token {i} embedding len {} != n_embd {n_embd}",
                        emb.len()
                    );
                }
                norms.push(emb.iter().map(|&x| (x as f64).powi(2)).sum::<f64>().sqrt());
            }
            Err(e) => {
                return format!(
                    "FAIL: embeddings_ith({i}) error: {e} (0.2.x null-buffer failure mode)"
                )
            }
        }
    }
    let distinct = norms.windows(2).any(|w| (w[0] - w[1]).abs() > 1e-6);
    format!(
        "OK: {} tokens read, norms [{:.3}..{:.3}], distinct={distinct}",
        norms.len(),
        norms.iter().cloned().fold(f64::INFINITY, f64::min),
        norms.iter().cloned().fold(0.0, f64::max),
    )
}

fn make_ctx<'a>(
    backend: &LlamaBackend,
    model: &'a Arc<LlamaModel>,
    n_ctx: u32,
    n_seq_max: u32,
    pooling: Option<LlamaPoolingType>,
    _cpu: bool,
) -> LlamaContext<'a> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8) as i32;
    let mut params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(n_ctx)
        .with_n_ubatch(2048)
        .with_n_seq_max(n_seq_max)
        .with_embeddings(true)
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    // gguf-native pooling (production path) when None is passed: the context
    // default UNSPECIFIED makes libllama read `qwen3.pooling_type` (Last).
    if let Some(p) = pooling {
        params = params.with_pooling_type(p);
    }
    model.new_context(backend, params).expect("new_context")
}

/// Embed one text through the pooled (gguf-native LAST) path, L2-normalized.
fn embed_pooled_one(model: &LlamaModel, ctx: &mut LlamaContext<'_>, text: &str) -> Vec<f32> {
    let mut toks = model.str_to_token(text, AddBos::Always).expect("tokenize");
    if toks.len() > MAX_CHUNK_TOKENS {
        toks.truncate(MAX_CHUNK_TOKENS);
    }
    ctx.clear_kv_cache();
    let mut batch = LlamaBatch::new(16384, 1);
    for (pos, &t) in toks.iter().enumerate() {
        batch.add(t, pos as i32, &[0], true).expect("batch.add");
    }
    ctx.decode(&mut batch).expect("decode pooled");
    l2_normalize(ctx.embeddings_seq_ith(0).expect("pooled embedding"))
}

/// Locate `needle` in `hay` by exact byte match, falling back to a
/// whitespace-collapsed match mapped back to raw byte offsets.
fn locate(hay: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    if let Some(b0) = hay.find(needle) {
        return Some((b0, b0 + needle.len()));
    }
    // Normalized fallback: collapse whitespace runs to a single space on both
    // sides, keeping a norm->raw offset map for the haystack.
    let mut norm = String::with_capacity(hay.len());
    let mut map = Vec::with_capacity(hay.len());
    let mut in_ws = true; // leading whitespace collapses away
    for (bi, ch) in hay.char_indices() {
        if ch.is_whitespace() {
            if !in_ws {
                norm.push(' ');
                map.push(bi);
                in_ws = true;
            }
        } else {
            norm.push(ch);
            // one map entry per byte pushed onto `norm`
            for k in 0..ch.len_utf8() {
                map.push(bi + k);
            }
            in_ws = false;
        }
    }
    let nneedle: String = {
        let mut s = String::with_capacity(needle.len());
        let mut ws = true;
        for ch in needle.chars() {
            if ch.is_whitespace() {
                if !ws {
                    s.push(' ');
                    ws = true;
                }
            } else {
                s.push(ch);
                ws = false;
            }
        }
        s.trim_end().to_string()
    };
    if nneedle.is_empty() {
        return None;
    }
    let n0 = norm.find(&nneedle)?;
    let raw0 = *map.get(n0)?;
    let raw1 = map
        .get(n0 + nneedle.len() - 1)
        .map(|&b| b + 1)
        .unwrap_or(hay.len());
    Some((raw0, raw1))
}

fn score_arm(
    queries: &[Query],
    query_vecs: &[Vec<f32>],
    chunks: &[ChunkRec],
    scored_idxs: &[usize],
    vecs: &[Vec<f32>],
) -> (f64, f64, f64) {
    let (mut h5, mut h10, mut mrr) = (0usize, 0usize, 0.0f64);
    for (q, qv) in queries.iter().zip(query_vecs) {
        let mut scored: Vec<(f32, &str)> = scored_idxs
            .iter()
            .filter(|&&ci| !vecs[ci].is_empty())
            .map(|&ci| (dot(qv, &vecs[ci]), chunks[ci].slug.as_str()))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let expected: std::collections::HashSet<&str> =
            q.expected.iter().map(|s| s.as_str()).collect();
        let first_hit = scored.iter().position(|(_, slug)| expected.contains(slug));
        if let Some(r) = first_hit {
            if r < 5 {
                h5 += 1;
            }
            if r < 10 {
                h10 += 1;
            }
            mrr += 1.0 / (r + 1) as f64;
        }
    }
    let n = queries.len().max(1) as f64;
    (h5 as f64 / n, h10 as f64 / n, mrr / n)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm = v
        .iter()
        .map(|&x| (x as f64).powi(2))
        .sum::<f64>()
        .sqrt()
        .max(1e-12) as f32;
    v.iter().map(|&x| x / norm).collect()
}

/// Peak RSS in MB. macOS ru_maxrss is bytes; Linux is KB.
fn peak_rss_mb() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let raw = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        raw / (1024 * 1024)
    } else {
        raw / 1024
    }
}

fn read_jsonl<T>(path: &PathBuf, f: impl Fn(&serde_json::Value) -> T) -> Vec<T> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| f(&serde_json::from_str(l).expect("parse jsonl row")))
        .collect()
}

fn write_results(out_dir: &PathBuf, v: serde_json::Value) {
    let path = out_dir.join("results.json");
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).expect("write results");
    println!("results written to {}", path.display());
}

struct Args {
    model: PathBuf,
    data_dir: PathBuf,
    out: PathBuf,
    windows: Vec<u32>,
    cpu: bool,
    smoke: bool,
}

impl Args {
    fn parse() -> Self {
        let raw: Vec<String> = std::env::args().collect();
        let mut model = None;
        let mut data_dir = PathBuf::from("research/enrichment-spikes/data");
        let mut out = PathBuf::from("research/enrichment-spikes/runs/sp6");
        let mut windows = vec![8192u32, 16384, 32768];
        let mut cpu = false;
        let mut smoke = false;
        let mut i = 1;
        while i < raw.len() {
            match raw[i].as_str() {
                "--model" => {
                    i += 1;
                    model = raw.get(i).map(PathBuf::from);
                }
                "--data-dir" => {
                    i += 1;
                    data_dir = PathBuf::from(&raw[i]);
                }
                "--out" => {
                    i += 1;
                    out = PathBuf::from(&raw[i]);
                }
                "--windows" => {
                    i += 1;
                    windows = raw[i]
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                }
                "--backend" => {
                    i += 1;
                    cpu = raw[i] == "cpu";
                }
                "--smoke" => smoke = true,
                other => {
                    eprintln!("unknown arg: {other}");
                    std::process::exit(2);
                }
            }
            i += 1;
        }
        let model = model.unwrap_or_else(|| {
            eprintln!("error: --model <path.gguf> is required");
            std::process::exit(2);
        });
        Args {
            model,
            data_dir,
            out,
            windows,
            cpu,
            smoke,
        }
    }
}
