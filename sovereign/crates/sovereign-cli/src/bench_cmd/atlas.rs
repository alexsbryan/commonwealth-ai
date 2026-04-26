//! Atlas-pipeline throughput + correctness bench.
//!
//! Hits the running daemon's `/v1/chat/completions` with a fixed set
//! of representative tasks, captures token counts straight from the
//! response `usage` block (so tokens/sec is the daemon's own count,
//! not a string-length estimate), validates output with the real
//! literary-atlas Phase 1 parser, and projects 1800-article runtime.
//!
//! Tasks (MVP):
//!   - `phase1_short` / `phase1_medium` / `phase1_long` — full atlas
//!     Phase 1 prompt against the three real al-Farabi chapters.
//!     Validator runs `LiteraryAtlasPipeline::parse_phase1` and counts
//!     atoms; that's a stricter check than "is it JSON" because the
//!     pipeline's lenient deserializer rejects the no-atoms /
//!     placeholder-echo failure modes.
//!   - `cluster_name_synth` — small structured prompt → single-object
//!     output. Stand-in for the Phase 3 / 5 / 8 short-call loops that
//!     dominate wall-clock after Phase 1.
//!
//! Why these tasks? The 38-min sep-al-farabi run was 79% Phase 1
//! and ~21% short calls. Same shape every article, so a benchmark
//! that mirrors the cost split projects total runtime accurately.
//!
//! Workflow:
//!   1. Edit `~/.config/sovereign/config.toml` `[models].primary`.
//!   2. `systemctl --user restart sovereign.service`
//!   3. `sovereign bench atlas --output run-<label>.json`
//!   4. Repeat for each candidate model.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use corpus_engine::enrichment::pipeline::{
    pipelines::literary_atlas::LiteraryAtlasPipeline, ChapterInput, ChatPrompt, Pipeline,
};
use serde::{Deserialize, Serialize};

use crate::enrich_cmd::config::EnrichConfig;
use crate::enrich_cmd::corpus_io::rebuild_corpus_state;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench atlas",
    summary: "Measure atlas LLM throughput + correctness against the loaded primary.",
    sections: &[
        HelpSection::Usage(
            "sovereign bench atlas [--corpus <id>] [--output <path>] [--tasks <ids>] [--no-warmup] [--max-tokens-cap <n>]",
        ),
        HelpSection::Flags(&[
            (
                "--corpus <id>",
                "Corpus to source Phase 1 chapters from. Default: sep-al-farabi (the smallest \
                 ingested SEP article — three chapters give a usable size spread). Must already \
                 be `sovereign enrich init`'d.",
            ),
            (
                "--output <path>",
                "Write structured JSON results to this path. The stdout summary table is \
                 always printed regardless. Pair with `--label` per run for archival.",
            ),
            (
                "--tasks <ids>",
                "Comma-separated task ids to run. Default: all. Useful for fast iteration on \
                 a single task while tuning a model.",
            ),
            (
                "--no-warmup",
                "Skip the 5-token warmup that ensures the lazy primary slot is already loaded \
                 before timing. Use when the slot is known-warm or you want load-tax included.",
            ),
            (
                "--max-tokens-cap <n>",
                "Override the per-task max_tokens cap (default 16384). Useful for testing how \
                 a model behaves with tighter or looser output budgets.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign bench atlas --output bench-qwopus.json",
                "Run all tasks, write JSON to disk, print table.",
            ),
            (
                "sovereign bench atlas --tasks phase1_medium,cluster_name_synth",
                "Iterate on two specific tasks during model tuning.",
            ),
        ]),
        HelpSection::Notes(
            "The bench measures the daemon's currently-loaded primary, not an arbitrary model \
             — restart the daemon between candidates. Each task's tokens/sec is computed from \
             the daemon-reported `usage.completion_tokens`, not response-string length, so it \
             accounts for tokenisation differences between models.",
        ),
    ],
};

// ─── CLI args ──────────────────────────────────────────────────

struct Args {
    corpus: String,
    output: Option<PathBuf>,
    tasks: Option<Vec<String>>,
    warmup: bool,
    max_tokens_cap: u32,
}

impl Args {
    fn defaults() -> Self {
        Self {
            corpus: "sep-al-farabi".into(),
            output: None,
            tasks: None,
            warmup: true,
            max_tokens_cap: 16384,
        }
    }
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::defaults();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => {
                i += 1;
                out.corpus = args
                    .get(i)
                    .ok_or("--corpus expects a value")?
                    .clone();
            }
            "--output" => {
                i += 1;
                out.output = Some(PathBuf::from(
                    args.get(i).ok_or("--output expects a path")?,
                ));
            }
            "--tasks" => {
                i += 1;
                let raw = args.get(i).ok_or("--tasks expects a value")?;
                out.tasks = Some(raw.split(',').map(|s| s.trim().to_string()).collect());
            }
            "--no-warmup" => {
                out.warmup = false;
            }
            "--max-tokens-cap" => {
                i += 1;
                let raw = args.get(i).ok_or("--max-tokens-cap expects a value")?;
                out.max_tokens_cap = raw
                    .parse()
                    .map_err(|e| format!("--max-tokens-cap parse error: {e}"))?;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(out)
}

// ─── Task model ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Validator {
    /// Run the real `LiteraryAtlasPipeline::parse_phase1` and count
    /// atoms. Strict — placeholder echo / no-atom / shape drift all
    /// fail.
    Phase1Atlas,
    /// Top-level JSON parse only. Used for tasks where the schema is
    /// looser than Phase 1 (cluster naming).
    JsonShape,
}

struct BenchTask {
    id: String,
    description: String,
    prompt: ChatPrompt,
    /// Per-task max_tokens. Phase 1 needs the full atlas budget;
    /// short-call tasks use a smaller budget so a runaway response
    /// doesn't dominate measurement time.
    max_tokens: u32,
    validator: Validator,
}

// ─── Result types (also serialized to JSON output) ─────────────

#[derive(Debug, Serialize, Deserialize)]
struct BenchTaskResult {
    task_id: String,
    description: String,
    success: bool,
    error: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    latency_ms: u128,
    /// `completion_tokens / latency_seconds`. The headline metric for
    /// model selection — at 1800-article scale, 5 tok/s vs 15 tok/s
    /// is the difference between a weekend and a month.
    decode_tokens_per_sec: f64,
    /// Atlas Phase 1 only — count of (entities + states + relations
    /// + events + claims + questions) the parser kept after lenient
    /// deserialization. Higher is generally better; `None` for
    /// non-atlas tasks.
    atoms_extracted: Option<usize>,
    /// `finish_reason == "length"` — the model hit `max_tokens` and
    /// got truncated. A truthy value here means tokens/sec is
    /// understated (decode kept going, output got cut) AND the
    /// validator may have rejected a partial extraction.
    truncated: bool,
    /// The model's full response text, captured ONLY when validation
    /// failed. Without it, "Phase 1 produced no recognisable JSON"
    /// is unactionable — you can't tell apart "wrapped in markdown
    /// fence" from "internal JSON corruption deep in the body" from
    /// "wrote prose instead of JSON" without seeing what came back.
    /// On success we drop the body to keep result files small. On
    /// failure we keep the whole thing because the corruption is
    /// often not in the first 500 chars (Darwin-9B's structural
    /// breakdown hit at byte 11409 of a 14720-char response).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response_head: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BenchRun {
    timestamp_utc: String,
    base_url: String,
    /// What the daemon actually loaded — pulled from each response's
    /// `model` field, not the request. Single source of truth so
    /// archives can't be relabelled later.
    model_id: String,
    corpus_id: String,
    warmup_used: bool,
    results: Vec<BenchTaskResult>,
    summary: BenchSummary,
}

#[derive(Debug, Serialize, Deserialize)]
struct BenchSummary {
    /// Mean tokens/sec across all tasks that succeeded. NaN if zero
    /// successes (error states aren't averaged in).
    decode_tps_mean: f64,
    /// Mean tokens/sec across only the Phase 1 tasks that succeeded.
    /// This is what dominates batch wall-clock.
    phase1_decode_tps_mean: f64,
    phase1_success_rate: f64,
    /// Mean Phase 1 wall-clock per chapter, averaged across the
    /// successful tasks. The number to multiply by your article-
    /// count × chapters-per-article to get a runtime estimate.
    phase1_seconds_per_chapter_mean: f64,
    /// Projected hours for 1800 SEP articles assuming 5 chapters per
    /// article and Phase 1 is the dominant cost (~80% in the
    /// reference run). `None` if no Phase 1 tasks succeeded.
    est_hours_1800_articles_5_chapters: Option<f64>,
}

// ─── Entry point ───────────────────────────────────────────────

pub async fn cmd_atlas(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    let cfg = match EnrichConfig::require(&parsed.corpus) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading enrichment config for `{}`: {e}", parsed.corpus);
            eprintln!("hint: run `sovereign enrich init {}` first", parsed.corpus);
            return 1;
        }
    };

    let (chapters, _manifest) = match rebuild_corpus_state(&cfg) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: rebuilding corpus state: {e}");
            return 1;
        }
    };
    if chapters.len() < 3 {
        eprintln!(
            "error: bench expects at least 3 chapters in `{}` for size variation; \
             found {}.",
            parsed.corpus,
            chapters.len()
        );
        return 1;
    }

    // Build tasks. Phase 1 sources prompts from the real pipeline so
    // any schema drift between bench and production is impossible.
    let tasks = build_tasks(&chapters, parsed.max_tokens_cap);

    let selected: Vec<&BenchTask> = if let Some(ids) = &parsed.tasks {
        let pool: Vec<&BenchTask> = tasks.iter().filter(|t| ids.contains(&t.id)).collect();
        if pool.is_empty() {
            eprintln!(
                "error: --tasks filter matched zero tasks. Available: {}",
                tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")
            );
            return 2;
        }
        pool
    } else {
        tasks.iter().collect()
    };

    println!("=== sovereign bench atlas ===");
    println!("  daemon:   {}", cfg.base_url);
    println!("  corpus:   {}", parsed.corpus);
    println!("  tasks:    {}", selected.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", "));
    println!();

    let client = reqwest::Client::builder()
        // Phase 1 against a long chapter on a slow model can run 15+
        // minutes; default reqwest timeout would kill it. Pick a cap
        // generous enough for any plausible run on this hardware.
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .unwrap();

    if parsed.warmup {
        println!("  warmup:   loading slot with a 5-token request…");
        if let Err(e) = warmup(&client, &cfg.base_url).await {
            eprintln!("warn: warmup failed ({e}); continuing without it.");
        }
        println!();
    }

    let mut model_id: Option<String> = None;
    let mut results: Vec<BenchTaskResult> = Vec::new();
    let pipeline = LiteraryAtlasPipeline::new();
    let pipeline = Arc::new(pipeline);

    println!(
        "  {:<20} {:>6} {:>10} {:>10} {:>10} {:>10} {:>8}",
        "task", "ok", "prompt", "out", "secs", "tok/s", "atoms"
    );
    println!("  {}", "-".repeat(80));

    for task in &selected {
        let start = Instant::now();
        let raw = run_chat(
            &client,
            &cfg.base_url,
            &task.prompt,
            task.max_tokens,
        )
        .await;
        let elapsed = start.elapsed();

        let result = match raw {
            Ok(resp) => {
                if model_id.is_none() {
                    model_id = Some(resp.model.clone());
                }
                let (success, error, atoms) =
                    validate(task, &resp.content, pipeline.as_ref());
                let secs = elapsed.as_secs_f64();
                let tps = if secs > 0.0 {
                    resp.usage.completion_tokens as f64 / secs
                } else {
                    0.0
                };
                BenchTaskResult {
                    task_id: task.id.clone(),
                    description: task.description.clone(),
                    success,
                    error,
                    prompt_tokens: resp.usage.prompt_tokens,
                    completion_tokens: resp.usage.completion_tokens,
                    total_tokens: resp.usage.total_tokens,
                    latency_ms: elapsed.as_millis(),
                    decode_tokens_per_sec: tps,
                    atoms_extracted: atoms,
                    truncated: resp.finish_reason.as_deref() == Some("length"),
                    // Full body on failure — the corruption is often
                    // not in the first 500 chars (see Darwin-9B's
                    // missing-quote at byte 11409). Drop on success
                    // to keep result files small.
                    response_head: if success { None } else { Some(resp.content.clone()) },
                }
            }
            Err(e) => BenchTaskResult {
                task_id: task.id.clone(),
                description: task.description.clone(),
                success: false,
                error: Some(e.to_string()),
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                latency_ms: elapsed.as_millis(),
                decode_tokens_per_sec: 0.0,
                atoms_extracted: None,
                truncated: false,
                response_head: None,
            },
        };

        println!(
            "  {:<20} {:>6} {:>10} {:>10} {:>10.1} {:>10.2} {:>8}",
            result.task_id,
            if result.success { "✓" } else { "✗" },
            result.prompt_tokens,
            result.completion_tokens,
            result.latency_ms as f64 / 1000.0,
            result.decode_tokens_per_sec,
            result
                .atoms_extracted
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        if let Some(err) = &result.error {
            // Truncate to first line — full text goes in JSON.
            let first = err.lines().next().unwrap_or("(empty)");
            println!("                       error: {first}");
        }
        if let Some(head) = &result.response_head {
            // Strip newlines so the head fits one line in the table.
            // The full untruncated head goes in --output JSON.
            let one_line: String = head
                .chars()
                .map(|c| if c == '\n' { ' ' } else { c })
                .collect();
            let preview: String = one_line.chars().take(120).collect();
            println!("                       response[head]: {preview}…");
        }
        if result.truncated {
            println!("                       ⚠ truncated — hit max_tokens");
        }
        results.push(result);
    }

    let summary = summarize(&results);
    let model_label = model_id.clone().unwrap_or_else(|| "<unknown>".into());

    println!();
    println!("  --- summary ({}) ---", model_label);
    println!(
        "  decode tok/s avg ............... {:>8.2}",
        summary.decode_tps_mean
    );
    println!(
        "  phase1 decode tok/s avg ........ {:>8.2}",
        summary.phase1_decode_tps_mean
    );
    println!(
        "  phase1 success rate ............ {:>8.1}%",
        summary.phase1_success_rate * 100.0
    );
    println!(
        "  phase1 secs/chapter avg ........ {:>8.1}",
        summary.phase1_seconds_per_chapter_mean
    );
    if let Some(hours) = summary.est_hours_1800_articles_5_chapters {
        println!(
            "  est. 1800 articles × 5 ch ...... {:>8.1} h  ({:.1} days)",
            hours,
            hours / 24.0
        );
    } else {
        println!("  est. 1800 articles × 5 ch ......      n/a  (no Phase 1 successes)");
    }

    if let Some(path) = &parsed.output {
        let run = BenchRun {
            timestamp_utc: chrono_format_now(),
            base_url: cfg.base_url.clone(),
            model_id: model_label,
            corpus_id: parsed.corpus.clone(),
            warmup_used: parsed.warmup,
            results,
            summary,
        };
        match serde_json::to_string_pretty(&run) {
            Ok(json) => match std::fs::write(path, json) {
                Ok(()) => println!("\n  wrote results → {}", path.display()),
                Err(e) => {
                    eprintln!("error: writing {}: {e}", path.display());
                    return 1;
                }
            },
            Err(e) => {
                eprintln!("error: serializing results: {e}");
                return 1;
            }
        }
    }

    0
}

// ─── Tasks ─────────────────────────────────────────────────────

fn build_tasks(chapters: &[ChapterInput], max_tokens_cap: u32) -> Vec<BenchTask> {
    // Sort chapters by body length so `_short` / `_medium` / `_long`
    // are stable across runs — relying on chapter id order would be
    // brittle for corpora that aren't naturally length-ordered.
    let mut by_len: Vec<&ChapterInput> = chapters.iter().collect();
    by_len.sort_by_key(|c| c.text.len());
    let short = by_len[0];
    let medium = by_len[by_len.len() / 2];
    let long = by_len[by_len.len() - 1];

    let pipeline = LiteraryAtlasPipeline::new();
    let phase1_short = pipeline.compose_phase1(short, &[]);
    let phase1_medium = pipeline.compose_phase1(medium, &[]);
    let phase1_long = pipeline.compose_phase1(long, &[]);

    vec![
        BenchTask {
            id: "phase1_short".into(),
            description: format!(
                "Atlas Phase 1 on the shortest chapter ({} bytes body)",
                short.text.len()
            ),
            prompt: phase1_short,
            max_tokens: max_tokens_cap,
            validator: Validator::Phase1Atlas,
        },
        BenchTask {
            id: "phase1_medium".into(),
            description: format!(
                "Atlas Phase 1 on the median chapter ({} bytes body)",
                medium.text.len()
            ),
            prompt: phase1_medium,
            max_tokens: max_tokens_cap,
            validator: Validator::Phase1Atlas,
        },
        BenchTask {
            id: "phase1_long".into(),
            description: format!(
                "Atlas Phase 1 on the longest chapter ({} bytes body)",
                long.text.len()
            ),
            prompt: phase1_long,
            max_tokens: max_tokens_cap,
            validator: Validator::Phase1Atlas,
        },
        BenchTask {
            id: "cluster_name_synth".into(),
            description:
                "Synthetic Phase 3-style cluster naming (small input, single-object output)"
                    .into(),
            prompt: cluster_name_prompt(),
            // Cluster naming outputs are tiny in production. Cap
            // smaller so a runaway response (model that ignores the
            // schema and rambles) costs ~2 min instead of ~20.
            max_tokens: 1024,
            validator: Validator::JsonShape,
        },
    ]
}

fn cluster_name_prompt() -> ChatPrompt {
    // Mirrors the production Phase 3 prompt shape (system tells the
    // model what a cluster is; user lists members; output is a
    // small structured object). Kept synthetic so the bench doesn't
    // require the corpus to have already produced clusters.
    let system = "You are an atlas-mapping assistant. Given a list of related entities \
                  drawn from a single document, propose a short canonical label that \
                  names the cluster they belong to, plus a one-sentence rationale. \
                  Respond with a single JSON object: {\"label\": \"...\", \"rationale\": \"...\"}.";
    let user = "Cluster members:\n\
                - al-Fârâbî (philosopher, 870–950 CE)\n\
                - Aristotle (philosopher)\n\
                - Plato (philosopher)\n\
                - Organon (work)\n\
                - Categories (work)\n\
                - On Interpretation (work)\n\
                - Prior Analytics (work)\n\
                - Posterior Analytics (work)\n\
                - syllogism (concept)\n\
                - demonstration (concept)\n\n\
                Return JSON only.";
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "label": { "type": "string" },
            "rationale": { "type": "string" }
        },
        "required": ["label", "rationale"],
        "additionalProperties": false
    });
    ChatPrompt::new(system, user)
        .with_response_schema("cluster_label", schema)
}

// ─── HTTP + response shape ─────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct UsageBlock {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct ChatResponseEnvelope {
    #[serde(default)]
    model: String,
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<UsageBlock>,
}

struct ChatResponse {
    model: String,
    content: String,
    finish_reason: Option<String>,
    usage: UsageBlock,
}

async fn run_chat(
    client: &reqwest::Client,
    base_url: &str,
    prompt: &ChatPrompt,
    max_tokens: u32,
) -> Result<ChatResponse, String> {
    let url = format!("{}/v1/chat/completions", base_url);
    // `model` is informational — the daemon's Priority-0 local-inference
    // path ignores it and serves whichever primary slot is loaded.
    // Keep an honest default so the request body looks like real
    // production traffic if anyone tails the journal.
    let mut body = serde_json::json!({
        "model": "primary",
        "messages": [
            { "role": "system", "content": prompt.system },
            { "role": "user", "content": prompt.user },
        ],
        // Greedy decoding makes correctness measurements
        // reproducible across bench reruns of the same model. It's
        // a slight departure from production (temperature 0.2) but
        // throughput is dominated by raw decode speed, not sampling
        // noise.
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "stream": false,
    });
    if let Some(schema) = prompt.response_schema.as_ref() {
        let name = prompt
            .response_schema_name
            .as_deref()
            .unwrap_or("response_schema");
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "response_format".into(),
                serde_json::json!({
                    "type": "json_schema",
                    "json_schema": { "name": name, "schema": schema, "strict": true }
                }),
            );
        }
    }
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("body read: {e}"))?;
    if !status.is_success() {
        return Err(format!("daemon HTTP {status}: {text}"));
    }
    let env: ChatResponseEnvelope = serde_json::from_str(&text)
        .map_err(|e| format!("response not JSON: {e} — body: {text}"))?;
    let choice = env
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| "response had zero choices".to_string())?;
    Ok(ChatResponse {
        model: env.model,
        content: choice.message.content,
        finish_reason: choice.finish_reason,
        usage: env.usage.unwrap_or(UsageBlock {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        }),
    })
}

async fn warmup(client: &reqwest::Client, base_url: &str) -> Result<(), String> {
    let prompt = ChatPrompt::new("You are a benchmark warmup.", "Say 'ok'.");
    let _ = run_chat(client, base_url, &prompt, 5).await?;
    Ok(())
}

// ─── Validation ────────────────────────────────────────────────

fn validate(
    task: &BenchTask,
    content: &str,
    pipeline: &LiteraryAtlasPipeline,
) -> (bool, Option<String>, Option<usize>) {
    match task.validator {
        Validator::Phase1Atlas => match pipeline.parse_phase1(content) {
            Ok(parsed) => {
                let atoms = parsed
                    .section_extraction
                    .as_ref()
                    .map(count_atoms)
                    .unwrap_or(0);
                (true, None, Some(atoms))
            }
            Err(e) => (false, Some(e.to_string()), None),
        },
        Validator::JsonShape => match serde_json::from_str::<serde_json::Value>(content) {
            Ok(v) if v.is_object() => (true, None, None),
            Ok(_) => (
                false,
                Some("response parsed as JSON but is not an object".into()),
                None,
            ),
            Err(e) => (false, Some(format!("not valid JSON: {e}")), None),
        },
    }
}

fn count_atoms(ext: &corpus_engine::enrichment::pipeline::SectionExtraction) -> usize {
    ext.entities_introduced.len()
        + ext.entities_developed.len()
        + ext.relations_introduced.len()
        + ext.relations_developed.len()
        + ext.events.len()
        + ext.claims.len()
        + ext.questions_raised.len()
}

// ─── Summary ───────────────────────────────────────────────────

fn summarize(results: &[BenchTaskResult]) -> BenchSummary {
    let successful: Vec<&BenchTaskResult> = results.iter().filter(|r| r.success).collect();
    let phase1_all: Vec<&BenchTaskResult> = results
        .iter()
        .filter(|r| r.task_id.starts_with("phase1_"))
        .collect();
    let phase1_ok: Vec<&BenchTaskResult> =
        phase1_all.iter().filter(|r| r.success).copied().collect();

    let decode_tps_mean = mean(successful.iter().map(|r| r.decode_tokens_per_sec));
    let phase1_decode_tps_mean = mean(phase1_ok.iter().map(|r| r.decode_tokens_per_sec));
    let phase1_secs_mean = mean(
        phase1_ok
            .iter()
            .map(|r| r.latency_ms as f64 / 1000.0),
    );
    let phase1_success_rate = if phase1_all.is_empty() {
        0.0
    } else {
        phase1_ok.len() as f64 / phase1_all.len() as f64
    };
    // 1800 articles × 5 chapters × per-chapter wall-clock. Phase 1
    // dominates ~80% of pipeline cost in the reference run; the
    // estimate covers Phase 1 specifically — short-call phases add
    // ~25% on top, which the operator can ballpark from the
    // `cluster_name_synth` row.
    let est_hours = if phase1_secs_mean.is_finite() && phase1_secs_mean > 0.0 {
        Some(phase1_secs_mean * 1800.0 * 5.0 / 3600.0)
    } else {
        None
    };

    BenchSummary {
        decode_tps_mean,
        phase1_decode_tps_mean,
        phase1_success_rate,
        phase1_seconds_per_chapter_mean: phase1_secs_mean,
        est_hours_1800_articles_5_chapters: est_hours,
    }
}

fn mean(iter: impl Iterator<Item = f64>) -> f64 {
    let mut n = 0usize;
    let mut sum = 0.0f64;
    for x in iter {
        if x.is_finite() {
            sum += x;
            n += 1;
        }
    }
    if n == 0 {
        f64::NAN
    } else {
        sum / n as f64
    }
}

// ─── Misc ──────────────────────────────────────────────────────

/// First `n` chars of `s`, char-boundary-safe (so we never slice
/// through a multi-byte UTF-8 sequence — atlas content has plenty
/// of accented Latin and non-ASCII transliterations like `al-Fârâbî`).
fn head_of(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn chrono_format_now() -> String {
    // No chrono dep in this crate; format manually so the JSON is
    // self-describing without pulling another crate.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix-{secs}")
}

