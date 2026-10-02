// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn-ingest` — ingest's own CLI (FIVE_PROGRAMS §2: the recipe pipeline,
//! CLI only). `svrn ingest <recipe.toml>` execs this binary with the verb as
//! its first argument (sovereign-cli `ingest_bin.rs`), so a developer who
//! wants ingest in CI takes this crate and no svrn crate.

mod finalize;
mod index;
mod ingest;
mod pull;
mod watch;

use clap::{Parser, Subcommand};
use sovereign_cli_base::tracing_init::init_tracing;

#[derive(Parser, Debug)]
#[command(name = "svrn-ingest", version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Build a corpus from a recipe against a bare endpoint: acquire,
    /// extract, chunk, embed, index, then the atlas enrichment.
    Ingest(ingest::IngestArgs),
    /// Install each named corpus that is not here from its recipe's prebuilt
    /// snapshot, under a deadline; `corpus-mcp serve --corpus` execs this.
    Pull(pull::PullArgs),
    /// Build one index from a recipe file into a named directory, or
    /// re-index a list of files in one; `code index` execs this.
    Index(index::IndexArgs),
    /// Promote a stranded `<corpus>-partition-local/` index into
    /// `<corpus>/`; `code finalize` execs this.
    Finalize(finalize::FinalizeArgs),
    /// Keep one code index current while its source tree is edited, until
    /// Ctrl-C; `code watch` execs this.
    Watch(watch::WatchArgs),
}

fn main() -> anyhow::Result<()> {
    // ingest's CLI verbs, moved from sovereign-cli-llm (pb-cli-llm-ingest-move),
    // keep that binary's hand-rolled argv, process setup and per-verb tracing.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if let Some(verb) = raw.first().map(String::as_str) {
        if sovereign_pipeline::CLI_VERBS.contains(&verb) {
            std::process::exit(run_cli_verb(verb, &raw[1..]));
        }
    }
    run_subcommand()
}

/// The process sovereign-cli-llm's `bin_main` gave these verbs: full
/// backtraces, 8 MiB stacks, the rebrand migration, and the verb's tracing
/// filter (target names follow the module path, now `sovereign_pipeline`).
fn run_cli_verb(verb: &str, rest: &[String]) -> i32 {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }
    sovereign_contracts::rebrand::promote_legacy_env();
    sovereign_contracts::rebrand::run_startup_migration();
    match verb {
        "pipeline" => init_tracing("sovereign_pipeline=info"),
        "enrich" => init_tracing("sovereign_pipeline=info,corpus_engine=info"),
        "bench" if std::env::var_os("RUST_LOG").is_some() => {
            init_tracing("sovereign_pipeline=info")
        }
        _ => {}
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("svrn-ingest-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime
        .block_on(sovereign_pipeline::run_cli_verb(verb, rest))
        .expect("CLI_VERBS names every verb run_cli_verb answers")
}

#[tokio::main]
async fn run_subcommand() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    match Args::parse().command {
        Command::Ingest(a) => ingest::run(a).await,
        Command::Pull(a) => pull::run(a).await,
        Command::Index(a) => index::run(a).await,
        Command::Finalize(a) => finalize::run(a).await,
        Command::Watch(a) => watch::run(a).await,
    }
}
