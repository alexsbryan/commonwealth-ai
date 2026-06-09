// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign bench book-report` — attach-document benchmark.
//!
//! See `sovereign/bench/book-report/README.md` for the full design.
//!
//! v1 scope (this file): stages 1-3 only — fetch the Gutenberg book,
//! attach it via `DocumentAssetManager::ingest()`, record per-phase
//! state-transition timings. Question dispatch + scoring land in v1.1.
//!
//! Direct mode (default) builds a Runtime in the bench process using
//! the same `chat_cmd::bootstrap::build_session` path the chat REPL
//! uses — `SplitInferenceProvider` delegates inference to the running
//! daemon over HTTP, but `DocumentAssetManager` runs in-process here
//! so we measure the ingest pipeline without OICP wire overhead on
//! the orchestration calls. The model itself is whichever primary the
//! daemon was started with.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::FutureExt;
use serde::{Deserialize, Serialize};
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::{
    CompletionRequest, DocumentAsset, DocumentSession, Message, NarrationEvent, Role, Speed,
};
use sovereign_tools::document_asset::{DocumentAssetManager, IngestProgress};

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::default_globals_for_voice_eval;
use sovereign_cli_shared::help::{self, Help, HelpSection};

/// Bench configuration baked in at compile time. Changing the questions
/// requires rebuilding the CLI; that's intentional — the bench is
/// versioned with the codebase, not authored at runtime.
const BENCH_TOML: &str = include_str!("../../../../bench/book-report/bench.toml");

/// Gutenberg URL for The Secret Agent (book id 974). Pinned to the
/// canonical UTF-8 plaintext mirror; the SHA-256 in `bench.toml` locks
/// the content version once first run completes.
const GUTENBERG_URL: &str = "https://www.gutenberg.org/cache/epub/974/pg974.txt";

const HELP: Help = Help {
    command: "sovereign bench book-report",
    summary: "Attach-document benchmark on Conrad's The Secret Agent. Fetch → attach → state-stream → tier dispatch → mechanical + LLM-judge scoring.",
    sections: &[
        HelpSection::Usage("sovereign bench book-report [--reuse-asset <id>] [--rebuild-skeleton] [--rebuild-raptor] [--tier <N>] [--questions <ids>] [--list-assets] [--cache-dir <path>] [--output <path>] [--refresh-source]"),
        HelpSection::Flags(&[
            (
                "--reuse-asset <id>",
                "Skip fetch + ingest; reuse an existing DocumentAsset from the daemon's store. \
                 Big iteration speedup — turns a ~25-min full run into ~5 min. The reused asset \
                 must already be Ready. Find candidates with --list-assets.",
            ),
            (
                "--list-assets",
                "Print every DocumentAsset in the daemon's store (id, title, state) and exit. \
                 Useful for picking an id to pass to --reuse-asset.",
            ),
            (
                "--tier <N>",
                "Run only the questions in the given tier (1-5). Composable with --reuse-asset \
                 for sub-minute iteration on a single tier's judge prompt.",
            ),
            (
                "--questions <id,id,...>",
                "Run only the questions whose id matches one of the comma-separated values \
                 (exact match, case-sensitive). Composable with --reuse-asset.",
            ),
            (
                "--cache-dir <path>",
                "Where to store the downloaded Gutenberg text. Default: ~/.sovereign/bench-cache/book-report.",
            ),
            (
                "--output <path>",
                "Write the structured timings JSON to this path in addition to stdout. \
                 Default: ~/.sovereign/bench-runs/book-report/<ts>/timings.json.",
            ),
            (
                "--refresh-source",
                "Force re-download of the Gutenberg text even if cached. Use when the bench.toml \
                 SHA-256 pin is being updated.",
            ),
        ]),
        HelpSection::Notes(
            "Requires a running daemon at the configured client port. The bench process builds its \
             own Runtime that delegates inference to the daemon but runs DocumentAssetManager \
             in-process — same code path the desktop's Attach mode uses, without the wire overhead \
             on orchestration calls. Wire mode (--wire) is not yet implemented.",
        ),
    ],
};

/// One recorded state transition. The bench renders these as a timeline
/// in the report so the team can see "ingest got to PartiallyReady at
/// 4.2s, BuildingSkeleton at 18s, Ready at 47s" at a glance.
#[derive(Debug, Clone, Serialize)]
pub struct StateTransition {
    /// Milliseconds since `attach_at`.
    pub ms_since_attach: u64,
    /// One of: started, indexing, chunk_indexed, partially_ready,
    /// skeleton_building, skeleton_chunk_processed, ready, failed.
    pub phase: String,
    /// Free-form per-phase detail (chunk counts, durations, etc.).
    pub detail: serde_json::Value,
}

/// Bench output. v1.1 adds Tier-1 question results + mechanical scores.
/// LLM-judge for Tier 2-5 lands in v1.3.
#[derive(Debug, Clone, Serialize)]
pub struct BookReportRun {
    pub bench_id: String,
    pub started_at_unix: u64,
    pub source: SourceInfo,
    pub asset_id: String,
    pub chat_model: Option<String>,
    /// Wall-clock from CLI start to ingest call return.
    pub attach_ms: u64,
    /// Wall-clock from attach to first RagAvailable transition.
    pub time_to_rag_ready_ms: Option<u64>,
    /// Wall-clock from attach to terminal Ready transition.
    pub time_to_ready_ms: Option<u64>,
    pub transitions: Vec<StateTransition>,
    pub terminated_at_phase: String,
    /// Per-question results — v1.1 ships Tier 1 only (mechanical scoring).
    pub questions: Vec<QuestionResult>,
    /// Aggregate rollup for v1.1 — Tier 1 mean score + mean latency.
    pub tier_summary: Vec<TierSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub kind: &'static str,
    pub url: String,
    pub local_path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// Subset of the bench.toml schema the runner currently consumes. Other
/// fields (`[bench.source]`, `latency_budget`, `reference_passages`,
/// `contamination_traps`) round-trip through `serde(default)` so the
/// runner doesn't crash on TOML it doesn't yet understand.
#[derive(Debug, Deserialize)]
struct BenchConfig {
    #[serde(default)]
    questions: Vec<Question>,
}

#[derive(Debug, Deserialize, Clone)]
struct Question {
    id: String,
    tier: u8,
    prompt: String,
    #[serde(default)]
    expected_facts: Vec<String>,
    /// Verified passages from pg974.txt. Used both to anchor the
    /// LLM-judge prompt and to give the operator something to compare
    /// the model's answer against in the rendered report.
    #[serde(default)]
    reference_passages: Vec<RefPassage>,
    /// Tier-5 contamination flags — paraphrased canonical-reception
    /// pitfalls. When present, the LLM-judge prompt warns the grader
    /// to dock score if the answer leans on these without textual
    /// anchor.
    #[serde(default)]
    critique_traps: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct RefPassage {
    /// Line range in pg974.txt, formatted "start" or "start-end"
    /// (1-indexed, inclusive).
    lines: String,
    #[serde(default)]
    note: Option<String>,
}

/// Per-question result. v1.1 captures the mechanical score; the
/// LLM-judge fields (`judge_score`, `judge_rationale`,
/// `hallucinated_quotes`) are reserved and emitted as `null` until
/// v1.3 wires them.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionResult {
    pub id: String,
    pub tier: u8,
    pub prompt: String,
    /// Skipped reasons: ingest failed before this question's gate, or
    /// the question's tier is above v1.1's coverage (Tier 2-5). When
    /// skipped, the response/latency/score fields are populated with
    /// defaults and `skipped_reason` carries the cause.
    pub skipped_reason: Option<String>,
    pub response: String,
    /// Comma-joined list of tool IDs the runtime actually invoked
    /// during the turn (`attached_doc_search`,
    /// `attached_doc_search, knowledge_lookup`, etc.), or `Runtime`
    /// when the handler did its own retrieval without going through
    /// a Tool. Derived from the narration log at dispatch time.
    pub operation: String,
    pub sources_count: usize,
    pub latency_ms: u64,
    pub expected_facts: Vec<String>,
    pub facts_hit: Vec<String>,
    pub facts_missed: Vec<String>,
    /// Whole-percent score: facts_hit.len() / expected_facts.len() * 100.
    pub mechanical_score_pct: u8,
    /// v1.3 — placeholders until LLM-judge ships.
    pub judge_score: Option<u8>,
    pub judge_rationale: Option<String>,
    pub hallucinated_quotes: Option<Vec<String>>,
    /// Runtime narration log captured during dispatch — the
    /// load-bearing diagnostic for which tools the model invoked.
    /// Now populated on every question since the bench dispatches
    /// uniformly through `runtime.handle_turn`; the per-tool
    /// `ToolInvocationStart` / `ToolInvocationComplete` phases are
    /// what `derive_operation_label` reads to produce the
    /// per-row `operation` summary.
    pub narration_log: Vec<NarrationEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierSummary {
    pub tier: u8,
    pub questions_run: usize,
    pub questions_skipped: usize,
    /// Mean of `mechanical_score_pct` across the tier's run questions.
    /// Authoritative for Tier 1; advisory for Tier 2-5 (the LLM-judge
    /// score is the load-bearing number above Tier 1).
    pub mean_mechanical_score_pct: u8,
    /// Mean of `judge_score` for Tier 2-5 questions whose judge ran
    /// successfully. `None` when no judge scores are present (Tier 1,
    /// or all judge calls errored).
    pub mean_judge_score: Option<f32>,
    pub mean_latency_ms: u64,
    /// Number of questions in this tier that had at least one
    /// fabricated quote detected. A non-zero value here is the bench's
    /// loudest quality alarm — escalate before tuning anything else.
    pub hallucination_flag_count: usize,
}

pub async fn cmd_book_report(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help::print(&HELP);
        return 0;
    }

    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    if opts.wire {
        eprintln!("error: --wire mode is not yet implemented. v1 ships direct in-process only.");
        eprintln!("       See sovereign/bench/book-report/README.md for the wire-mode roadmap.");
        return 2;
    }

    match run(opts).await {
        Ok(report) => {
            print_summary(&report);
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[derive(Debug)]
struct Opts {
    cache_dir: PathBuf,
    output: Option<PathBuf>,
    refresh_source: bool,
    wire: bool,
    /// Skip fetch + ingest; look up the asset by id and dispatch
    /// questions against it. Reduces ~25-min runs to ~5 min.
    reuse_asset: Option<String>,
    /// Print existing assets in the daemon's store and exit.
    list_assets: bool,
    /// Filter questions to a single tier (1-5).
    tier: Option<u8>,
    /// Filter questions to a comma-separated id list.
    question_ids: Option<Vec<String>>,
    /// When set alongside `--reuse-asset`, rebuild the asset's
    /// skeleton (entity_index, structural_moments, action atoms)
    /// from its stored chunks before firing questions. Useful when
    /// the original skeleton was built with a smaller model and
    /// missed entities the bench depends on.
    rebuild_skeleton: bool,
    /// When set alongside `--reuse-asset`, rebuild the RAPTOR atlas
    /// + motif index for the asset before firing questions. Skips
    /// the legacy skeleton rebuild, so it's the fast path for
    /// populating the new atlas on assets ingested before the
    /// RAPTOR pipeline shipped (~2-3 min vs ~20 min for full
    /// re-ingest).
    rebuild_raptor: bool,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut cache_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut refresh_source = false;
    let mut wire = false;
    let mut reuse_asset: Option<String> = None;
    let mut list_assets = false;
    let mut tier: Option<u8> = None;
    let mut question_ids: Option<Vec<String>> = None;
    let mut rebuild_skeleton = false;
    let mut rebuild_raptor = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cache-dir" => {
                i += 1;
                cache_dir = Some(PathBuf::from(
                    args.get(i).ok_or("--cache-dir requires a path")?,
                ));
            }
            "--output" => {
                i += 1;
                output = Some(PathBuf::from(
                    args.get(i).ok_or("--output requires a path")?,
                ));
            }
            "--reuse-asset" => {
                i += 1;
                reuse_asset = Some(args.get(i).ok_or("--reuse-asset requires an id")?.clone());
            }
            "--list-assets" => list_assets = true,
            "--tier" => {
                i += 1;
                let n: u8 = args
                    .get(i)
                    .ok_or("--tier requires a number")?
                    .parse()
                    .map_err(|e| format!("--tier: {e}"))?;
                if !(1..=5).contains(&n) {
                    return Err(format!("--tier must be 1..=5, got {n}"));
                }
                tier = Some(n);
            }
            "--questions" => {
                i += 1;
                let raw = args.get(i).ok_or("--questions requires a comma list")?;
                let ids: Vec<String> = raw
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if ids.is_empty() {
                    return Err("--questions requires at least one id".to_string());
                }
                question_ids = Some(ids);
            }
            "--refresh-source" => refresh_source = true,
            "--wire" => wire = true,
            "--rebuild-skeleton" => rebuild_skeleton = true,
            "--rebuild-raptor" => rebuild_raptor = true,
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    let cache_dir = cache_dir.unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".sovereign/bench-cache/book-report")
    });
    Ok(Opts {
        cache_dir,
        output,
        refresh_source,
        wire,
        reuse_asset,
        list_assets,
        tier,
        question_ids,
        rebuild_skeleton,
        rebuild_raptor,
    })
}

async fn run(opts: Opts) -> Result<BookReportRun, String> {
    let started_at = chrono::Utc::now();
    let bench_id = format!("book-report-{}", started_at.format("%Y%m%dT%H%M%S"));

    // ── Stage 1: fetch ─────────────────────────────────────────
    // Always run — even in --reuse-asset mode the source text is
    // needed for hallucination detection + reference-passage
    // resolution. Cache hit makes this ~10ms when warm.
    eprintln!("[1/3] fetch — Gutenberg #974 (The Secret Agent)");
    let source = fetch_source(&opts.cache_dir, opts.refresh_source)
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    eprintln!(
        "      cached at {} ({} bytes, sha256={}…)",
        source.local_path.display(),
        source.bytes,
        &source.sha256[..16],
    );
    let source_text = std::fs::read_to_string(&source.local_path)
        .map_err(|e| format!("read source {}: {e}", source.local_path.display()))?;

    // ── Bootstrap: build the Runtime that talks to the daemon ──
    eprintln!("[2/3] bootstrap — connecting to daemon, building DocumentAssetManager");
    let globals = default_globals_for_voice_eval();
    let session = build_session(&globals)
        .await
        .map_err(|e| format!("daemon bootstrap failed: {e}. Is the daemon running?"))?;
    let manager =
        DocumentAssetManager::new(Arc::clone(&session.inference), Arc::clone(&session.store));
    let chat_model = Some(session.inference.model_id_for(Speed::Slow));

    // ── --list-assets exits here ───────────────────────────────
    if opts.list_assets {
        list_assets(session.store.as_ref()).await?;
        // Return a stub report so the caller's pattern still works.
        // The CLI treats list-assets as a success diagnostic, not a
        // bench run, so persistence is skipped.
        return Ok(stub_report(
            bench_id,
            started_at.timestamp() as u64,
            source,
            chat_model,
        ));
    }

    // ── Stage 3: attach OR reuse ───────────────────────────────
    let (asset, attach_ms, transitions_vec, terminal_phase) = if let Some(reuse_id) =
        &opts.reuse_asset
    {
        eprintln!("[3/3] reuse — looking up existing asset {reuse_id}");
        match session.store.get_document_asset(reuse_id).await {
            Ok(Some(found)) => {
                eprintln!(
                    "      found: title=\"{}\" state={:?}",
                    found.title, found.state
                );
                let asset_to_use = if opts.rebuild_skeleton {
                    eprintln!("      --rebuild-skeleton: re-running skeleton extraction (uses current build_skeleton speed)");
                    let rebuild_start = std::time::Instant::now();
                    match manager.rebuild_skeleton(reuse_id).await {
                        Ok(new_skeleton) => {
                            let secs = rebuild_start.elapsed().as_secs();
                            eprintln!(
                                "      rebuild ok in {secs}s: {} entities, {} moments, {} actions",
                                new_skeleton.main_entities.len(),
                                new_skeleton.structural_moments.len(),
                                new_skeleton.actions.len(),
                            );
                            // Reload the asset to pick up the new skeleton.
                            match session.store.get_document_asset(reuse_id).await {
                                Ok(Some(refreshed)) => refreshed,
                                _ => found,
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "      rebuild_skeleton failed: {e}; using existing skeleton"
                            );
                            found
                        }
                    }
                } else {
                    found
                };
                if opts.rebuild_raptor {
                    eprintln!("      --rebuild-raptor: populating RAPTOR atlas + motif index on the existing asset");
                    let raptor_start = std::time::Instant::now();
                    match manager.rebuild_raptor_atlas(reuse_id).await {
                        Ok(()) => {
                            let secs = raptor_start.elapsed().as_secs();
                            let node_count = session
                                .store
                                .list_raptor_nodes(reuse_id)
                                .await
                                .map(|v| v.len())
                                .unwrap_or(0);
                            let motif_count = session
                                .store
                                .list_asset_motifs(reuse_id)
                                .await
                                .map(|v| v.iter().filter(|m| m.is_distinctive).count())
                                .unwrap_or(0);
                            eprintln!(
                                    "      raptor rebuild ok in {secs}s: {node_count} nodes, {motif_count} distinctive motifs"
                                );
                        }
                        Err(e) => {
                            eprintln!("      rebuild_raptor_atlas failed: {e}; continuing without RAPTOR data");
                        }
                    }
                }
                (
                    Some(asset_to_use),
                    0u64,
                    vec![StateTransition {
                        ms_since_attach: 0,
                        phase: "reused".to_string(),
                        detail: serde_json::json!({ "asset_id": reuse_id }),
                    }],
                    "reused".to_string(),
                )
            }
            Ok(None) => {
                return Err(format!(
                    "no asset with id {reuse_id} in the daemon's store. Try --list-assets."
                ))
            }
            Err(e) => return Err(format!("lookup asset {reuse_id}: {e}")),
        }
    } else {
        attach_and_stream(&manager, &source).await
    };
    let asset_id = asset.as_ref().map(|a| a.id.clone()).unwrap_or_default();

    let time_to_rag_ready_ms = transitions_vec
        .iter()
        .find(|t| t.phase == "rag_available")
        .map(|t| t.ms_since_attach);
    let time_to_ready_ms = transitions_vec
        .iter()
        .find(|t| t.phase == "ready")
        .map(|t| t.ms_since_attach);

    // ── Stage 4-5: parse bench.toml, fire questions, score ────
    let bench_cfg: BenchConfig =
        toml::from_str(BENCH_TOML).map_err(|e| format!("parse embedded bench.toml: {e}"))?;
    let filtered_questions = filter_questions(
        &bench_cfg.questions,
        opts.tier,
        opts.question_ids.as_deref(),
    );
    eprintln!(
        "      bench has {} question(s); {} match filters",
        bench_cfg.questions.len(),
        filtered_questions.len(),
    );
    let (questions, tier_summary) = run_questions(
        Arc::clone(&session.runtime),
        Arc::clone(&session.store),
        Arc::clone(&session.inference),
        asset.as_ref(),
        &source_text,
        &filtered_questions,
    )
    .await;

    let report = BookReportRun {
        bench_id,
        started_at_unix: started_at.timestamp() as u64,
        source,
        asset_id,
        chat_model,
        attach_ms,
        time_to_rag_ready_ms,
        time_to_ready_ms,
        transitions: transitions_vec,
        terminated_at_phase: terminal_phase,
        questions,
        tier_summary,
    };

    persist_report(&report, opts.output.as_deref()).map_err(|e| format!("persist report: {e}"))?;
    Ok(report)
}

/// Fire every question in the bank, score each one.
///
/// **Routing.** Every question goes through `runtime.handle_turn`. The
/// runtime composes with its registered tool catalog — including
/// `attached_doc_search`, which resolves the most-recently-ingested
/// Ready document from the store. This replaced the pre-2026-05-20
/// stale path that ran a parallel `DocumentAssetManager::route` →
/// `manager.ask` pipeline gated by the document's own router; the
/// book-report bench surfaced that the parallel router mis-routed
/// Tier-1 factual questions as `OffTopic`, sending them to the
/// general corpus when the answer was sitting in the attached text.
///
/// The `operation` field on each result is derived from the
/// narration log — the unique tool IDs the runtime invoked during
/// the turn. Tells the operator at a glance whether the model
/// reached for `attached_doc_search`, the corpus, or neither.
///
/// **Scoring (per tier).**
/// - Tier 1: mechanical only — substring match of `expected_facts`.
/// - Tier 2-5: LLM-judge against the rubric, with reference passages
///   from pg974.txt resolved by `[reference_passages]` line ranges
///   so the grader has ground truth in front of it.
/// - All tiers: hallucination detection — any quoted passage in the
///   answer (≥30 chars inside double quotes) is substring-checked
///   against the cached source text. Mismatches surface in the
///   report as `hallucinated_quotes`.
async fn run_questions(
    runtime: Arc<Runtime>,
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
    asset: Option<&DocumentAsset>,
    source_text: &str,
    questions: &[Question],
) -> (Vec<QuestionResult>, Vec<TierSummary>) {
    let mut results: Vec<QuestionResult> = Vec::with_capacity(questions.len());

    if questions.is_empty() {
        eprintln!("[4/4] questions — bench.toml has no [[questions]] entries");
        return (results, Vec::new());
    }

    eprintln!(
        "[4/4] questions — firing {} question(s) across {} tier(s)",
        questions.len(),
        questions
            .iter()
            .map(|q| q.tier)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
    );

    for q in questions {
        let asset_ref = match asset {
            Some(a) => a,
            None => {
                results.push(skipped(q, "ingest_failed"));
                continue;
            }
        };

        eprintln!("      [{}] T{} {}", q.id, q.tier, truncate(&q.prompt, 60));
        let q_start = Instant::now();
        // catch_unwind so a panic in the runtime doesn't lose every
        // prior question's data. AssertUnwindSafe is justified because
        // the future only holds Arc<dyn> refs + borrowed args — no
        // shared interior mutability that could observe a
        // half-poisoned state. On panic we record a dispatch_err and
        // continue with the next question.
        let dispatch_future =
            AssertUnwindSafe(dispatch_question(&runtime, &store, asset_ref, &q.prompt));
        let dispatch = match dispatch_future.catch_unwind().await {
            Ok(r) => r,
            Err(payload) => {
                let msg = panic_payload_to_string(&payload);
                eprintln!("        → panic during dispatch: {msg}");
                Err(format!("panic: {msg}"))
            }
        };
        let dispatch_ms = q_start.elapsed().as_millis() as u64;

        let (response, operation, sources_count, narration_log, dispatch_err) = match dispatch {
            Ok(ans) => (
                ans.text,
                ans.operation,
                ans.sources_count,
                ans.narration_log,
                None,
            ),
            Err(e) => {
                eprintln!("        → dispatch error: {e}");
                (String::new(), String::new(), 0, Vec::new(), Some(e))
            }
        };

        // Mechanical scoring on expected_facts — always runs even on
        // Tier 2-5 because the substring hit/miss is data the operator
        // wants to see alongside the judge score.
        let (facts_hit, facts_missed, mechanical_score_pct) = score_question(q, &response);

        // Hallucination detection — applies to all tiers because any
        // quoted passage in an answer is verifiable against the source.
        let hallucinated_quotes = if response.is_empty() {
            Vec::new()
        } else {
            detect_hallucinations(&response, source_text)
        };

        // LLM-judge for Tier 2-5. Tier 1's mechanical score is already
        // the load-bearing number; running the judge there adds cost
        // without signal. Tier 5 carries critique_traps which the
        // judge prompt surfaces to dock contamination-flavoured
        // answers explicitly.
        let (judge_score, judge_rationale) = if q.tier >= 2 && !response.is_empty() {
            let resolved_refs = resolve_reference_passages(&q.reference_passages, source_text);
            match run_llm_judge(
                inference.as_ref(),
                q,
                &response,
                &resolved_refs,
                &hallucinated_quotes,
            )
            .await
            {
                Ok(j) => (Some(j.score), Some(j.rationale)),
                Err(e) => (None, Some(format!("judge_error: {e}"))),
            }
        } else {
            (None, None)
        };

        let display_score = if q.tier == 1 {
            format!("{}%", mechanical_score_pct)
        } else {
            judge_score
                .map(|s| format!("{}/5", s))
                .unwrap_or_else(|| "—".to_string())
        };
        let hallu_chip = if hallucinated_quotes.is_empty() {
            "".to_string()
        } else {
            format!(" · ⚠ {} fabricated quote(s)", hallucinated_quotes.len())
        };
        let tool_chip = tool_call_summary(&narration_log);
        eprintln!(
            "        → {}ms · {} sources · op={} · score={}{}{}",
            dispatch_ms, sources_count, operation, display_score, hallu_chip, tool_chip,
        );

        results.push(QuestionResult {
            id: q.id.clone(),
            tier: q.tier,
            prompt: q.prompt.clone(),
            skipped_reason: dispatch_err.map(|e| format!("dispatch_failed: {e}")),
            response,
            operation,
            sources_count,
            latency_ms: dispatch_ms,
            expected_facts: q.expected_facts.clone(),
            facts_hit,
            facts_missed,
            mechanical_score_pct,
            judge_score,
            judge_rationale,
            hallucinated_quotes: Some(hallucinated_quotes),
            narration_log,
        });
    }

    let tier_summary = summarize_tiers(&results);
    (results, tier_summary)
}

/// Output of the LLM-judge. Score in 0..=5, rationale a single
/// sentence the report renders verbatim.
struct JudgeOutput {
    score: u8,
    rationale: String,
}

/// Build the judge prompt, call the inference provider, parse the JSON
/// reply. Uses the daemon's primary chat model — same model the bench
/// is grading — but supplied with the verified reference passages and
/// rubric so the judge isn't reasoning from scratch about literary
/// criticism, just checking the answer against ground truth.
async fn run_llm_judge(
    inference: &dyn InferenceProvider,
    q: &Question,
    answer: &str,
    resolved_refs: &[String],
    hallucinated_quotes: &[String],
) -> Result<JudgeOutput, String> {
    let mut prompt = String::new();
    prompt.push_str("You are a strict literary-eval grader for a benchmark on Joseph Conrad's \"The Secret Agent\". Score the answer below against the rubric. Do NOT be lenient.\n\n");
    prompt.push_str("# Question\n");
    prompt.push_str(q.prompt.trim());
    prompt.push_str("\n\n# Answer to grade\n");
    prompt.push_str(answer.trim());
    prompt.push_str("\n\n# Verified reference passages from the source text\n");
    if resolved_refs.is_empty() {
        prompt.push_str("_(no reference passages declared for this question)_\n");
    } else {
        for (i, r) in resolved_refs.iter().enumerate() {
            prompt.push_str(&format!("[{}] {}\n\n", i + 1, r.trim()));
        }
    }
    if !q.expected_facts.is_empty() {
        prompt.push_str("\n# Expected facts (substrings a correct answer should contain)\n");
        for f in &q.expected_facts {
            prompt.push_str(&format!("- {f}\n"));
        }
    }
    if !q.critique_traps.is_empty() {
        prompt.push_str("\n# Contamination traps (Tier 5 — dock score if any apply)\n");
        for t in &q.critique_traps {
            prompt.push_str(&format!("- {t}\n"));
        }
    }
    // **Don't cascade `hallucinated_quotes` into the judge prompt.**
    // The bench's substring-based detector produces false positives
    // when models write long-form Tier 2-5 answers with multiple
    // unrelated `"`-bounded fragments (scare quotes, citations,
    // markdown). Feeding those false positives in with a "cannot
    // score above 1" rule collapsed every T2-T5 judge score to 0/5
    // on the 2026-05-21 v1.1 full run. The detector still runs and
    // populates `hallucinated_quotes` for the report; the judge
    // sees only the answer + verified reference passages and grades
    // against the rubric directly. Argument parameter retained for
    // ABI stability — it's an explicit signal that the cascade was
    // a design choice, not an oversight; the leading underscore
    // marks it intentionally unused.
    let _ = hallucinated_quotes;
    prompt.push_str(
        "\n# Rubric\n\
         - 5: passage-anchored (cites or paraphrases a specific reference passage); correct synthesis; calibrated about uncertainty\n\
         - 4: passage-anchored; mostly correct; minor synthesis gap\n\
         - 3: partially anchored; correct gist\n\
         - 2: paraphrased without anchor; plausible but unverifiable\n\
         - 1: confident assertion without anchor\n\
         - 0: contamination trap fired (Tier 5)\n\
         \n\
         Respond with ONLY a single JSON object on one line, no other text:\n\
         {\"score\": <integer 0-5>, \"rationale\": \"<one short sentence>\"}\n",
    );

    let request = CompletionRequest {
        prompt,
        system_message: Some(
            "You are a literary-eval grader. Return only the JSON object requested.".to_string(),
        ),
        preferred_speed: Speed::Slow,
        max_tokens: Some(256),
        temperature: Some(0.0),
        ..Default::default()
    };
    let response = inference
        .complete(&request)
        .await
        .map_err(|e| format!("inference: {e}"))?;
    parse_judge_json(&response.text)
}

/// Extract `{ "score": N, "rationale": "…" }` from arbitrary LLM
/// output. Models sometimes wrap JSON in ```json fences or prefix it
/// with prose; we tolerate both by isolating the first `{` … last `}`.
fn parse_judge_json(text: &str) -> Result<JudgeOutput, String> {
    let start = text.find('{').ok_or("no `{` in judge response")?;
    let end = text.rfind('}').ok_or("no `}` in judge response")?;
    if end < start {
        return Err("`}` before `{` in judge response".into());
    }
    let payload = &text[start..=end];
    let v: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| format!("parse JSON: {e} (payload: {})", truncate(payload, 200)))?;
    let score_u64 = v
        .get("score")
        .and_then(|s| s.as_u64())
        .ok_or("missing or non-integer `score`")?;
    if score_u64 > 5 {
        return Err(format!("score out of range: {score_u64}"));
    }
    let rationale = v
        .get("rationale")
        .and_then(|r| r.as_str())
        .ok_or("missing `rationale`")?
        .trim()
        .to_string();
    Ok(JudgeOutput {
        score: score_u64 as u8,
        rationale,
    })
}

/// Extract double-quoted substrings of 30..=240 chars from the answer
/// and verify each appears in the source text (after whitespace
/// normalization). Returns the quotes that do NOT appear — those are
/// the fabrication candidates.
///
/// **Upper bound is load-bearing.** Without `{30,240}` the regex
/// `"([^"]{30,})"` greedy-matches across paragraph boundaries
/// whenever a model writes prose with multiple `"` characters in
/// different sentences. Empirically (book-report v1.1 full run,
/// 2026-05-21): T2-T5 questions had 5-15 "fabricated quotes" each
/// that were really 200-800 char prose stitches spanning markdown
/// formatting, scare quotes, and real citations. These false
/// positives cascaded into the LLM-judge prompt's "any hallucinated
/// passage cannot score above 1" rule, dropping every judge score
/// to 0/5. Capping at 240 chars eliminates almost all stitches
/// while preserving genuine 1-2 sentence quoted passages, which is
/// what the bench is supposed to catch.
fn detect_hallucinations(answer: &str, source: &str) -> Vec<String> {
    // Re-use a single Regex per process; the lazy_static dance isn't
    // worth dragging in here. Compile cost is negligible vs the
    // inference round-trip we're about to make.
    let re = match regex::Regex::new(r#""([^"]{30,240})""#) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let source_norm = normalize_whitespace(source).to_lowercase();
    let mut fabricated: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cap in re.captures_iter(answer) {
        let quote = match cap.get(1) {
            Some(m) => m.as_str(),
            None => continue,
        };
        let key = normalize_whitespace(quote).to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key.clone());
        if !source_norm.contains(&key) {
            fabricated.push(quote.to_string());
        }
    }
    fabricated
}

fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// Resolve `[reference_passages]` line ranges to actual text from the
/// cached source file. Lines are 1-indexed; ranges are inclusive.
/// Returns one string per RefPassage, with the note prepended when
/// present so the judge knows what the operator intended.
fn resolve_reference_passages(refs: &[RefPassage], source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut resolved = Vec::with_capacity(refs.len());
    for r in refs {
        let (start, end) = parse_line_range(&r.lines);
        let start_idx = start.saturating_sub(1);
        let end_idx = end.min(lines.len());
        if start_idx >= lines.len() || start_idx > end_idx {
            continue;
        }
        let slice = lines[start_idx..end_idx].join("\n");
        let prefix = r
            .note
            .as_ref()
            .map(|n| format!("({n})\n"))
            .unwrap_or_default();
        resolved.push(format!("{prefix}{slice}"));
    }
    resolved
}

fn parse_line_range(spec: &str) -> (usize, usize) {
    let spec = spec.trim();
    match spec.split_once('-') {
        Some((a, b)) => (a.trim().parse().unwrap_or(0), b.trim().parse().unwrap_or(0)),
        None => {
            let n = spec.parse().unwrap_or(0);
            (n, n)
        }
    }
}

/// One successful question dispatch. `operation` summarises the
/// tools the runtime invoked during the turn (e.g.
/// `attached_doc_search`, `attached_doc_search, knowledge_lookup`),
/// or `Runtime` when no tools fired and the answer came from the
/// retrieval-shaped synthesis path. `narration_log` carries the
/// per-phase events the runtime emitted — the load-bearing
/// diagnostic for whether the model chose to consult the attached
/// document.
struct DispatchedAnswer {
    text: String,
    operation: String,
    sources_count: usize,
    narration_log: Vec<NarrationEvent>,
}

/// Dispatch a single question through the runtime's normal turn
/// pipeline. Fresh `conversation_id` per question so context doesn't
/// leak between bench items; a fresh `DocumentSession` pinned to that
/// conversation tells the runtime an attachment is in scope so it
/// dispatches through `handle_attached_doc_turn` instead of the
/// general-purpose intent handlers.
///
/// **What this used to be.** Before 2026-05-20 this function mirrored
/// the desktop's pre-tool-era `ask_document` flow: call
/// `DocumentAssetManager::route()` first, fall back to
/// `runtime.handle_turn` only on `OffTopic`, otherwise dispatch
/// through `manager.ask()`. The book-report bench exposed that the
/// parallel router mis-routed factual questions about the attached
/// novel as `OffTopic` — sending them to the general corpus when
/// the answer was in the attached text — and that even when it did
/// route correctly, `manager.ask` ran a parallel one-shot map-reduce
/// with no gap-check, no iterative retrieval, and no narration. See
/// sovereign decision `7693f16b`.
///
/// **What it is now.** A thin shim that creates a `DocumentSession`
/// pointing at the Ready asset, then drives the runtime's turn
/// pipeline. `Runtime::handle_turn` detects the session and routes
/// through `handle_attached_doc_turn` — a `ReasonWithTools`-style loop
/// over `[attached_doc_search, knowledge_lookup, web_fetch]` where the
/// model picks tools.
async fn dispatch_question(
    runtime: &Arc<Runtime>,
    store: &Arc<dyn StateStore>,
    asset: &DocumentAsset,
    prompt: &str,
) -> Result<DispatchedAnswer, String> {
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let user_msg = Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        role: Role::User,
        content: prompt.to_string(),
        created_at: chrono::Utc::now().timestamp(),
        metadata: None,
        version: 0,
    };
    store
        .save_message(&user_msg)
        .await
        .map_err(|e| format!("save user msg: {e}"))?;

    // Mint a DocumentSession so the runtime detects the attachment
    // and routes through `handle_attached_doc_turn`. The session is
    // intentionally minimal — `operation` / `map_prompt` /
    // `reduce_prompt` are the legacy map-reduce path's fields and
    // aren't consulted by the new tool-loop handler. We leave them as
    // empty strings rather than inventing values.
    let session = DocumentSession {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        filename: asset.title.clone(),
        source: asset.id.clone(),
        word_count: 0,
        chunk_count: 0,
        created_at: chrono::Utc::now().timestamp(),
        operation: String::new(),
        map_prompt: String::new(),
        reduce_prompt: String::new(),
        last_output: None,
        history: Vec::new(),
    };
    store
        .create_document_session(&session)
        .await
        .map_err(|e| format!("create document session: {e}"))?;

    let response = runtime
        .handle_turn(prompt, &conversation_id)
        .await
        .map_err(|e| format!("runtime: {e}"))?;

    // Retrieved-chunks count is stamped on assistant-message metadata
    // for KQ/SQ/DQ paths. Absent => 0.
    let sources_count = response
        .message
        .metadata
        .as_ref()
        .and_then(|m| m.get("retrieved_chunks"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    // Capture the runtime's narration for this question. The
    // SessionStore retains the latest QuerySession per conversation
    // for 30s after completion, so this read is race-free as long
    // as the bench doesn't churn through questions faster than that.
    let narration_log = runtime
        .sessions
        .latest_for_conversation(&conversation_id)
        .map(|s| s.narration.clone())
        .unwrap_or_default();

    // Operation label = the unique set of tool IDs the runtime
    // actually invoked, derived from narration. Empty => "Runtime"
    // (handler did its own retrieval without going through a Tool).
    let operation = derive_operation_label(&narration_log);

    Ok(DispatchedAnswer {
        text: response.message.content,
        operation,
        sources_count,
        narration_log,
    })
}

/// Pull tool IDs out of `tool_invocation_start` narration phases and
/// return a stable, comma-joined summary. This is what the operator
/// reads at-a-glance to know whether the model picked
/// `attached_doc_search` versus a corpus tool versus none.
fn derive_operation_label(log: &[NarrationEvent]) -> String {
    use std::collections::BTreeSet;
    let mut tool_ids: BTreeSet<String> = BTreeSet::new();
    for evt in log {
        let v = serde_json::to_value(&evt.phase).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(map) = v {
            if let Some(payload) = map.get("tool_invocation_start") {
                if let Some(id) = payload.get("tool_id").and_then(|s| s.as_str()) {
                    tool_ids.insert(id.to_string());
                }
            }
        }
    }
    if tool_ids.is_empty() {
        "Runtime".to_string()
    } else {
        tool_ids.into_iter().collect::<Vec<_>>().join(", ")
    }
}

/// Score a question's response against `expected_facts` by
/// case-insensitive substring presence. Returns `(hit, missed,
/// score_pct)`. Empty `expected_facts` always scores 100 so questions
/// authored without facts (placeholders) don't poison the aggregate.
fn score_question(q: &Question, response: &str) -> (Vec<String>, Vec<String>, u8) {
    let needle_haystack = response.to_lowercase();
    let mut hit = Vec::with_capacity(q.expected_facts.len());
    let mut missed = Vec::with_capacity(q.expected_facts.len());
    for fact in &q.expected_facts {
        if needle_haystack.contains(&fact.to_lowercase()) {
            hit.push(fact.clone());
        } else {
            missed.push(fact.clone());
        }
    }
    let pct = if q.expected_facts.is_empty() {
        100
    } else {
        (hit.len() * 100 / q.expected_facts.len()) as u8
    };
    (hit, missed, pct)
}

fn skipped(q: &Question, reason: &str) -> QuestionResult {
    QuestionResult {
        id: q.id.clone(),
        tier: q.tier,
        prompt: q.prompt.clone(),
        skipped_reason: Some(reason.to_string()),
        response: String::new(),
        operation: String::new(),
        sources_count: 0,
        latency_ms: 0,
        expected_facts: q.expected_facts.clone(),
        facts_hit: Vec::new(),
        facts_missed: q.expected_facts.clone(),
        mechanical_score_pct: 0,
        judge_score: None,
        judge_rationale: None,
        hallucinated_quotes: None,
        narration_log: Vec::new(),
    }
}

/// Snake-case discriminator for a `NarrationPhase` value. Mirrors the
/// TS-side `narrationPhaseTag` helper: bare strings return themselves;
/// struct variants (serialised as `{"tool_invocation_start": {...}}`)
/// return the one key. Used for terse one-line tool-chip rendering
/// without dragging the full payload into the stdout chip.
fn narration_phase_tag(phase: &sovereign_core::types::NarrationPhase) -> String {
    let v = serde_json::to_value(phase).unwrap_or(serde_json::Value::Null);
    match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Object(map) => map
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string()),
        _ => "unknown".to_string(),
    }
}

/// One-line chip summarising the narration log for the stdout chip
/// row. Empty when nothing was recorded; otherwise something like
/// " · tools: retrieval_start, retrieval_complete, gap_check_fired".
fn tool_call_summary(log: &[NarrationEvent]) -> String {
    if log.is_empty() {
        return String::new();
    }
    let tags: Vec<String> = log.iter().map(|e| narration_phase_tag(&e.phase)).collect();
    let unique: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        tags.into_iter()
            .filter(|t| seen.insert(t.clone()))
            .collect()
    };
    format!(" · phases: {}", unique.join(", "))
}

fn summarize_tiers(results: &[QuestionResult]) -> Vec<TierSummary> {
    use std::collections::BTreeMap;
    // Per-tier accumulators:
    //   (ran, skipped, sum_mech, sum_lat, sum_judge, judge_n, hallu_flags)
    type Bucket = (usize, usize, u32, u64, u32, usize, usize);
    let mut by_tier: BTreeMap<u8, Bucket> = BTreeMap::new();
    for r in results {
        let bucket = by_tier.entry(r.tier).or_insert((0, 0, 0, 0, 0, 0, 0));
        if r.skipped_reason.is_none() {
            bucket.0 += 1;
            bucket.2 += r.mechanical_score_pct as u32;
            bucket.3 += r.latency_ms;
            if let Some(js) = r.judge_score {
                bucket.4 += js as u32;
                bucket.5 += 1;
            }
            if r.hallucinated_quotes
                .as_ref()
                .map(|h| !h.is_empty())
                .unwrap_or(false)
            {
                bucket.6 += 1;
            }
        } else {
            bucket.1 += 1;
        }
    }
    by_tier
        .into_iter()
        .map(
            |(tier, (ran, skipped, sum_mech, sum_lat, sum_judge, judge_n, hallu))| TierSummary {
                tier,
                questions_run: ran,
                questions_skipped: skipped,
                mean_mechanical_score_pct: if ran == 0 {
                    0
                } else {
                    (sum_mech / ran as u32) as u8
                },
                mean_judge_score: if judge_n == 0 {
                    None
                } else {
                    Some(sum_judge as f32 / judge_n as f32)
                },
                mean_latency_ms: if ran == 0 { 0 } else { sum_lat / ran as u64 },
                hallucination_flag_count: hallu,
            },
        )
        .collect()
}

/// Best-effort string view of a panic payload — `panic!("foo")` boxes
/// a `&'static str`; `panic!("{}", x)` boxes a `String`; anything else
/// falls back to `<non-string panic>`. Used to surface upstream panics
/// in the bench's per-question error column without losing the rest of
/// the run.
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic>".to_string()
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        trimmed.replace('\n', " ")
    } else {
        let cut: String = trimmed.chars().take(max).collect();
        format!("{}…", cut.replace('\n', " "))
    }
}

/// Drive `DocumentAssetManager::ingest()` end-to-end while recording
/// every `IngestProgress` transition with elapsed-ms timestamps.
/// Returns the completed asset (or `None` on ingest failure), the
/// total attach duration, the transition log, and the terminal phase
/// label.
async fn attach_and_stream(
    manager: &DocumentAssetManager,
    source: &SourceInfo,
) -> (Option<DocumentAsset>, u64, Vec<StateTransition>, String) {
    let attach_at = Instant::now();
    let transitions: Arc<Mutex<Vec<StateTransition>>> = Arc::new(Mutex::new(Vec::new()));
    let callback_transitions = Arc::clone(&transitions);
    let callback_attach_at = attach_at;

    eprintln!("[3/3] attach — DocumentAssetManager::ingest()");
    eprintln!("      transitions stream below; rag_available unblocks Tier-1+2 questions");

    let ingest_result = manager
        .ingest(source.local_path.as_path(), move |progress| {
            let elapsed = callback_attach_at.elapsed().as_millis() as u64;
            let (phase, detail) = render_progress(&progress);
            eprintln!("      t+{:>6}ms  {}", elapsed, phase);
            if let Ok(mut log) = callback_transitions.lock() {
                log.push(StateTransition {
                    ms_since_attach: elapsed,
                    phase: phase.to_string(),
                    detail,
                });
            }
        })
        .await;

    let attach_ms = attach_at.elapsed().as_millis() as u64;
    let (asset, terminal_phase) = match ingest_result {
        Ok(a) => (Some(a), "ready".to_string()),
        Err(e) => {
            eprintln!("      ingest failed after t+{attach_ms}ms: {e}");
            if let Ok(mut log) = transitions.lock() {
                log.push(StateTransition {
                    ms_since_attach: attach_ms,
                    phase: "failed".to_string(),
                    detail: serde_json::json!({ "error": e.to_string() }),
                });
            }
            (None, "failed".to_string())
        }
    };
    let transitions_vec = transitions.lock().map(|g| g.clone()).unwrap_or_default();
    (asset, attach_ms, transitions_vec, terminal_phase)
}

/// Print every DocumentAsset in the daemon's store. Used by
/// `--list-assets` so the operator can pick an id to pass to
/// `--reuse-asset`. Output is intentionally terse — id + state + title
/// is enough to identify a candidate.
async fn list_assets(store: &dyn StateStore) -> Result<(), String> {
    let assets = store
        .list_document_assets()
        .await
        .map_err(|e| format!("list_document_assets: {e}"))?;
    if assets.is_empty() {
        eprintln!("(no document assets in the daemon's store)");
        return Ok(());
    }
    eprintln!("{:<38}  {:<24}  state", "asset_id", "title");
    eprintln!("{}", "─".repeat(80));
    for a in &assets {
        let title = truncate(&a.title, 22);
        eprintln!("{:<38}  {:<24}  {:?}", a.id, title, a.state);
    }
    eprintln!();
    eprintln!("Reuse one with:  sovereign bench book-report --reuse-asset <asset_id>");
    Ok(())
}

/// Stub report returned when `--list-assets` short-circuits. No
/// persistence happens; the caller still gets a `BookReportRun`-shaped
/// value so the success path in `cmd_book_report` doesn't need to
/// branch.
fn stub_report(
    bench_id: String,
    started_at_unix: u64,
    source: SourceInfo,
    chat_model: Option<String>,
) -> BookReportRun {
    BookReportRun {
        bench_id,
        started_at_unix,
        source,
        asset_id: String::new(),
        chat_model,
        attach_ms: 0,
        time_to_rag_ready_ms: None,
        time_to_ready_ms: None,
        transitions: Vec::new(),
        terminated_at_phase: "list_only".to_string(),
        questions: Vec::new(),
        tier_summary: Vec::new(),
    }
}

/// Filter the bench's question list by tier and/or explicit id list.
/// Both filters compose AND; passing neither returns the full bank.
fn filter_questions(all: &[Question], tier: Option<u8>, ids: Option<&[String]>) -> Vec<Question> {
    all.iter()
        .filter(|q| tier.is_none_or(|t| q.tier == t))
        .filter(|q| ids.is_none_or(|ids| ids.iter().any(|target| target == &q.id)))
        .cloned()
        .collect()
}

/// Map an `IngestProgress` event onto the bench's phase taxonomy. We
/// keep our own labels rather than re-exporting the tool's enum so the
/// JSON schema is stable across `sovereign-tools` refactors. The
/// `rag_available` label is what the bench measures as
/// `time_to_rag_ready_ms` — Tier 1+2 questions are answerable from
/// here on, even though the skeleton may still be building.
fn render_progress(p: &IngestProgress) -> (&'static str, serde_json::Value) {
    match p {
        IngestProgress::Started {
            word_count,
            chunk_count,
            filename,
            ..
        } => (
            "started",
            serde_json::json!({
                "word_count": word_count,
                "chunk_count": chunk_count,
                "filename": filename,
            }),
        ),
        IngestProgress::Indexing { done, total } => (
            "indexing",
            serde_json::json!({ "done": done, "total": total }),
        ),
        IngestProgress::RagAvailable { asset_id } => {
            ("rag_available", serde_json::json!({ "asset_id": asset_id }))
        }
        IngestProgress::BuildingSkeleton { done, total } => (
            "building_skeleton",
            serde_json::json!({ "done": done, "total": total }),
        ),
        IngestProgress::MultiHopReady { asset_id } => (
            "multi_hop_ready",
            serde_json::json!({ "asset_id": asset_id }),
        ),
        IngestProgress::Ready {
            asset_id,
            main_entities,
            structural_moments,
        } => (
            "ready",
            serde_json::json!({
                "asset_id": asset_id,
                "main_entities": main_entities,
                "structural_moments": structural_moments,
            }),
        ),
        IngestProgress::Failed { reason } => ("failed", serde_json::json!({ "reason": reason })),
    }
}

async fn fetch_source(cache_dir: &Path, force_refresh: bool) -> Result<SourceInfo, String> {
    std::fs::create_dir_all(cache_dir)
        .map_err(|e| format!("create cache dir {}: {e}", cache_dir.display()))?;
    let local_path = cache_dir.join("974.txt");
    let need_fetch = force_refresh || !local_path.exists();
    if need_fetch {
        eprintln!("      downloading {GUTENBERG_URL}");
        let resp = reqwest::get(GUTENBERG_URL)
            .await
            .map_err(|e| format!("GET {GUTENBERG_URL}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Gutenberg returned HTTP {}", resp.status()));
        }
        let bytes = resp.bytes().await.map_err(|e| format!("read body: {e}"))?;
        std::fs::write(&local_path, &bytes)
            .map_err(|e| format!("write {}: {e}", local_path.display()))?;
    }
    let bytes_on_disk = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
    let sha256 = sha256_of_file(&local_path)?;
    Ok(SourceInfo {
        kind: "gutenberg",
        url: GUTENBERG_URL.to_string(),
        local_path,
        bytes: bytes_on_disk,
        sha256,
    })
}

fn sha256_of_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn persist_report(report: &BookReportRun, explicit_output: Option<&Path>) -> Result<(), String> {
    let default_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".sovereign/bench-runs/book-report")
        .join(&report.bench_id);
    std::fs::create_dir_all(&default_dir)
        .map_err(|e| format!("create run dir {}: {e}", default_dir.display()))?;

    // JSON — machine-readable timings + raw results.
    let json_path = default_dir.join("timings.json");
    let json =
        serde_json::to_string_pretty(report).map_err(|e| format!("serialize report: {e}"))?;
    std::fs::write(&json_path, &json).map_err(|e| format!("write {}: {e}", json_path.display()))?;

    // Markdown — human-readable rollup.
    let md_path = default_dir.join("report.md");
    let md = render_markdown(report);
    std::fs::write(&md_path, &md).map_err(|e| format!("write {}: {e}", md_path.display()))?;

    eprintln!("      report:    {}", md_path.display());
    eprintln!("      timings:   {}", json_path.display());

    if let Some(extra) = explicit_output {
        if extra != json_path {
            std::fs::write(extra, &json).map_err(|e| format!("write {}: {e}", extra.display()))?;
            eprintln!("      timings:   {}", extra.display());
        }
    }
    Ok(())
}

/// Render the human-readable bench rollup. Sections:
///   - source + chat model + asset id
///   - timing milestones (attach / RAG-ready / Ready)
///   - state transition table
///   - per-tier summary
///   - per-question detail with hit/miss against expected_facts
fn render_markdown(r: &BookReportRun) -> String {
    use std::fmt::Write;
    let mut s = String::new();

    let _ = writeln!(s, "# book-report — `{}`\n", r.bench_id);
    let _ = writeln!(
        s,
        "Run started <t:{}>. Source pinned to SHA-256 `{}`.\n",
        r.started_at_unix,
        &r.source.sha256.get(..16).unwrap_or(&r.source.sha256),
    );

    let _ = writeln!(s, "## Source");
    let _ = writeln!(s, "- **File:** `{}`", r.source.local_path.display());
    let _ = writeln!(s, "- **Bytes:** {}", r.source.bytes);
    let _ = writeln!(s, "- **SHA-256:** `{}`", r.source.sha256);
    let _ = writeln!(s, "- **URL:** {}", r.source.url);
    let _ = writeln!(
        s,
        "- **Chat model:** `{}`\n",
        r.chat_model.as_deref().unwrap_or("<unknown>")
    );

    let _ = writeln!(s, "## Timings");
    let _ = writeln!(s, "| Milestone | ms |");
    let _ = writeln!(s, "|---|---:|");
    let _ = writeln!(s, "| attach → ingest return | {} |", r.attach_ms);
    let _ = writeln!(
        s,
        "| attach → RAG-ready | {} |",
        r.time_to_rag_ready_ms
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_string()),
    );
    let _ = writeln!(
        s,
        "| attach → Ready | {} |",
        r.time_to_ready_ms
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_string()),
    );
    let _ = writeln!(s, "| terminated at | `{}` |\n", r.terminated_at_phase);

    if !r.transitions.is_empty() {
        let _ = writeln!(s, "### State transitions");
        let _ = writeln!(s, "| ms | phase |");
        let _ = writeln!(s, "|---:|---|");
        for t in &r.transitions {
            let _ = writeln!(s, "| {} | `{}` |", t.ms_since_attach, t.phase);
        }
        let _ = writeln!(s);
    }

    if !r.tier_summary.is_empty() {
        let _ = writeln!(s, "## Tier summary");
        let _ = writeln!(
            s,
            "| Tier | ran | skipped | mechanical | judge (0-5) | hallucinations | mean latency |"
        );
        let _ = writeln!(s, "|---:|---:|---:|---:|---:|---:|---:|");
        for t in &r.tier_summary {
            let mech = if t.questions_run == 0 {
                "—".to_string()
            } else {
                format!("{}%", t.mean_mechanical_score_pct)
            };
            let judge = match t.mean_judge_score {
                Some(s) => format!("{:.1}", s),
                None => "—".to_string(),
            };
            let hallu = if t.hallucination_flag_count == 0 {
                "—".to_string()
            } else {
                format!("⚠ {}", t.hallucination_flag_count)
            };
            let lat = if t.questions_run == 0 {
                "—".to_string()
            } else {
                format!("{} ms", t.mean_latency_ms)
            };
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} | {} |",
                t.tier, t.questions_run, t.questions_skipped, mech, judge, hallu, lat,
            );
        }
        let _ = writeln!(s);
    }

    if !r.questions.is_empty() {
        let _ = writeln!(s, "## Per-question");
        for q in &r.questions {
            let _ = writeln!(s, "### `{}` (Tier {})", q.id, q.tier);
            if let Some(reason) = &q.skipped_reason {
                let _ = writeln!(s, "**Skipped:** {reason}\n");
                continue;
            }
            // Header line — pull whichever score is load-bearing for
            // this tier into the top slot.
            let score_summary = if q.tier == 1 {
                format!(
                    "mechanical {}/{} ({}%)",
                    q.facts_hit.len(),
                    q.expected_facts.len(),
                    q.mechanical_score_pct,
                )
            } else {
                match q.judge_score {
                    Some(s) => format!(
                        "judge {}/5 · mechanical {}/{} ({}%)",
                        s,
                        q.facts_hit.len(),
                        q.expected_facts.len(),
                        q.mechanical_score_pct,
                    ),
                    None => format!(
                        "judge —  · mechanical {}/{} ({}%)",
                        q.facts_hit.len(),
                        q.expected_facts.len(),
                        q.mechanical_score_pct,
                    ),
                }
            };
            let hallu_marker = q
                .hallucinated_quotes
                .as_ref()
                .filter(|h| !h.is_empty())
                .map(|h| format!(" · ⚠ {} fabricated", h.len()))
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "*{} sources · {} ms · {}{}*\n",
                q.sources_count, q.latency_ms, score_summary, hallu_marker,
            );
            let _ = writeln!(s, "**Q:** {}\n", q.prompt.trim());
            let _ = writeln!(s, "**A:**\n\n> {}\n", indent_quoted(&q.response));
            if let Some(rationale) = &q.judge_rationale {
                let _ = writeln!(s, "**Judge rationale:** {}\n", rationale.trim());
            }
            if !q.expected_facts.is_empty() {
                let _ = writeln!(s, "**Facts hit:** {}", facts_list(&q.facts_hit));
                let _ = writeln!(s, "**Facts missed:** {}", facts_list(&q.facts_missed));
            }
            if let Some(hallu) = &q.hallucinated_quotes {
                if !hallu.is_empty() {
                    let _ = writeln!(s, "\n**Fabricated quotes (not in source):**");
                    for h in hallu {
                        let _ = writeln!(s, "- > {}", truncate(h, 200));
                    }
                }
            }
            if !q.narration_log.is_empty() {
                let _ = writeln!(s, "\n**Narration log (runtime tool/phase trace):**");
                let _ = writeln!(s, "| ms | phase | text |");
                let _ = writeln!(s, "|---:|---|---|");
                for evt in &q.narration_log {
                    let tag = narration_phase_tag(&evt.phase);
                    let text = truncate(&evt.text, 80).replace('|', "\\|");
                    let _ = writeln!(s, "| {} | `{}` | {} |", evt.elapsed_ms, tag, text);
                }
            }
            let _ = writeln!(s, "\n**Operation:** `{}`\n", q.operation);
        }
    }

    let _ = writeln!(
        s,
        "---\n\n_LLM-judge runs for Tier 2-5 using the daemon's primary model. Hallucination check substring-matches every quoted span ≥30 chars in the answer against pg974.txt._"
    );
    s
}

fn indent_quoted(text: &str) -> String {
    text.lines()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n> ")
}

fn facts_list(facts: &[String]) -> String {
    if facts.is_empty() {
        "_(none)_".to_string()
    } else {
        facts
            .iter()
            .map(|f| format!("`{}`", f))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn print_summary(r: &BookReportRun) {
    eprintln!();
    eprintln!(
        "─── book-report run {} ──────────────────────────",
        r.bench_id
    );
    eprintln!(
        "  source:       {} ({} bytes)",
        r.source.local_path.display(),
        r.source.bytes
    );
    eprintln!(
        "  chat model:   {}",
        r.chat_model.as_deref().unwrap_or("<unknown>")
    );
    eprintln!(
        "  asset id:     {}",
        if r.asset_id.is_empty() {
            "<failed>"
        } else {
            &r.asset_id
        },
    );
    eprintln!("  attach ms:    {}", r.attach_ms);
    if let Some(rag) = r.time_to_rag_ready_ms {
        eprintln!("  RAG-ready ms: {rag}  (Tier 1+2 dispatchable)");
    }
    if let Some(ready) = r.time_to_ready_ms {
        eprintln!("  Ready ms:     {ready}  (Tier 4+5 dispatchable)");
    }
    eprintln!("  transitions:  {} recorded", r.transitions.len());
    eprintln!("  terminated:   {}", r.terminated_at_phase);

    if !r.tier_summary.is_empty() {
        eprintln!();
        eprintln!("  Tier  ran  skip  mech    judge   hallu  latency");
        eprintln!("  ───────────────────────────────────────────────");
        for t in &r.tier_summary {
            let mech = if t.questions_run == 0 {
                "—".to_string()
            } else {
                format!("{}%", t.mean_mechanical_score_pct)
            };
            let judge = match t.mean_judge_score {
                Some(s) => format!("{:.1}/5", s),
                None => "—".to_string(),
            };
            let hallu = if t.hallucination_flag_count == 0 {
                "—".to_string()
            } else {
                format!("⚠{}", t.hallucination_flag_count)
            };
            let lat = if t.questions_run == 0 {
                "—".to_string()
            } else {
                format!("{}ms", t.mean_latency_ms)
            };
            eprintln!(
                "   {}    {}    {}    {:>4}   {:>5}   {:>5}  {:>8}",
                t.tier, t.questions_run, t.questions_skipped, mech, judge, hallu, lat,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::types::NarrationPhase;

    fn evt(phase: NarrationPhase) -> NarrationEvent {
        NarrationEvent {
            phase,
            text: String::new(),
            elapsed_ms: 0,
        }
    }

    #[test]
    fn derive_operation_label_empty_log_is_runtime() {
        assert_eq!(derive_operation_label(&[]), "Runtime");
    }

    #[test]
    fn derive_operation_label_single_tool_invocation() {
        let log = vec![
            evt(NarrationPhase::RoutingStart),
            evt(NarrationPhase::ToolInvocationStart {
                call_id: "c1".into(),
                tool_id: "attached_doc_search".into(),
                summary: "Searching attached document".into(),
            }),
            evt(NarrationPhase::ToolInvocationComplete {
                call_id: "c1".into(),
                tool_id: "attached_doc_search".into(),
                ok: true,
                result_summary: "3 passages".into(),
            }),
        ];
        assert_eq!(derive_operation_label(&log), "attached_doc_search");
    }

    #[test]
    fn derive_operation_label_multi_tool_dedups_and_sorts() {
        // Same tool invoked twice + another tool — dedup, then
        // alphabetise so the label is stable across runs.
        let log = vec![
            evt(NarrationPhase::ToolInvocationStart {
                call_id: "c1".into(),
                tool_id: "knowledge_lookup".into(),
                summary: "".into(),
            }),
            evt(NarrationPhase::ToolInvocationStart {
                call_id: "c2".into(),
                tool_id: "attached_doc_search".into(),
                summary: "".into(),
            }),
            evt(NarrationPhase::ToolInvocationStart {
                call_id: "c3".into(),
                tool_id: "attached_doc_search".into(),
                summary: "".into(),
            }),
        ];
        assert_eq!(
            derive_operation_label(&log),
            "attached_doc_search, knowledge_lookup",
        );
    }

    #[test]
    fn derive_operation_label_ignores_non_tool_phases() {
        // Retrieval/curation/drafting frames are pipeline stages
        // inside a handler, not tool invocations. The "operation"
        // label is specifically about tools the model picked, so
        // these must not leak in.
        let log = vec![
            evt(NarrationPhase::RetrievalStart),
            evt(NarrationPhase::RetrievalComplete {
                chunks_in: 8,
                corpora: vec!["sep".into()],
            }),
            evt(NarrationPhase::DraftingStart),
        ];
        assert_eq!(derive_operation_label(&log), "Runtime");
    }
}
