//! `sovereign eval ...` — measure retrieval quality against a question
//! bank.
//!
//! The eval command is the measurement substrate for everything we
//! tune in the corpus pipeline: filter scope, chunker config,
//! embedding model, retrieval limits, eventually atlas-seeded
//! re-ranking. Run it once to baseline, change one knob, run it again,
//! diff. Without this loop, "did the change help?" is a vibes call
//! against whatever question the developer typed last.
//!
//! The runner reuses `chat_cmd::bootstrap::ChatSession` so it talks to
//! the same daemon your desktop app uses. That keeps eval honest:
//! whatever model is loaded for chat is what the bank measures.
//!
//! Subcommands:
//!   - `run`  — execute a bank, print results, optionally write JSON
//!   - `diff` — compare two run-output JSON files (planned; not in v1)
//!
//! By default the runner is retrieval-only — it does NOT call
//! `/v1/chat/completions`. That keeps the cheap baseline cheap and
//! isolates retrieval-tuning experiments from chat-model variance.
//! Pass `--synth` to drive each question through the full
//! `Runtime::handle_message_stream` path the desktop chat surface uses
//! (intent classifier → router → search tools → prompt assembly →
//! chat completion); facts are then matched against the synthesised
//! answer rather than the retrieved chunks. Synth mode exercises the
//! routing and aggregation layers, which are tunable in their own
//! right.

pub mod bank;
pub mod report;
pub mod runner;
pub mod runner_threads;
pub mod score;

use runner_threads as threads;

use std::path::PathBuf;

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign eval",
    summary: "Run a question bank against a corpus; measure retrieval quality.",
    sections: &[
        HelpSection::Usage("sovereign eval <subcommand> [args]"),
        HelpSection::Subcommands(&[
            ("run", "Execute a bank and print per-question + rollup scores."),
        ]),
        HelpSection::Notes(
            "Operates against the running daemon at localhost:9741 (override with \
             --daemon). Retrieval-only — does not call the chat model. The bank \
             format lives at sovereign-recipes/<corpus>/eval/*.toml.",
        ),
    ],
};

const RUN_HELP: Help = Help {
    command: "sovereign eval run",
    summary: "Run a question bank, print per-question results + category rollup.",
    sections: &[
        HelpSection::Usage(
            "sovereign eval run --bank <path> [--synth] [--limit N] [--inspect] [--format text|json] [--output <path>]",
        ),
        HelpSection::Flags(&[
            ("--bank <path>",  "Path to the bank TOML (e.g. sovereign-recipes/wikipedia/eval/wikipedia_questions.toml)."),
            ("--synth",        "Drive each question through the full chat pipeline (routing → retrieval → synthesis). Slower, but exercises the model + routing layers."),
            ("--routing-only", "Call the classifier per question and score the routing decision against `expected_intent` (or category default). Skips retrieval and synthesis — fast iteration loop for tuning the classifier prompt."),
            ("--limit <N>",    "Top-N chunks to retrieve per question (retrieval mode only; default: 10)."),
            ("--inspect",      "Print missing facts/sources + top retrieved chunks per question."),
            ("--no-judge",     "Skip the LLM-as-judge \"instructor mode\" pass under --synth. Default: judge runs alongside the strict scorer to catch paraphrased coverage."),
            ("--threads",      "Multi-turn mode. Bank must be a `[[threads]]` shape (see sovereign/bench/wikipedia_learn/threads.toml). Each thread walks N follow-up turns under one conversation_id; per-turn deterministic scoring + one thread-level LLM judge call. Implies --synth pipeline. Disables --routing-only / --with-atlas / retrieval-only mode."),
            ("--thread-id <id>", "Filter --threads mode to a single thread by id. Useful for fast iteration on one fixture (e.g. `--thread-id computing_history`)."),
            ("--format text|json", "Stdout format (default: text)."),
            ("--output <path>", "Also write the full run as pretty JSON to this path."),
            ("--with-atlas <ids>", "Comma-separated list of atlas corpus ids; each is embedded once and the per-question retrieval pools their entries via global cosine top-K (the multi-article SEP pilot path). One id is the single-atlas case. Off by default."),
            ("--atlas-top-k <N>", "Top-K atlas matches injected per question (default 3). Only used with --with-atlas."),
            ("--atlas-min-description-chars <N>", "Skip entities whose description is shorter than N chars (default 200 — keeps actually-enriched entities; pass 0 to embed every non-placeholder which can mean ~40min on wiki-scale atlases)."),
            ("--atlas-depth <list>", "Comma-separated enrichment_depth allowlist (e.g. `extracted` or `extracted,structural_classified`). Empty = accept any depth."),
            ("--atlas-max-entries <N>", "Hard cap on the number of entities embedded. Useful as a safety net on misconfigured runs."),
            ("--atlas-include <list>", "Comma-separated atom/edge kinds to surface from the atlas in addition to entities. `claim` (Path 2 Phase A) surfaces Claim atoms as virtual chunks framed as `[Claim: act, status] content`. `tension` (Phase B) surfaces Tension edges as virtual chunks framed as `[Tension] sub_question` followed by both endpoint atoms — feeds the dialectical_breadth axis. `configuration` (Phase C) surfaces Configuration atoms (the interpretive shape of the article as a whole) framed as `[Configuration: label] description` — feeds the argument_depth axis."),
            ("--loose-source-judge", "After rigid scoring, ask a fast-slot LLM whether each *missing* expected_source is materially covered by the retrieved chunks (paraphrase / canonical-sibling / indirect coverage all count). Strict superset of the rigid score — never lowers it. ~one fast-slot call per question."),
            ("--essay-judge", "Multi-axis 0–3 LLM scorer over the retrieved set: topical_coverage, position_attribution, dialectical_breadth, argument_depth (total 0–12). Answers \"does the bag have essay-worthy substance?\" rather than \"are the right articles in it?\". ~one fast-slot call per question."),
            ("--help, -h",     "Show this message."),
        ]),
        HelpSection::Notes(
            "All `chat` global flags also apply: --daemon, --data-dir, --chat-model, \
             --embed-model, --temperature, --max-tokens. The bank's `corpus` field \
             MUST match an installed corpus_id; install it first via \
             `sovereign corpus install <id>`. Under --synth, --chat-model selects the \
             model that will do synthesis; --max-tokens lets you sweep the \
             latency/coverage tradeoff (lower = faster wall, terser answer) without \
             touching the operator's product config.",
        ),
    ],
};

pub async fn run_eval(args: &[String]) -> i32 {
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    let first = args[0].as_str();
    if first == "--help" || first == "-h" || first == "help" {
        help::print(&HELP);
        return 0;
    }
    match first {
        "run" => cmd_run(&args[1..]).await,
        other => {
            eprintln!("error: unknown subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

#[derive(Debug, Clone)]
enum OutputFormat {
    Text,
    Json,
}

struct RunArgs {
    bank: PathBuf,
    limit: usize,
    inspect: bool,
    synth: bool,
    routing_only: bool,
    format: OutputFormat,
    output: Option<PathBuf>,
    /// Skip the LLM-as-judge "instructor mode" pass. Default: judge runs
    /// alongside the strict scorer in synth mode. Use this on
    /// fast-iteration loops where the strict score alone is enough.
    no_judge: bool,
    /// Atlas corpus id whose `atoms.json` will be loaded, embedded
    /// once, and fused into per-question retrieval as virtual chunks.
    /// Off by default — atlas content is not part of retrieval unless
    /// explicitly opted in.
    with_atlas: Option<String>,
    /// How many atlas Entity matches to fuse per question. Default
    /// keeps the merged top-K largely chunk-driven while letting the
    /// strongest entity grounding through.
    atlas_top_k: usize,
    /// Drop entities whose `description` is shorter than this many
    /// chars. Default 200 — keeps actually-enriched entities, drops
    /// structural one-liners ("X is a Y born in Z."). Set to 0 to
    /// embed every non-placeholder entity (slow on wiki-scale atlases:
    /// 50K+ entities × ~50ms/embed = ~40 min).
    atlas_min_description_chars: usize,
    /// Optional comma-separated `enrichment_depth` allowlist (e.g.
    /// `extracted` or `extracted,structural_classified`). Empty =
    /// accept any depth.
    atlas_depth: Vec<String>,
    /// Hard cap on number of entities to embed. None = unlimited.
    atlas_max_entries: Option<usize>,
    /// Path 2 — comma-separated atom kinds to surface from the atlas.
    /// `entity` is always included implicitly. Additional values:
    /// `claim` (Phase A — substantive proposition atoms with
    /// discourse_act + epistemic_status). Future Phase B/C will add
    /// `tension` and `configuration`. Default empty = entities only.
    atlas_include_kinds: Vec<String>,
    /// Run an LLM-as-judge "loose source-credit" pass after rigid
    /// scoring. For each question's *missing* expected_sources (titles
    /// that didn't match retrieved chunks under `normalize_title`
    /// folding), ask the fast slot whether the topic IS materially
    /// covered by the retrieved chunks — paraphrase / canonical-sibling
    /// / indirect coverage all count. Strict superset of the rigid
    /// score, never lowers it. Costs ~one fast-slot call per question.
    /// Off by default. See `score::score_sources_loose`.
    loose_source_judge: bool,
    /// Run a multi-axis essay-readiness judge over the retrieved set.
    /// Scores 0–3 on each of four axes (topical_coverage,
    /// position_attribution, dialectical_breadth, argument_depth),
    /// total 0–12. Where loose-source-judge answers "are the right
    /// articles in the bag?", this answers "does the bag have what an
    /// undergraduate essay needs?" — substance, not just recall.
    /// Costs ~one fast-slot call per question. Off by default. See
    /// `score::score_essay_readiness`.
    essay_judge: bool,
    /// Multi-turn thread bench. Bank file carries `[[threads]]` with
    /// nested `[[threads.turns]]`. Each thread is replayed under one
    /// `conversation_id` so retrieval and synthesis see prior turns'
    /// history. Per-turn scores are deterministic; the LLM judge runs
    /// ONCE per thread over the full transcript. See
    /// `eval_cmd::runner_threads`.
    threads: bool,
    /// Optional thread-id filter for `--threads` mode. When set, the
    /// runner executes only threads whose `id` matches. Useful for
    /// fast iteration on a single fixture (e.g. the marathon thread)
    /// without paying the cost of running the whole bank.
    thread_id_filter: Option<String>,
}

impl Default for RunArgs {
    fn default() -> Self {
        Self {
            bank: PathBuf::new(),
            limit: 10,
            inspect: false,
            synth: false,
            routing_only: false,
            format: OutputFormat::Text,
            output: None,
            no_judge: false,
            with_atlas: None,
            atlas_top_k: 3,
            atlas_min_description_chars: 200,
            atlas_depth: Vec::new(),
            atlas_max_entries: None,
            atlas_include_kinds: Vec::new(),
            loose_source_judge: false,
            essay_judge: false,
            threads: false,
            thread_id_filter: None,
        }
    }
}

async fn cmd_run(args: &[String]) -> i32 {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        help::print(&RUN_HELP);
        return 0;
    }

    let (mut globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    // Eval is a rule-following measurement, not a free-form chat —
    // sampling variance from a non-zero temperature shows up as
    // run-to-run noise on the bank metrics, swamping the signal we
    // care about (did the routing/retrieval/synthesis change actually
    // move the score?). Default to temperature 0 unless the operator
    // passed `--temperature` explicitly. Same logic applies to
    // retrieval-only mode (route classifier + Fast-slot calls in any
    // path the runtime takes) so we set it on `globals` regardless of
    // mode rather than only under `--synth`.
    if globals.temperature.is_none() {
        globals.temperature = Some(0.0);
    }

    let mut a = RunArgs::default();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--bank" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("error: --bank needs a value");
                    return 2;
                };
                a.bank = PathBuf::from(v);
            }
            "--limit" => {
                i += 1;
                a.limit = rest
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(a.limit);
            }
            "--inspect" => {
                a.inspect = true;
            }
            "--synth" => {
                a.synth = true;
            }
            "--routing-only" => {
                a.routing_only = true;
            }
            "--no-judge" => {
                a.no_judge = true;
            }
            "--threads" => {
                a.threads = true;
            }
            "--thread-id" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("error: --thread-id needs a value");
                    return 2;
                };
                a.thread_id_filter = Some(v.clone());
            }
            "--format" => {
                i += 1;
                match rest.get(i).map(String::as_str) {
                    Some("text") => a.format = OutputFormat::Text,
                    Some("json") => a.format = OutputFormat::Json,
                    Some(other) => {
                        eprintln!("error: --format expects text|json, got `{other}`");
                        return 2;
                    }
                    None => {
                        eprintln!("error: --format needs a value");
                        return 2;
                    }
                }
            }
            "--output" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("error: --output needs a value");
                    return 2;
                };
                a.output = Some(PathBuf::from(v));
            }
            "--with-atlas" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("error: --with-atlas needs an atlas-corpus-id value");
                    return 2;
                };
                a.with_atlas = Some(v.clone());
            }
            "--atlas-top-k" => {
                i += 1;
                a.atlas_top_k = rest
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(a.atlas_top_k);
            }
            "--atlas-min-description-chars" => {
                i += 1;
                a.atlas_min_description_chars = rest
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(a.atlas_min_description_chars);
            }
            "--atlas-depth" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("error: --atlas-depth needs a comma-separated value");
                    return 2;
                };
                a.atlas_depth = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--atlas-max-entries" => {
                i += 1;
                a.atlas_max_entries = rest.get(i).and_then(|s| s.parse().ok());
            }
            "--atlas-include" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("error: --atlas-include needs a comma-separated value (e.g. `claim`)");
                    return 2;
                };
                a.atlas_include_kinds = v
                    .split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--loose-source-judge" => {
                a.loose_source_judge = true;
            }
            "--essay-judge" => {
                a.essay_judge = true;
            }
            extra => {
                eprintln!("error: unexpected argument `{extra}`");
                return 2;
            }
        }
        i += 1;
    }

    if a.bank.as_os_str().is_empty() {
        eprintln!("error: --bank is required");
        eprintln!("hint: try sovereign-recipes/wikipedia/eval/wikipedia_questions.toml");
        return 2;
    }

    // Thread bench short-circuits the rest of the flow. The bank
    // shape is different (`[[threads]]` vs `[[questions]]`), the
    // runner is different (`run_thread_bank`), and atlas / routing-
    // only modes don't apply.
    if a.threads {
        if a.routing_only {
            eprintln!("error: --threads is incompatible with --routing-only");
            return 2;
        }
        let mut bank = match bank::load_thread_bank(&a.bank) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: load thread bank: {e}");
                return 1;
            }
        };
        if let Some(filter) = a.thread_id_filter.as_deref() {
            let before = bank.threads.len();
            bank.threads.retain(|t| t.id == filter);
            if bank.threads.is_empty() {
                eprintln!(
                    "error: --thread-id `{filter}` matched no threads (bank has {before})"
                );
                return 2;
            }
            eprintln!(
                "filter: --thread-id `{filter}` → kept {} of {before} threads",
                bank.threads.len()
            );
        }
        let total_turns: usize = bank.threads.iter().map(|t| t.turns.len()).sum();
        eprintln!(
            "loaded thread bank `{}` — {} threads, {total_turns} turns, target corpus `{}`",
            bank.bank.name, bank.threads.len(), bank.bank.corpus,
        );
        crate::util::tracing_init::init_tracing(
            "sovereign_cli=info,sovereign_tools::atlas_context_manager=info,\
             sovereign_tools::knowledge_view=warn",
        );
        let session = match build_session(&globals).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("bootstrap failed: {e}");
                return 1;
            }
        };
        let run = threads::run_thread_bank(&session, &bank, !a.no_judge).await;
        match a.format {
            OutputFormat::Text => threads::print_threads_text(&run),
            OutputFormat::Json => {
                if let Err(e) = threads::print_threads_json(&run) {
                    eprintln!("error: {e}");
                    return 1;
                }
            }
        }
        if let Some(path) = a.output.as_deref() {
            if let Err(e) = threads::write_threads_json_file(&run, path) {
                eprintln!("error: write output: {e}");
                return 1;
            }
            eprintln!("wrote thread run JSON to {}", path.display());
        }
        return 0;
    }

    let bank = match bank::load_bank(&a.bank) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: load bank: {e}");
            return 1;
        }
    };
    eprintln!(
        "loaded bank `{}` — {} questions, target corpus `{}`",
        bank.bank.name,
        bank.questions.len(),
        bank.bank.corpus,
    );

    // Initialize tracing so the atlas-context manager + other
    // background-init logs surface to stderr. Default filter is
    // chatty enough to see the atlas-context lifecycle without
    // drowning in lance-internal trace.
    crate::util::tracing_init::init_tracing(
        "sovereign_cli=info,sovereign_tools::atlas_context_manager=info,\
         sovereign_tools::knowledge_view=warn",
    );

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    if a.routing_only {
        eprintln!(
            "routing-only mode — calling the classifier per question, scoring against \
             expected_intent (or category default). No retrieval, no synthesis. \
             Useful for tuning the classifier prompt against a specific fast-slot model."
        );
        let run = match runner::run_bank_routing(&session, &bank).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        report::print_routing(&run);
        if let Some(path) = a.output.as_deref() {
            if let Err(e) = report::write_routing_json_file(&run, path) {
                eprintln!("error: write output: {e}");
                return 1;
            }
            eprintln!("wrote routing JSON to {}", path.display());
        }
        return 0;
    }

    let atlas_ctxs: Vec<sovereign_core::atlas_context::AtlasContext> = if let Some(id_list) =
        a.with_atlas.as_deref()
    {
        let include_claims = a
            .atlas_include_kinds
            .iter()
            .any(|k| k == "claim");
        let include_tensions = a
            .atlas_include_kinds
            .iter()
            .any(|k| k == "tension");
        let include_configurations = a
            .atlas_include_kinds
            .iter()
            .any(|k| k == "configuration");
        // Surface unknown kinds as a warning so typos don't silently
        // produce an entities-only run.
        for k in &a.atlas_include_kinds {
            if !matches!(
                k.as_str(),
                "claim" | "entity" | "tension" | "configuration"
            ) {
                eprintln!(
                    "warn: --atlas-include `{k}` is not yet recognised; \
                     accepted today: entity, claim, tension, configuration."
                );
            }
        }
        let filter = runner::AtlasLoadFilter {
            min_description_chars: a.atlas_min_description_chars,
            depth_allowlist: a.atlas_depth.clone(),
            max_entries: a.atlas_max_entries,
            include_claims,
            include_tensions,
            include_configurations,
        };
        // `--with-atlas` accepts a comma-separated list of atlas
        // corpus ids. Each loads independently (with its own
        // canonical_name = article_slug derivation) and the per-question
        // retrieval pools their entries via `atlas_top_k_across`. This
        // is the multi-article SEP-pilot path: enrich N per-article
        // atlases, point one --with-atlas at all of them, let the
        // global cosine pick the topically-aligned surfaces.
        let mut out = Vec::new();
        for id in id_list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match runner::load_atlas_context(&session, id, a.atlas_top_k, &filter).await {
                Ok(ctx) => out.push(ctx),
                Err(e) => {
                    eprintln!("error: --with-atlas {id}: {e}");
                    return 1;
                }
            }
        }
        out
    } else {
        Vec::new()
    };

    // Load the structural graph layer for each atlas (atoms-by-id,
    // edge adjacency). Used by `atlas_navigate` for graph BFS — the
    // substantive layer of the atlas that bag-of-atoms cosine
    // retrieval ignores. Cheap: just parses atoms.json + edges.json
    // already on disk from build time.
    let atlas_graphs: Vec<runner::AtlasGraph> = atlas_ctxs
        .iter()
        .filter_map(|ctx| {
            let atlas_dir =
                crate::enrich_cmd::paths::index_root(&ctx.atlas_corpus_id)
                    .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME);
            match runner::AtlasGraph::load_from_disk(&ctx.atlas_corpus_id, &atlas_dir) {
                Ok(g) => Some(g),
                Err(e) => {
                    eprintln!("warn: atlas-graph load `{}`: {e}", ctx.atlas_corpus_id);
                    None
                }
            }
        })
        .collect();

    let run = if a.synth {
        eprintln!(
            "synth mode — driving full chat pipeline. This will take ~one chat-completion \
             per question; sit tight."
        );
        if !atlas_ctxs.is_empty() {
            eprintln!(
                "note: --with-atlas is ignored under --synth (synth path uses runtime \
                 retrieval, not the eval runner's chunk search)."
            );
        }
        match runner::run_bank_synth(&session, &bank, !a.no_judge).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        match runner::run_bank(
            &session,
            &bank,
            a.limit,
            &atlas_ctxs,
            &atlas_graphs,
            a.loose_source_judge,
            a.essay_judge,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    };

    match a.format {
        OutputFormat::Text => report::print_text(&run, a.inspect, Some(&bank)),
        OutputFormat::Json => {
            if let Err(e) = report::print_json(&run) {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }

    if let Some(path) = a.output.as_deref() {
        if let Err(e) = report::write_json_file(&run, path) {
            eprintln!("error: write output: {e}");
            return 1;
        }
        eprintln!("wrote run JSON to {}", path.display());
    }

    0
}
