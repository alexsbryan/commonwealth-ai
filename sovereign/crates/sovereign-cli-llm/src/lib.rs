// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-llm` — sibling binary that owns every LLM-touching
//! CLI verb (bench/chat/eval/atlas/enrich/recipe/pipeline/mesh/...) +
//! every corpus_* dispatcher. Parent `sovereign` shim execs into this
//! binary for those argv[1] values.
//!
//! Lives apart from `sovereign-cli` (the dispatcher) and
//! `sovereign-cli-atos` (project / atos / code / daemon) so each
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
//! `sovereign-cli` LINKS this crate — `#[cfg(feature = "awareness")]` only —
//! to serve `svrn awareness` in its own process. No exec hop: nc-19's whole
//! deliverable was making one verb stop paying for one, and adding one back
//! here would spend that.
//!
//! ## Where the `awareness` feature lives, and why it lives here
//!
//! On THIS crate, and `sovereign-cli/awareness` is a pass-through that also
//! turns on the link. A feature belongs to the crate holding the code it
//! gates: while it lived on `sovereign-cli` and the code lived here, the two
//! could not agree, which is the same class of split-brain that produced the
//! two disagreeing module gates the feature already died of once (see
//! `awareness_cmd/mod.rs`). One decider, one name (ARCH §10.6).

mod alignment_cmd;
mod atlas_cmd;
// UNGATED on purpose, and `pub` because `sovereign-cli` calls
// `awareness_cmd::run_awareness` across the link. Only `awareness_cmd::args`
// (the flag SPEC — data plus the shared parser) compiles without the feature;
// every heavy submodule carries its own `#[cfg(feature = "awareness")]`.
// Declaring the module here under a gate as well, while the module itself
// carries a second one, is what made `--features awareness` fail to compile at
// all for three months. ONE gate, and it is the inner one. See
// `awareness_cmd/mod.rs`.
pub mod awareness_cmd;
mod backlog_cmd;
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
mod guest_link;
mod gym_judge;
mod inner_chaos;
mod knowledge_gym_cmd;
mod mcp_cmd;
mod mcp_demo_server;
mod mesh_bench;
mod mesh_cmd;
mod mesh_guest;
mod mesh_member_cmd;
mod mesh_soak;
mod mesh_travel;
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
mod remote_gguf;
mod router_cache_cmd;
mod router_fit_cmd;
mod search_gym_cmd;
mod solve_cmd;
mod turn_sink;
mod voice_eval;
mod worker_pod_provider;
mod workflow_cmd;

use sovereign_cli_shared::tracing_init::init_tracing;

/// The sibling binary's entry point. `src/main.rs` is a shim over this so the
/// crate has exactly one implementation of its verb table.
pub fn bin_main() {
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
        "backlog" => backlog_cmd::run_backlog(rest).await,
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

#[cfg(test)]
mod backstage_boundary {
    //! The MODULE-level half of the back-of-house rule.
    //!
    //! `quality/ARCH_LAYERS.toml` declares `sovereign-eval` back-of-house and
    //! forbids product crates from carrying it in the default build. This
    //! crate carries it anyway, and has an `[[exception]]` saying so, because
    //! `bench_cmd` (51 files, ~31k lines) shares the crate with `chat_cmd`,
    //! `corpus_cmd` and `mesh_cmd`. Splitting it out needs a `[lib]` target
    //! over ~130k lines and pub-visibility churn through the three heaviest
    //! modules here — priced, and not paid for.
    //!
    //! What IS enforceable meanwhile is containment: exactly one module may
    //! name the instrument. That keeps the eventual crate split a move rather
    //! than an excavation, and turns "we meant to keep this in bench_cmd" from
    //! a thing someone remembers into a thing that fails (ARCH §7 — structural,
    //! not remembered).
    //!
    //! This is strictly weaker than a crate boundary: Cargo still LINKS the
    //! harness crate into the shipped binary. The test cannot fix that and does
    //! not claim to.

    /// The one module allowed to name the back-of-house instrument.
    const ALLOWED: &str = "bench_cmd";

    #[test]
    fn bench_cmd_is_the_only_module_naming_the_eval_harness() {
        // Assembled at runtime, never written as a literal. THIS FILE IS INSIDE
        // THE TREE BEING SCANNED, so a literal here would make the guard match
        // itself and fail on its own source — which is exactly what happened on
        // the first cut. Keep the token out of this file, including test names
        // and assertion text.
        let needle = ["sovereign", "eval"].join("_");

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let rel = path.strip_prefix(&src).unwrap_or(&path).to_path_buf();
                scanned += 1;
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if text.contains(&needle)
                    && !rel.starts_with(ALLOWED)
                    && rel.file_stem().is_none_or(|s| s != ALLOWED)
                {
                    offenders.push(rel.display().to_string());
                }
            }
        }

        // An empty walk would pass while proving nothing — the classic
        // zero-case false green (ARCH §18.1).
        assert!(
            scanned > 100,
            "only {scanned} files scanned — the walk is broken, not the code"
        );
        assert!(
            offenders.is_empty(),
            "`{needle}` is back-of-house (quality/ARCH_LAYERS.toml `backstage`) and only \
             `{ALLOWED}` may name it. These product modules do: {offenders:?}.\n\
             If you need an authoring-harness verdict, depend on \
             `sovereign-authoring-harness` DIRECTLY — the `::authoring_harness` path on the \
             eval crate is only a compatibility alias, and routing a product verb through \
             it is how this crate acquired the dependency in the first place."
        );
    }
}
