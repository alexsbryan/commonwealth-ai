//! Bench runner for the notes_tiered T1 surface.
//!
//! Loads the fixture under `sovereign/bench/notes_tiered/`, opens
//! a fresh NoteStore in a temp dir, writes every fixture note,
//! then runs each query under two paths:
//!
//!   - `baseline`: `read_notes_scoped` (FTS5 BM25 only)
//!   - `semantic`: `read_notes_scoped_semantic` with the daemon's
//!     embed slot (or a stub when `--no-daemon`)
//!
//! Scoring: `hit@k` per query against the fixture's
//! `expected_hits`. Reports per-failure-class aggregates so the
//! bench answers "does T1 fix the synonym/stem/paraphrase miss
//! classes?" — the original audit's 3/8 hit rate is the regression
//! guard baseline.
//!
//! Usage:
//!   cargo run --release --example notes_tiered_bench
//!   cargo run --release --example notes_tiered_bench -- --no-daemon
//!   cargo run --release --example notes_tiered_bench -- \
//!       --daemon http://127.0.0.1:9741 \
//!       --baseline sovereign/bench/notes_tiered/baselines/notes-tiered/latest.json
//!
//! With `--no-daemon`, T1 disables and the runner only reports the
//! baseline path — useful for CI regression guards on FTS5
//! behaviour. The semantic numbers populate when a daemon's embed
//! slot is reachable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use corpus_engine_notes::{EmbedFn, NoteScope, NoteSource, NoteStore, ScopeFilter};
use serde::Deserialize;

#[derive(Parser, Debug)]
#[command(about = "T1 retrieval bench for NoteStore")]
struct Args {
    /// Path to notes fixture TOML.
    #[arg(
        long,
        default_value = "sovereign/bench/notes_tiered/fixtures/notes.toml"
    )]
    notes: PathBuf,

    /// Path to query fixture TOML.
    #[arg(
        long,
        default_value = "sovereign/bench/notes_tiered/fixtures/queries.toml"
    )]
    queries: PathBuf,

    /// Daemon URL for the embed slot. Empty / --no-daemon disables T1.
    #[arg(long, default_value = "http://127.0.0.1:9741")]
    daemon: String,

    /// Disable the T1 path entirely; report baseline only.
    #[arg(long)]
    no_daemon: bool,

    /// Blend weight passed to read_notes_scoped_semantic.
    #[arg(long, default_value_t = 0.5)]
    embed_weight: f32,

    /// Embed model id to advertise to the daemon. Default matches
    /// the daemon's default fast-slot embed model.
    #[arg(long, default_value = "qwen-embedding-0.6b")]
    embed_model: String,

    /// Where to write the JSON report. Default: stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct NotesFixture {
    #[serde(default, rename = "bank")]
    _bank: Option<toml::Value>,
    notes: Vec<FixtureNote>,
}

#[derive(Debug, Deserialize, Clone)]
struct FixtureNote {
    id: String,
    kind: String,
    content: String,
    #[serde(default)]
    symbols: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct QueriesFixture {
    bank: QueriesBank,
    queries: Vec<FixtureQuery>,
}

#[derive(Debug, Deserialize)]
struct QueriesBank {
    #[allow(dead_code)]
    name: String,
    target_k: usize,
}

#[derive(Debug, Deserialize, Clone)]
struct FixtureQuery {
    id: String,
    query: String,
    expected_hits: Vec<String>,
    audit_baseline: usize,
    failure_class: String,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, serde::Serialize)]
struct QueryResult {
    id: String,
    query: String,
    failure_class: String,
    expected: Vec<String>,
    baseline_hits: Vec<String>,
    semantic_hits: Vec<String>,
    audit_baseline: usize,
    /// `baseline_hits` ∩ `expected`.
    baseline_correct: usize,
    /// `semantic_hits` ∩ `expected`.
    semantic_correct: usize,
    delta_vs_audit: i64,
    baseline_latency_ms: f64,
    semantic_latency_ms: f64,
}

#[derive(Debug, serde::Serialize)]
struct BenchReport {
    target_k: usize,
    embed_weight: f32,
    daemon_url: String,
    daemon_reachable: bool,
    total_queries: usize,
    audit_baseline_total_correct: usize,
    baseline_total_correct: usize,
    semantic_total_correct: usize,
    by_failure_class: BTreeMap<String, ClassAggregate>,
    queries: Vec<QueryResult>,
}

#[derive(Debug, Default, serde::Serialize)]
struct ClassAggregate {
    queries: usize,
    expected_total: usize,
    baseline_correct: usize,
    semantic_correct: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let notes_text = std::fs::read_to_string(&args.notes)?;
    let queries_text = std::fs::read_to_string(&args.queries)?;
    let notes_fix: NotesFixture = toml::from_str(&notes_text)?;
    let queries_fix: QueriesFixture = toml::from_str(&queries_text)?;
    let target_k = queries_fix.bank.target_k;

    // Build an id→fixture-id lookup so we can map NoteStore's
    // UUIDs back to the human-readable fixture ids ("n1", "n3"…)
    // for scoring.
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("notes.db");
    let store = NoteStore::open(&db_path)?;
    let (store, daemon_reachable) = if !args.no_daemon {
        match build_daemon_embed_fn(&args.daemon, &args.embed_model).await {
            Ok(embed_fn) => (store.with_embed_fn(embed_fn), true),
            Err(e) => {
                eprintln!(
                    "warning: daemon embed not reachable at {} ({e}); falling back to baseline-only",
                    args.daemon
                );
                (store, false)
            }
        }
    } else {
        (store, false)
    };

    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    for note in &notes_fix.notes {
        let real_id = store
            .write_note_full(
                &note.kind,
                &note.content,
                note.symbols.clone(),
                note.files.clone(),
                &note.session_id,
                NoteScope::Global,
                None,
                None,
                NoteSource::Agent,
                None,
                None,
            )
            .await?;
        id_map.insert(real_id, note.id.clone());
    }

    let mut report = BenchReport {
        target_k,
        embed_weight: args.embed_weight,
        daemon_url: args.daemon.clone(),
        daemon_reachable,
        total_queries: queries_fix.queries.len(),
        audit_baseline_total_correct: queries_fix.queries.iter().map(|q| q.audit_baseline).sum(),
        baseline_total_correct: 0,
        semantic_total_correct: 0,
        by_failure_class: BTreeMap::new(),
        queries: Vec::new(),
    };

    // Set the env var the semantic path reads.
    // SAFETY: example binary is single-threaded at this point.
    unsafe {
        std::env::set_var(
            "SOVEREIGN_NOTES_EMBED_WEIGHT",
            args.embed_weight.to_string(),
        );
    }

    for q in &queries_fix.queries {
        let baseline_start = Instant::now();
        let baseline = store
            .read_notes_scoped(
                Some(&q.query),
                &[],
                &[],
                &[],
                target_k,
                false,
                &ScopeFilter::default(),
            )
            .await?;
        let baseline_latency_ms = baseline_start.elapsed().as_secs_f64() * 1000.0;

        let baseline_hits: Vec<String> = baseline
            .iter()
            .filter_map(|n| id_map.get(&n.id).cloned())
            .collect();

        let (semantic_hits, semantic_latency_ms) = if daemon_reachable {
            let start = Instant::now();
            let rows = store
                .read_notes_scoped_semantic(
                    Some(&q.query),
                    &[],
                    &[],
                    &[],
                    target_k,
                    false,
                    &ScopeFilter::default(),
                    Some(&q.query),
                )
                .await?;
            let lat = start.elapsed().as_secs_f64() * 1000.0;
            (
                rows.iter()
                    .filter_map(|n| id_map.get(&n.id).cloned())
                    .collect::<Vec<_>>(),
                lat,
            )
        } else {
            (Vec::new(), 0.0)
        };

        let baseline_correct = q
            .expected_hits
            .iter()
            .filter(|h| baseline_hits.contains(h))
            .count();
        let semantic_correct = q
            .expected_hits
            .iter()
            .filter(|h| semantic_hits.contains(h))
            .count();

        report.baseline_total_correct += baseline_correct;
        report.semantic_total_correct += semantic_correct;

        let agg = report
            .by_failure_class
            .entry(q.failure_class.clone())
            .or_default();
        agg.queries += 1;
        agg.expected_total += q.expected_hits.len();
        agg.baseline_correct += baseline_correct;
        agg.semantic_correct += semantic_correct;

        let delta = baseline_correct as i64 - q.audit_baseline as i64;
        report.queries.push(QueryResult {
            id: q.id.clone(),
            query: q.query.clone(),
            failure_class: q.failure_class.clone(),
            expected: q.expected_hits.clone(),
            baseline_hits,
            semantic_hits,
            audit_baseline: q.audit_baseline,
            baseline_correct,
            semantic_correct,
            delta_vs_audit: delta,
            baseline_latency_ms,
            semantic_latency_ms,
        });

        if !q.notes.is_empty() {
            // Echo the notes column for the operator reading stderr.
            eprintln!("  [{}] {}", q.id, q.notes);
        }
    }

    print_summary(&report);

    if let Some(out) = args.out {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, serde_json::to_string_pretty(&report)?)?;
        eprintln!("wrote {}", out.display());
    }

    Ok(())
}

fn print_summary(report: &BenchReport) {
    println!();
    println!("notes_tiered bench — k={}", report.target_k);
    println!(
        "  daemon: {} ({})",
        report.daemon_url,
        if report.daemon_reachable {
            "reachable"
        } else {
            "unreachable / baseline-only"
        }
    );
    println!("  embed_weight: {:.2}", report.embed_weight);
    println!();
    let expected_total: usize = report.queries.iter().map(|q| q.expected.len()).sum();
    println!(
        "  audit baseline (recorded 2026-05-25): {} / {} hits",
        report.audit_baseline_total_correct, expected_total
    );
    println!(
        "  this run baseline (FTS5 only):        {} / {} hits",
        report.baseline_total_correct, expected_total
    );
    if report.daemon_reachable {
        println!(
            "  this run semantic (T1 blend, w={:.2}): {} / {} hits  (delta {:+})",
            report.embed_weight,
            report.semantic_total_correct,
            expected_total,
            report.semantic_total_correct as i64 - report.baseline_total_correct as i64,
        );
    }
    println!();
    println!("  by failure_class:");
    for (cls, agg) in &report.by_failure_class {
        if report.daemon_reachable {
            println!(
                "    {:14} baseline {}/{}   semantic {}/{}",
                cls,
                agg.baseline_correct,
                agg.expected_total,
                agg.semantic_correct,
                agg.expected_total
            );
        } else {
            println!(
                "    {:14} baseline {}/{}",
                cls, agg.baseline_correct, agg.expected_total
            );
        }
    }
    println!();
    println!("  per-query:");
    for q in &report.queries {
        let semantic_str = if report.daemon_reachable {
            format!("  T1 {}/{}", q.semantic_correct, q.expected.len())
        } else {
            String::new()
        };
        println!(
            "    {:30} [{:11}] baseline {}/{}{}  ({:.1}ms / {:.1}ms)",
            q.id,
            q.failure_class,
            q.baseline_correct,
            q.expected.len(),
            semantic_str,
            q.baseline_latency_ms,
            q.semantic_latency_ms,
        );
    }
    println!();
}

/// Build an [`EmbedFn`] backed by the daemon's `/v1/embeddings`
/// endpoint. Returns Err if the daemon isn't reachable so the
/// caller can fall back to baseline-only mode.
async fn build_daemon_embed_fn(
    daemon_url: &str,
    model: &str,
) -> Result<EmbedFn, Box<dyn std::error::Error>> {
    let probe = format!("{}/v1/models", daemon_url);
    let resp = reqwest::Client::new()
        .get(&probe)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(format!("daemon /v1/models → HTTP {}", resp.status()).into());
    }

    let url = format!("{}/v1/embeddings", daemon_url);
    let model = model.to_string();
    let url_for_closure = url.clone();
    let model_for_closure = model.clone();
    let embed: EmbedFn = Arc::new(move |text: &str| {
        let url = url_for_closure.clone();
        let model = model_for_closure.clone();
        let input = text.to_string();
        Box::pin(async move {
            let resp = reqwest::Client::new()
                .post(&url)
                .json(&serde_json::json!({
                    "model": model,
                    "input": input,
                }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| {
                    corpus_engine_notes::Error::Io(std::io::Error::other(format!(
                        "daemon embed: {e}"
                    )))
                })?;
            if !resp.status().is_success() {
                return Err(corpus_engine_notes::Error::Io(std::io::Error::other(
                    format!("daemon embed → HTTP {}", resp.status()),
                )));
            }
            let body: serde_json::Value = resp.json().await.map_err(|e| {
                corpus_engine_notes::Error::Io(std::io::Error::other(format!(
                    "daemon embed parse: {e}"
                )))
            })?;
            let vec = body
                .get("data")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("embedding"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect::<Vec<f32>>()
                })
                .ok_or_else(|| {
                    corpus_engine_notes::Error::Io(std::io::Error::other(
                        "daemon embed: no embedding in response",
                    ))
                })?;
            Ok(vec)
        })
    });
    Ok(embed)
}
