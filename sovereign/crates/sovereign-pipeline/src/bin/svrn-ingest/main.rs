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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
