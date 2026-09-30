// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-bench` — bench's own CLI (FIVE_PROGRAMS §11 "The cli-llm
//! split", phase-b pb-cli-llm-bench-move). The `sovereign` dispatcher execs
//! this sibling for `svrn bench`, `svrn eval` and `svrn quality lane`.
//!
//! bench measures the other programs by dialling them, never by embedding
//! them: a turn goes to svrn's turn route (`bench_cmd::subject`), svrn's own
//! internals and grounding primitives come back from `svrn __probe`
//! (`eval_cmd::probe_score`, `bench_cmd::svrn_judge`), and what bench runs of
//! ingest it execs through the dispatcher's `svrn enrich|corpus …`. So the
//! crate names no svrn or ingest crate; the leaves it names are bench's
//! `leaf_budget` (quality/ARCH_LAYERS.toml).
//!
//! svrn's white-box lanes stayed in sovereign-cli-llm, and the dispatcher
//! routes their spellings there: `bench judge-replay`, `bench
//! resolver-precision`, `bench atlas` and `eval inner-chaos`, and the
//! `search-gym` and `knowledge-gym` verbs.

mod bench_cmd;
mod eval_cmd;
mod quality_lane_cmd;

use sovereign_cli_base::tracing_init::init_tracing;

/// The sibling binary's entry point; `src/main.rs` is a shim over it.
pub fn bin_main() {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }
    // Rebrand back-compat, as every sibling runs it: idempotent, non-destructive.
    sovereign_contracts::rebrand::promote_legacy_env();
    sovereign_contracts::rebrand::run_startup_migration();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-cli-bench-rt")
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

    // The bench verbs print their own summaries on stderr and stay quiet by
    // default, so harnesses parsing that stderr are not disturbed; with
    // RUST_LOG set they get the tracing glassbox, as they did in
    // sovereign-cli-llm.
    match cmd {
        "bench" | "eval" if std::env::var_os("RUST_LOG").is_some() => {
            init_tracing("sovereign_cli_bench=info")
        }
        _ => {}
    }

    let code: i32 = match cmd {
        "bench" => bench_cmd::run_bench(rest).await,
        "eval" => eval_cmd::run_eval(rest).await,
        // One lane of `svrn quality check`: the runner lives in
        // `sovereign-cli` and touches no model; the lanes live here.
        "quality-lane" => quality_lane_cmd::run(rest).await,
        "" => {
            eprintln!("sovereign-cli-bench: usage: sovereign-cli-bench <bench|eval|quality-lane> [args...]");
            2
        }
        other => {
            eprintln!("sovereign-cli-bench: unknown subcommand '{other}'");
            2
        }
    };

    std::process::exit(code);
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod group_tests;
