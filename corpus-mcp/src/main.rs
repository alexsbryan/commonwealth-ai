// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus-mcp` — a corpus-engine MCP host that needs nothing but an
//! OpenAI-compatible endpoint.
//!
//! ```sh
//! llama-server -m Qwen3-Embedding-0.6B-Q8_0.gguf --embeddings --port 8080
//! corpus-mcp --base-url http://localhost:8080/v1 --corpus sep
//! ```
//!
//! Two verbs. With no subcommand it SERVES, which is what it has always done
//! and what every existing invocation means. `corpus-mcp ingest <recipe.toml>`
//! builds a corpus from a recipe against the same kind of endpoint — the
//! second of `EPISTEMIC_INDEX.md` §4's three commands (order ei-5b-build-verb).
//!
//! ```sh
//! corpus-mcp ingest my-coins.toml --chat-url http://localhost:8090/v1 \
//!                                 --embed-url http://localhost:8089/v1
//! ```
//!
//! Speaks MCP over stdio (newline-delimited JSON-RPC 2.0: `initialize`,
//! `tools/list`, `tools/call`). Five tools: `ask` (the default — cited
//! passages plus the map of ideas the atlas walk traversed to find them),
//! `corpus_list`, `corpus_search` (cited chunks), `atoms_lookup` (declared
//! atlas atoms) and `corpus_ontology` (what the corpus declared). No sovereign daemon, no local
//! model, no mesh — the dep tree carries no llama.cpp, ort or iroh, and
//! `tests/no_inference_stack.rs` fails if it ever does.
//!
//! What it does NOT do, stated rather than implied: the atom-grounded RANKING
//! (`atom_enum`, `atlas_grounding`) lives in `sovereign-core` and is not here.
//! Tier 1 (cited chunk search) and tier 1.5 (read what enrichment produced)
//! cross the seam; the ranking is the separate RAG extraction.

mod ask;
mod host;
mod ingest;
mod mcp;
mod tools;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "corpus-mcp", version, about)]
struct Args {
    /// What to do. Absent = serve, which is what every invocation of this
    /// binary meant before `ingest` existed and still means. Adding a
    /// subcommand rather than a mode flag keeps `--help` honest about which
    /// flags belong to which verb.
    #[command(subcommand)]
    command: Option<Command>,

    /// Base URL of an OpenAI-compatible inference frontend, e.g.
    /// `http://localhost:8080/v1`. Required to SERVE. Capability is
    /// detected from it (`GET <root>/oicp/v1/capabilities`), never configured.
    #[arg(long)]
    base_url: Option<String>,

    /// Corpus id to serve (repeatable). Default: every installed index.
    #[arg(long = "corpus")]
    corpora: Vec<String>,

    /// Model id sent in `POST /v1/embeddings`. Default: the first id
    /// `GET /v1/models` returns; refused (not defaulted) if that is empty.
    #[arg(long)]
    embed_model: Option<String>,

    /// Data root holding `indexes/`. Default: the same derivation every
    /// sovereign binary uses (`SOVEREIGN_DATA_DIR`, else `~/.svrnmesh`).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Default top-K for `corpus_search`.
    #[arg(long, default_value_t = 10)]
    limit: usize,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Build a corpus from a recipe against a bare endpoint: acquire,
    /// extract, chunk, embed, index, then the atlas enrichment.
    Ingest(ingest::IngestArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Everything diagnostic goes to stderr: stdout is the MCP channel.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    if let Some(Command::Ingest(ingest_args)) = args.command {
        return ingest::run(ingest_args).await;
    }

    // Serve. `--base-url` is what a serve needs and an ingest does not, so it
    // is checked here rather than declared mandatory — a required flag would
    // make `corpus-mcp ingest …` demand a URL it was given two better ones for.
    let Some(base_url) = args.base_url else {
        anyhow::bail!(
            "serving needs --base-url <url> (an OpenAI-compatible endpoint, e.g. \
             http://localhost:8080/v1). For `corpus-mcp ingest`, see --help."
        );
    };
    let profile = host::probe(&base_url, args.embed_model).await?;
    let data_dir = args
        .data_dir
        .unwrap_or_else(sovereign_contracts::rebrand::data_dir);
    eprintln!("corpus-mcp: data root {}", data_dir.display());

    let embed = corpus_engine::embed_http::http_embed_fn(
        profile.embeddings_url.clone(),
        profile.embed_model.clone(),
    );
    let server = tools::Server::open(
        data_dir.join("recipes"),
        data_dir.join("indexes"),
        embed,
        args.corpora,
        args.limit,
        profile,
    )
    .await?;
    mcp::serve_stdio(server).await
}
