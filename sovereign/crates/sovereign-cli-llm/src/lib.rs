// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-llm` — sibling binary that owns svrn's LLM-touching
//! CLI verbs (chat, workflow, govern, ...). Parent `sovereign` shim execs
//! into this binary for those argv[1] values. bench's verbs (`bench`,
//! `eval`, `quality-lane`) are `sovereign-cli-bench`'s since
//! pb-cli-llm-bench-move; svrn's white-box lanes under those spellings stay
//! here (`run_bench_verb`, `run_eval_verb`). ingest's verbs (`enrich`,
//! `corpus`, `atlas`, `meta-atlas`, `recipe`, `pipeline`, `alignment`,
//! `bench atlas`) are `svrn-ingest`'s since pb-cli-llm-ingest-move, except the
//! sub-verbs under those spellings that are svrn's (`enrich_cmd`, `atlas_cmd`,
//! `corpus_cmd` here).
//!
//! Lives apart from `sovereign-cli` (the dispatcher) and
//! `sovereign-cli-dev` (project / code / daemon) so each
//! binary's leaf-edit recompile only touches its own subcommands.
//!
//! ## Why this crate has a `[lib]` target (2026-08-21)
//!
//! It was `[[bin]]`-only until nc-26, and that had a cost nobody was
//! watching. `sovereign-cli`'s [`awareness_cmd`] reached into this crate's
//! private module tree — `use crate::enrich_cmd::inference_client::{…}` from
//! two sites — which has not compiled since the 2026-05-22 slice-5 split
//! moved `enrich_cmd` here and left `awareness_cmd` behind. `cargo check -p
//! sovereign-cli --features awareness` failed with two `E0433` for three
//! months, and no gate noticed because no gate built the feature.
//!
//! The repair is the shape nc-19 used on `sovereign-cli-dev`: the module
//! tree moves into `src/lib.rs`, [`main`](../main.rs) becomes a shim over
//! [`bin_main`], and the code that needed `enrich_cmd` moves to the crate
//! that owns it. `awareness_cmd` is now a sibling of `enrich_cmd` rather
//! than a trespasser on it, so the import resolves by construction.
//!
//! `svrn awareness` is exec'd into the stock distribution's
//! `sovereign-cli-llm-stock` like every other verb here, since
//! pb-cli-llm-ingest-move-remainder: extract and filter write their atlas
//! through ingest's atlas port, which only a composed process holds. The
//! dispatcher no longer links this crate.
//!
//! ## Where the `awareness` feature lives, and why it lives here
//!
//! On THIS crate, and `sovereign-stock/awareness` passes it through for the
//! binary that serves it. A feature belongs to the crate holding the code it
//! gates: while it lived on `sovereign-cli` and the code lived here, the two
//! could not agree, which is the same class of split-brain that produced the
//! two disagreeing module gates the feature already died of once (see
//! `awareness_cmd/mod.rs`). One decider, one name (ARCH §10.6).

mod atlas_cmd;
// UNGATED on purpose. Only `awareness_cmd::args`
// (the flag SPEC — data plus the shared parser) compiles without the feature;
// every heavy submodule carries its own `#[cfg(feature = "awareness")]`.
// Declaring the module here under a gate as well, while the module itself
// carries a second one, is what made `--features awareness` fail to compile at
// all for three months. ONE gate, and it is the inner one. See
// `awareness_cmd/mod.rs`.
pub mod awareness_cmd;
mod chat_cmd;
mod corpus_catalog_cmd;
mod corpus_cmd;
mod corpus_extract_entities_cmd;
use sovereign_cli_base::corpus_resolve;
mod corpus_watch_cmd;
mod enrich_cmd;
mod govern_cmd;
mod gym_judge;
mod inner_chaos;
mod judge_replay;
// `svrn job` — the work plane's operator surface. Sits beside `ring_cmd`
// rather than inside it because they are two verbs on one rail: `ring`
// deploys an app to a trust ring, `job` hands that ring a unit of compute.
// It reaches `ring_cmd::rail_append`, which is the ONE append client
// (ARCH §10.6).
mod knowledge_gym_cmd;
mod legacy_store;
mod mcp_cmd;
mod mcp_demo_server;
pub mod meshapp_cmd;
pub mod meshapp_registry;
mod mobile_cmd;
mod newsworthy_cmd;
mod portfolio_cmd;
/// `svrn __probe`: svrn describes its own internals for bench to judge.
mod probe_cmd;
mod proxy_cmd;
mod reading_diag_cmd;
mod recipe_agent_cmd;
mod recipe_agent_live_trial;
mod resolver_precision;
mod router_cache_cmd;
mod router_fit_cmd;
mod search_gym_cmd;
pub mod serve_dial;
mod turn_sink;
mod voice_eval;
mod workflow_cmd;

use sovereign_cli_shared::tracing_init::init_tracing;

/// The bare sibling binary's entry point: no ingest program composed, so a
/// lane that reads corpora names the absence. `src/main.rs` is a shim over
/// this so the crate has exactly one implementation of its verb table.
pub fn bin_main() {
    bin_main_with(None)
}

/// The entry with ingest composed by a distribution (the stock
/// distribution's `sovereign-cli-llm-stock`, pb-cli-llm-ingest-move-compose),
/// as `process::run` takes it for the daemon.
pub fn bin_main_with(ingest: Option<sovereign_daemon::hosted_ingest::HostedIngest>) {
    chat_cmd::ingest::install(ingest);
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
            "sovereign_cli=info,sovereign_cli_llm=info,\
             commonwealth_discovery=info,sovereign_daemon=info",
        ),
        // `publish` only reads and rewrites config; it dials nothing.
        "publish" | "unpublish" => init_tracing("sovereign_cli_llm=info"),
        // `run` supervises a child and talks to the local daemon over
        // loopback; its own reporting is on stderr, so info is the floor.
        "run" => init_tracing("sovereign_cli_llm=info"),
        // `meshapp dev` serves through the host kit's shell, whose mount
        // trace and bundle-escape refusals are `host_kit` events. cli-mesh
        // installed no subscriber, so both were invisible (phase-b-16).
        "meshapp" => init_tracing("sovereign_cli_llm=info,host_kit=info"),
        "enrich" => init_tracing("sovereign_cli_llm=info,corpus_engine=info"),
        // The filter the dispatcher set when it linked awareness in-process.
        "awareness" => init_tracing("sovereign_cli=info,sovereign_tools=debug,corpus_engine=debug"),
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
        // job: the fold's own refusals. `job status` prints an UNREADABLE
        // COUNT, and the reason each line was refused is the whole answer to
        // "why is my submission not in the fold" — it lives in
        // `commonwealth_work`'s `unreadable`, at debug, and without a
        // subscriber here the terminal could report the count and never the
        // cause (ARCH §9.1). Quiet by default so `--json` stays parseable.
        "job" if std::env::var_os("RUST_LOG").is_some() => {
            init_tracing("sovereign_cli_llm=info,commonwealth_work=debug")
        }
        _ => {}
    }

    let code: i32 = match cmd {
        "bench" => run_bench_verb(rest).await,
        "chat" => chat_cmd::run_chat(rest).await,
        // Execed by the dispatcher (pb-cli-llm-ingest-move-remainder): this
        // process holds ingest's atlas port, which extract and filter write
        // through.
        #[cfg(feature = "awareness")]
        "awareness" => awareness_cmd::run_awareness(rest).await,
        #[cfg(not(feature = "awareness"))]
        "awareness" => {
            eprintln!(
                "awareness: built only under the `awareness` cargo feature\n\
                 (it pulls the heavy knowledge-view surface). Rebuild with\n\
                 `cargo build -p sovereign-stock --features awareness` to enable."
            );
            2
        }
        "govern" => govern_cmd::run_govern(rest).await,
        "proxy" => proxy_cmd::run_proxy(rest).await,
        "portfolio" => portfolio_cmd::run_portfolio(rest).await,
        "eval" => run_eval_verb(rest).await,
        "voice" => voice_eval::run_voice_eval(rest).await,
        "reading-diag" => reading_diag_cmd::run(rest).await,
        "search-gym" => search_gym_cmd::run_search_gym(rest).await,
        "knowledge-gym" => knowledge_gym_cmd::run_knowledge_gym(rest).await,
        "atlas" => atlas_cmd::run_atlas(rest).await,
        "enrich" => enrich_cmd::run_enrich(rest).await,
        "newsworthy" => newsworthy_cmd::run(rest).await,
        "recipe-agent" => recipe_agent_cmd::run_recipe_agent(rest).await,
        "maintainer" => recipe_agent_cmd::run_maintainer(rest).await,
        "router-cache" => router_cache_cmd::run(rest).await,
        "router" => router_fit_cmd::run(rest).await,
        "workflow" => workflow_cmd::run_workflow(rest).await,
        "mcp" => mcp_cmd::run_mcp(rest).await,
        "meshapp" => meshapp_cmd::run(rest).await,
        "mobile" => mobile_cmd::run_mobile(rest).await,
        "corpus" => corpus_cmd::run_corpus(rest).await,
        // ingest's since pb-cli-llm-ingest-move; the dispatcher execs svrn-ingest.
        "meta-atlas" | "recipe" | "pipeline" | "alignment" => ingest_verb_elsewhere(cmd, ""),
        // One lane of `svrn quality check`: bench's since
        // pb-cli-llm-bench-move, and the dispatcher execs sovereign-cli-bench.
        "quality-lane" => bench_verb_elsewhere("quality-lane", rest),
        // Hidden: the probe `eval run`'s white-box modes exec and score.
        "__probe" => probe_cmd::run(rest).await,
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

/// `eval`'s dispatch. `inner-chaos` is svrn's white-box test of itself and
/// stays here; eval_cmd is bench's and must name no svrn-side module
/// (phase-b-55), so the arm is routed before eval_cmd sees the args.
async fn run_eval_verb(rest: &[String]) -> i32 {
    match rest.first().map(String::as_str) {
        Some("inner-chaos") => inner_chaos::run_inner_chaos(&rest[1..]).await,
        _ => bench_verb_elsewhere("eval", rest),
    }
}

/// `bench`'s dispatch. `judge-replay` and `resolver-precision` replay svrn's
/// own grounding gate (phase-b-60), and `atlas` composes and parses ingest's
/// own Phase 1 prompts in-process (phase-b-64): they stay here as white-box
/// tests; bench_cmd is bench's and names no svrn-side module, so the three
/// arms are routed before bench_cmd sees the args. bench_cmd's HELP still
/// lists them.
async fn run_bench_verb(rest: &[String]) -> i32 {
    match rest.first().map(String::as_str) {
        Some("judge-replay") => judge_replay::cmd_judge_replay(&rest[1..]).await,
        Some("resolver-precision") => resolver_precision::cmd_resolver_precision(&rest[1..]).await,
        // ingest's white-box lane; the dispatcher execs svrn-ingest for it.
        Some("atlas") => ingest_verb_elsewhere("bench", "atlas"),
        _ => bench_verb_elsewhere("bench", rest),
    }
}

/// A bench verb reached this binary directly. It is bench's own
/// (`sovereign-cli-bench`, pb-cli-llm-bench-move) and the dispatcher execs it
/// there, so this names where it went rather than answering unknown.
fn bench_verb_elsewhere(verb: &str, rest: &[String]) -> i32 {
    let sub = rest.first().map(String::as_str).unwrap_or("");
    tracing::debug!(verb, sub, "bench verb reached sovereign-cli-llm");
    eprintln!(
        "sovereign-cli-llm: `{verb} {sub}` is bench's; it runs in sovereign-cli-bench. \
         Run it as `svrn {verb} {sub}`."
    );
    2
}

/// A verb or sub-verb that is ingest's since pb-cli-llm-ingest-move: a named
/// pointer, exit 2, never "unknown".
pub(crate) fn ingest_verb_elsewhere(verb: &str, sub: &str) -> i32 {
    tracing::debug!(verb, sub, "ingest verb reached sovereign-cli-llm");
    let spelled = if sub.is_empty() {
        verb.to_string()
    } else {
        format!("{verb} {sub}")
    };
    eprintln!(
        "sovereign-cli-llm: `{spelled}` is ingest's; it runs in svrn-ingest. \
         Run it as `svrn {spelled}`."
    );
    2
}

#[cfg(test)]
mod bench_dispatch {
    /// svrn's white-box lanes answer their own `--help` with 0 through
    /// `bench`'s dispatch; bench's own verbs, which run in sovereign-cli-bench,
    /// and ingest's `bench atlas`, which runs in svrn-ingest, answer 2 with the
    /// place they went.
    #[tokio::test]
    async fn bench_gate_replays_are_answered_svrn_side() {
        let args: Vec<String> = ["atlas", "--help"].map(String::from).to_vec();
        assert_eq!(super::run_bench_verb(&args).await, 2);
        for verb in ["judge-replay", "resolver-precision"] {
            let args: Vec<String> = [verb, "--help"].map(String::from).to_vec();
            assert_eq!(super::run_bench_verb(&args).await, 0, "{verb}");
        }
        let args: Vec<String> = ["all", "--help"].map(String::from).to_vec();
        assert_eq!(super::run_bench_verb(&args).await, 2);
    }
}

#[cfg(test)]
mod eval_dispatch {
    #[tokio::test]
    async fn eval_inner_chaos_help_is_answered_by_inner_chaos() {
        let args: Vec<String> = ["inner-chaos", "--help"].map(String::from).to_vec();
        // inner_chaos answers its own --help with 0; bench's `eval run`, which
        // runs in sovereign-cli-bench, answers 2.
        assert_eq!(super::run_eval_verb(&args).await, 0);
        let args: Vec<String> = ["run", "--help"].map(String::from).to_vec();
        assert_eq!(super::run_eval_verb(&args).await, 2);
    }
}
