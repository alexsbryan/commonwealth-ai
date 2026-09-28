// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus serve` — the THIRD of `EPISTEMIC_INDEX.md` §4's three commands, and
//! what this binary did under no verb at all before the other two existed.
//!
//! ## Pull-if-absent is ingest's
//!
//! A named corpus that is not installed is installed by ingest's own CLI,
//! `svrn-ingest pull`, which holds the engine that restores a prebuilt
//! snapshot (sovereign-pipeline, bin/svrn-ingest/pull.rs). This host links
//! no engine (phase-b pb-corpus-mcp-reads): it finds which named corpora are
//! absent, execs the pull for those, and serves. A warm root never needs the
//! pull binary; an absent corpus with no pull binary is refused by name
//! (ARCH §18.3), never served as if it were empty.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use corpus_index::index::CorpusIndex;

use crate::tools;

/// Ingest's CLI, located by the one locator of that binary.
const INGEST_BIN: &str = "svrn-ingest";

#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Base URL of an OpenAI-compatible inference frontend, e.g.
    /// `http://localhost:8080/v1`. Optional: with none, the discovery ladder
    /// runs (Ollama, llama-server, this host's OICP daemon) and names every
    /// rung. Capability is detected from whichever wins
    /// (`GET <root>/oicp/v1/capabilities`), never configured.
    #[arg(long)]
    pub base_url: Option<String>,

    /// Corpus id to serve (repeatable). Default: every installed index. A
    /// named corpus that is not installed is PULLED if its recipe declares a
    /// prebuilt snapshot.
    #[arg(long = "corpus")]
    pub corpora: Vec<String>,

    /// Model id sent in `POST /v1/embeddings`. Default: the first id
    /// `GET /v1/models` returns; refused (not defaulted) if that is empty.
    #[arg(long)]
    pub embed_model: Option<String>,

    /// Data root holding `indexes/`. Default: the same derivation every
    /// sovereign binary uses (`SOVEREIGN_DATA_DIR`, else `~/.svrnmesh`).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Default top-K for `corpus_search`.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,

    /// Minutes a pull-if-absent may take before it is refused. Default:
    /// `svrn-ingest pull`'s own deadline, which says why serving carries one
    /// at all and building does not. 0 disables the bound.
    #[arg(long)]
    pub pull_deadline_mins: Option<u64>,
}

pub async fn run(args: ServeArgs) -> Result<()> {
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(sovereign_contracts::rebrand::data_dir);
    eprintln!("corpus-mcp: data root {}", data_dir.display());
    let indexes_dir = data_dir.join("indexes");

    // Which named corpora are absent, and whether the binary that installs
    // them is here, are local facts: settled before the endpoint probe, so a
    // refusal for a missing `svrn-ingest` costs no network round-trip.
    let absent: Vec<&String> = args
        .corpora
        .iter()
        .filter(|id| !CorpusIndex::has_committed_data(&indexes_dir.join(id)))
        .collect();
    let ingest_bin = if absent.is_empty() {
        None
    } else {
        Some(locate_ingest(&absent)?)
    };

    let profile =
        crate::host::discover_and_probe(args.base_url.as_deref(), args.embed_model.clone()).await?;
    // The MCP tools' questions are QUERY-side; the pull's document-side
    // embedder is `svrn-ingest pull`'s. Handing one raw `EmbedFn` to both was
    // how `ask` came to search the atlas seed table from 0.128 mean cosine
    // outside it (measured 2026-09-07, note 500f1229) — a silent space
    // substitution, exit 0 (ARCH §18.3).
    let embed_query = profile.embed_query_fn();

    if let Some(bin) = ingest_bin {
        pull(&bin, &absent, &args, &data_dir)?;
    }

    let server =
        tools::Server::open(indexes_dir, embed_query, args.corpora, args.limit, profile).await?;
    crate::mcp::serve_stdio(server).await
}

/// Ingest's `svrn-ingest`, by the one locator of that binary
/// (`SOVEREIGN_INGEST_BIN`, else beside this binary, else `PATH`), or a
/// refusal that names it: an absent corpus is never served as if it were
/// empty (ARCH §18.3).
fn locate_ingest(absent: &[&String]) -> Result<PathBuf> {
    match sovereign_turn_client::reach::locate_sibling(INGEST_BIN, "SOVEREIGN_INGEST_BIN") {
        Some(bin) => Ok(bin),
        None => {
            tracing::debug!(corpora = ?absent, "corpus-mcp: pull-if-absent refused, no {INGEST_BIN}");
            bail!(
                "corpus(es) {absent:?} are not installed, and installing one needs ingest's \
                 binary `{INGEST_BIN}` (`svrn-ingest pull`), which was not found. Build it with \
                 `cargo build -p sovereign-pipeline`, or set SOVEREIGN_INGEST_BIN to its path. \
                 Serving never installs a corpus itself."
            )
        }
    }
}

/// Install the named corpora that are not here through `svrn-ingest pull`.
/// `has_committed_data` on the canonical path is the same predicate the pull
/// (and the restorer) use, so the three cannot disagree about "installed";
/// the pull re-checks each id and reports its own four outcomes.
fn pull(bin: &Path, absent: &[&String], args: &ServeArgs, data_dir: &Path) -> Result<()> {
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("pull").arg("--data-dir").arg(data_dir);
    if let Some(url) = &args.base_url {
        cmd.arg("--base-url").arg(url);
    }
    if let Some(model) = &args.embed_model {
        cmd.arg("--embed-model").arg(model);
    }
    if let Some(mins) = args.pull_deadline_mins {
        cmd.arg("--pull-deadline-mins").arg(mins.to_string());
    }
    for id in absent {
        cmd.arg("--corpus").arg(id);
    }
    tracing::debug!(bin = %bin.display(), corpora = ?absent, "corpus-mcp: pull-if-absent through svrn-ingest pull");
    let status = cmd
        .status()
        .with_context(|| format!("running `{} pull`", bin.display()))?;
    if !status.success() {
        bail!(
            "`{} pull` did not install {absent:?} ({status}); its reason is above",
            bin.display()
        );
    }
    Ok(())
}
