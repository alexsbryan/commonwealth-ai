// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus-mcp` — a corpus-engine MCP host that needs nothing but an
//! OpenAI-compatible endpoint.
//!
//! `EPISTEMIC_INDEX.md` §4's whole experience, for a person who is barely
//! technical, is one recipe and three commands:
//!
//! ```sh
//! corpus recipe new --ontology numismatics --id my-coins   # writes my-coins.toml
//! corpus ingest my-coins.toml                              # acquire → … → enrich
//! corpus serve --corpus my-coins                           # the MCP host
//! ```
//!
//! Note what is NOT on those lines: a URL. When no endpoint is named,
//! [`host::discover`] walks a fixed ladder — Ollama's `:11434`, llama-server's
//! `:8080`, then this host's own OICP daemon — and prints what each rung said
//! (order ei-6-distribution). `--base-url` short-circuits it and is never
//! substituted when it fails.
//!
//! With no subcommand the binary SERVES, which is what every invocation meant
//! before `ingest` existed (order ei-5b-build-verb) and what `serve`'s own
//! flags still mean — [`serve::ServeArgs`] is ONE struct, flattened at the top
//! level and carried by the `serve` verb, so the two spellings cannot drift.
//!
//! Speaks MCP over stdio (newline-delimited JSON-RPC 2.0: `initialize`,
//! `tools/list`, `tools/call`). Five tools: `ask` (the default — cited
//! passages plus the map of ideas the atlas walk traversed to find them),
//! `corpus_list`, `corpus_search` (cited chunks), `atoms_lookup` (declared
//! atlas atoms) and `corpus_ontology` (what the corpus declared). No sovereign
//! daemon, no local model, no mesh — the dep tree carries no llama.cpp, ort or
//! iroh, and `tests/no_inference_stack.rs` fails if it ever does.
//!
//! What it does NOT do, stated rather than implied: the atom-grounded RANKING
//! (`atom_enum`, `atlas_grounding`) lives in `sovereign-core` and is not here.
//! Tier 1 (cited chunk search) and tier 1.5 (read what enrichment produced)
//! cross the seam; the ranking is the separate RAG extraction.

mod ask;
mod host;
mod ingest;
mod mcp;
mod recipe;
mod serve;
mod tools;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "corpus-mcp", version, about)]
struct Args {
    /// What to do. Absent = serve, which is what every invocation of this
    /// binary meant before the verbs existed and still means. Adding
    /// subcommands rather than mode flags keeps `--help` honest about which
    /// flags belong to which verb.
    #[command(subcommand)]
    command: Option<Command>,

    /// The bare form's serve flags. The SAME struct `serve` takes, flattened
    /// here — one declaration, so `corpus-mcp --base-url …` and `corpus-mcp
    /// serve --base-url …` cannot come to mean different things (ARCH §10.6).
    #[command(flatten)]
    serve: serve::ServeArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Scaffold a recipe from a built-in ontology template.
    #[command(subcommand)]
    Recipe(recipe::RecipeCommand),

    /// Build a corpus from a recipe against a bare endpoint: acquire,
    /// extract, chunk, embed, index, then the atlas enrichment.
    Ingest(ingest::IngestArgs),

    /// Serve installed corpora over MCP on stdio, pulling a named corpus's
    /// prebuilt snapshot first if it is not installed here.
    Serve(serve::ServeArgs),
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
    match args.command {
        Some(Command::Recipe(cmd)) => recipe::run(cmd),
        Some(Command::Ingest(a)) => ingest::run(a).await,
        Some(Command::Serve(a)) => serve::run(a).await,
        None => serve::run(args.serve).await,
    }
}
