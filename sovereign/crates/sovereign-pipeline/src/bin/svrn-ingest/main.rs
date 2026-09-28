// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn-ingest` — ingest's own CLI (FIVE_PROGRAMS §2: the recipe pipeline,
//! CLI only). `svrn ingest <recipe.toml>` execs this binary with the verb as
//! its first argument (sovereign-cli `ingest_bin.rs`), so a developer who
//! wants ingest in CI takes this crate and no svrn crate.

mod ingest;

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
    }
}
