// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-llm` — sibling binary that owns every LLM-touching
//! CLI verb (bench/chat/eval/atlas/enrich/recipe/pipeline/mesh/...) +
//! every corpus_* dispatcher. Parent `sovereign` shim execs into this
//! binary for those argv[1] values.
//!
//! Lives apart from `sovereign-cli` (the dispatcher) and
//! `sovereign-cli-atos` (project / atos / code / daemon) so each
//! binary's leaf-edit recompile only touches its own subcommands.

mod alignment_cmd;
mod atlas_cmd;
mod bench_cmd;
mod chat_cmd;
mod claim_cmd;
mod corpus_catalog_cmd;
mod corpus_cmd;
mod corpus_extract_entities_cmd;
mod corpus_resolve;
mod corpus_scrub_cmd;
mod corpus_snapshot_cmd;
mod corpus_watch_cmd;
mod enrich_cmd;
mod eval_cmd;
mod govern_cmd;
mod gym_judge;
mod inner_chaos;
mod knowledge_gym_cmd;
mod mcp_cmd;
mod mcp_demo_server;
mod mesh_bench;
mod mesh_cmd;
mod mesh_soak;
mod meshapp_cmd;
mod meshapp_registry;
mod meta_atlas_cmd;
mod mobile_cmd;
mod newsworthy_cmd;
mod pipeline_cmd;
mod portfolio_cmd;
mod proxy_cmd;
mod reading_diag_cmd;
mod recipe_agent_cmd;
mod recipe_agent_live_trial;
mod recipe_cmd;
mod router_cache_cmd;
mod router_fit_cmd;
mod search_gym_cmd;
mod solve_cmd;
mod voice_eval;
mod worker_pod_provider;
mod workflow_cmd;

use sovereign_cli_shared::tracing_init::init_tracing;

fn main() {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }
    // Rebrand back-compat (see sovereign_core::rebrand): idempotent, non-destructive.
    sovereign_core::rebrand::promote_legacy_env();
    sovereign_core::rebrand::run_startup_migration();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-cli-llm-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(async_main());
}

async fn async_main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = raw_args.first().map(|s| s.as_str()).unwrap_or("");
    let rest: &[String] = if raw_args.is_empty() {
        &[]
    } else {
        &raw_args[1..]
    };

    // Tracing init for the long-running / streaming paths. Matches
    // the configs sovereign-cli used pre-split.
    match cmd {
        "mesh" => init_tracing(
            "sovereign_cli=info,sovereign_cli_llm=info,sovereign_mesh=info,\
             commonwealth_discovery=info,commonwealth_api=info",
        ),
        "pipeline" => init_tracing("sovereign_cli_llm=info,sovereign_pipeline=info"),
        "enrich" => init_tracing("sovereign_cli_llm=info,corpus_engine=info"),
        "voice" | "search-gym" | "knowledge-gym" => init_tracing("sovereign_cli_llm=info"),
        // The bench verbs print their own [chaos]/[parity] summaries via eprintln
        // and stay quiet by default (no subscriber) so harnesses parsing their
        // stderr aren't disturbed. But they gain the full tracing glassbox when
        // RUST_LOG is explicitly set — so a measurement run can be debugged
        // (e.g. `RUST_LOG=retrieval_audit=info` to watch the atom-enum /
        // atlas-grounding retrieval decisions, or `agentic_kq=info`) on demand.
        "bench" if std::env::var_os("RUST_LOG").is_some() => init_tracing("sovereign_cli_llm=info"),
        // chat: glassbox the grounded synth/gate lifecycle on demand (truncation
        // trace 2026-06-30) — quiet by default so `--format json` stays parseable.
        "chat" if std::env::var_os("RUST_LOG").is_some() => {
            init_tracing("sovereign_cli_llm=info,sovereign_core=info")
        }
        // eval: same on-demand glassbox as chat — e.g.
        // `RUST_LOG=memory_grounding=info` to watch the recall grounding
        // gate + sticky-pin lifecycle during inner-chaos runs.
        "eval" if std::env::var_os("RUST_LOG").is_some() => {
            init_tracing("sovereign_cli_llm=info,sovereign_core=info")
        }
        // workflow: quiet by default so the run summary + `## item` bodies stay
        // clean for piping, but glassbox the runner on demand — including the
        // B:P9a decision of whether the chat context window + embed prefix came
        // from the host's OICP manifest or the v0.3 fallback.
        "workflow" if std::env::var_os("RUST_LOG").is_some() => init_tracing(
            "sovereign_cli_llm=info,sovereign_workflow_host=info,sovereign_workflow=info",
        ),
        _ => {}
    }

    let code: i32 = match cmd {
        "bench" => bench_cmd::run_bench(rest).await,
        "chat" => chat_cmd::run_chat(rest).await,
        "govern" => govern_cmd::run_govern(rest).await,
        "proxy" => proxy_cmd::run_proxy(rest).await,
        "portfolio" => portfolio_cmd::run_portfolio(rest).await,
        "claim" => claim_cmd::run(rest).await,
        "solve" => solve_cmd::run(rest).await,
        "eval" => eval_cmd::run_eval(rest).await,
        "voice" => voice_eval::run_voice_eval(rest).await,
        "reading-diag" => reading_diag_cmd::run(rest).await,
        "search-gym" => search_gym_cmd::run_search_gym(rest).await,
        "knowledge-gym" => knowledge_gym_cmd::run_knowledge_gym(rest).await,
        "atlas" => atlas_cmd::run_atlas(rest).await,
        "meta-atlas" => meta_atlas_cmd::run_meta_atlas(rest).await,
        "meshapp" => meshapp_cmd::run(rest).await,
        "enrich" => enrich_cmd::run_enrich(rest).await,
        "newsworthy" => newsworthy_cmd::run(rest).await,
        "recipe" => recipe_cmd::run_recipe(rest).await,
        "recipe-agent" => recipe_agent_cmd::run_recipe_agent(rest).await,
        "maintainer" => recipe_agent_cmd::run_maintainer(rest).await,
        "router-cache" => router_cache_cmd::run(rest).await,
        "router" => router_fit_cmd::run(rest).await,
        "pipeline" => pipeline_cmd::run_pipeline(rest).await,
        "workflow" => workflow_cmd::run_workflow(rest).await,
        "mcp" => mcp_cmd::run_mcp(rest).await,
        "alignment" => alignment_cmd::run_alignment(rest).await,
        "mesh" => mesh_cmd::run_mesh(rest).await,
        "mobile" => mobile_cmd::run_mobile(rest).await,
        "corpus" => corpus_cmd::run_corpus(rest).await,
        "" => {
            eprintln!("sovereign-cli-llm: usage: sovereign-cli-llm <subcommand> [args...]");
            2
        }
        other => {
            eprintln!("sovereign-cli-llm: unknown subcommand '{other}'");
            2
        }
    };

    std::process::exit(code);
}
