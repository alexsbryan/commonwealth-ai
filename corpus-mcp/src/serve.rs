// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus serve` — the THIRD of `EPISTEMIC_INDEX.md` §4's three commands, and
//! what this binary did under no verb at all before the other two existed.
//!
//! ## Pull-if-absent (order ei-6-distribution, §1 Distribution row)
//!
//! `corpus serve --corpus sep` on a machine that has never held `sep` used to
//! fail with "no searchable corpus". It now installs it first — and installs
//! it through the path that already exists rather than a second downloader:
//! `CorpusEngine::ingest(CorpusSpec::Builtin(id))` resolves the recipe from
//! the registry and, when that recipe declares `[prebuilt]`, hands off to
//! `corpus-engine/src/engine/ingest_prebuilt.rs` — the resume-aware
//! `BulkDownloader`, the sha256 check, the embedding-width floor, and the
//! extraction that lands `indexes/<id>/` AND `enrichment/<id>/` in one go.
//! Nothing about downloading is decided here (ARCH §19: the inventory
//! outranks the plan), which is also why this adds no HTTP client and no row
//! to the F26 egress census — the pull happens at
//! `corpus-engine/src/acquirers/bulk_download.rs`, registered `InboundOnly 1`
//! since the census was written.
//!
//! The guard that makes it safe: a recipe with NO `[prebuilt]` block is not
//! pulled. `ingest` on such a recipe would acquire and embed the whole corpus
//! from source — minutes to hours, on an endpoint chosen for serving — which
//! is `corpus ingest`'s job and is never what `serve` should silently start.
//! That case is REPORTED by name with the command that does it (ARCH §18.3).
//!
//! Pull-if-absent applies ONLY to ids the caller named. With no `--corpus` the
//! host serves whatever is installed, and "whatever is installed" cannot be
//! missing.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use corpus_engine::{CorpusEngine, CorpusIndex, CorpusSpec};

use crate::tools;

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
}

pub async fn run(args: ServeArgs) -> Result<()> {
    let profile =
        crate::host::discover_and_probe(args.base_url.as_deref(), args.embed_model).await?;
    let data_dir = args
        .data_dir
        .unwrap_or_else(sovereign_contracts::rebrand::data_dir);
    eprintln!("corpus-mcp: data root {}", data_dir.display());

    let embed = corpus_engine::embed_http::http_embed_fn(
        profile.embeddings_url.clone(),
        profile.embed_model.clone(),
    );
    let recipes_dir = data_dir.join("recipes");
    let indexes_dir = data_dir.join("indexes");

    if !args.corpora.is_empty() {
        // NO `.with_embedding_model(&profile.embed_model)` here, and the
        // comment that used to justify one was wrong in a way that cost 93
        // minutes of somebody's afternoon (run 20260905T181423Z).
        //
        // It claimed the restorer needs a declared model to compare against
        // the snapshot's `compatible_embedding_model`. It does not work that
        // way: `snapshot::check_embedding_compatibility` treats DIMENSIONS as
        // the hard floor and a name difference as merely `NameMismatch`, which
        // is then settled EMPIRICALLY by re-embedding a sample of the
        // snapshot's own chunks (`ingest_prebuilt::probe_embedding_space`,
        // mean cosine >= 0.92). Names are unreliable on purpose — they drift
        // across dir, stem, repo and quant for one model.
        //
        // What the line actually did was feed the comparison a value from the
        // WRONG NAMESPACE: `profile.embed_model` is whatever id the endpoint
        // reports, which for llama-server is the GGUF FILENAME
        // (`Qwen3-Embedding-0.6B-Q8_0.gguf`), while the snapshot declares the
        // canonical `qwen-embedding-0.6b`. Those can never match. Passing
        // nothing leaves the same NameMismatch verdict and the same probe, so
        // dropping it loses no safety — it just stops the code claiming a
        // comparison it was not making.
        let engine = CorpusEngine::new(recipes_dir.clone(), indexes_dir.clone(), embed.clone());
        for id in &args.corpora {
            ensure_installed(&engine, id).await?;
        }
    }

    let server = tools::Server::open(
        recipes_dir,
        indexes_dir,
        embed,
        args.corpora,
        args.limit,
        profile,
    )
    .await?;
    crate::mcp::serve_stdio(server).await
}

/// Install `id` if it is not here, or say precisely why it cannot be.
///
/// Four outcomes, all named (ARCH §18.2): already installed, pulled, no
/// registry entry, or an entry with no prebuilt snapshot. None of them is a
/// silent success and none is a silent skip.
async fn ensure_installed(engine: &CorpusEngine, id: &str) -> Result<()> {
    // `has_committed_data` on the canonical path, and NOT
    // `installed_indexes()`: that call opens every `chunks.lance` under the
    // data root (~10 s on a populated install, note
    // `daemon_installed_indexes_reopen`), which would be a tax on every serve
    // to answer a question a directory read settles. It is also the SAME
    // predicate `try_restore_prebuilt` uses to refuse overwriting an installed
    // corpus, so this check and the restorer's cannot disagree (ARCH §10.6).
    let canonical = engine.canonical_path(id);
    if CorpusIndex::has_committed_data(&canonical) {
        tracing::debug!(
            corpus = id,
            path = %canonical.display(),
            "corpus-mcp: already installed, no pull"
        );
        return Ok(());
    }

    // `load_recipe` is the engine's ONE recipe resolver (local overrides, then
    // the registry) — the same door `ingest` walks through a moment later.
    let recipe = engine.load_recipe(id).await.map_err(|e| {
        anyhow::anyhow!(
            "corpus `{id}` is not installed and the recipe registry has no entry for it ({e}). \
             Scaffold one with `corpus-mcp recipe new --ontology <template> --id {id}` and build it \
             with `corpus-mcp ingest {id}.toml`."
        )
    })?;
    let Some(prebuilt) = recipe.prebuilt.as_ref() else {
        bail!(
            "corpus `{id}` is not installed, and its recipe declares no prebuilt snapshot — \
             there is nothing to pull. Build it: `corpus-mcp ingest <recipe.toml>`. (Serving will \
             not start an acquire-and-embed of the whole corpus on your behalf.)"
        );
    };
    eprintln!(
        "corpus-mcp: corpus `{id}` is not installed — pulling the prebuilt snapshot from \
         huggingface.co/datasets/{} ({})",
        prebuilt.hf_repo, prebuilt.hf_filename
    );
    // The degradation this verb CANNOT prevent, named before it can happen
    // rather than discovered as a process that will not finish (ARCH §18.3).
    //
    // `CorpusEngine::ingest` restores the snapshot only if the embedding-space
    // probe accepts it; on a failed or unrunnable probe it deletes what it
    // extracted and falls through to a FULL acquire-extract-chunk-embed of the
    // corpus from source. For `corpus ingest` that fall-through is correct —
    // building is the point. For `serve` it is not what anyone asked for, and
    // on `sep` (1,770 articles, ~182k paragraphs) it is hours through a local
    // embedding endpoint.
    //
    // This host cannot intercept that decision — it is made inside `ingest`,
    // after the download, and the restore entry point is `pub(crate)`. So the
    // honest thing available here is to say so first, with the two ways out.
    eprintln!(
        "corpus-mcp: if `{}`'s embedding space does not match this endpoint, the restore is \
         DISCARDED and `ingest` rebuilds `{id}` from source instead — hours, not minutes. \
         Ctrl-C and run `corpus-mcp ingest` deliberately if that is what you want; set \
         SOVEREIGN_FORCE_PREBUILT=1 to accept the snapshot on its declared name and skip \
         the probe.",
        prebuilt.compatible_embedding_model
    );
    tracing::debug!(
        corpus = id,
        hf_repo = %prebuilt.hf_repo,
        hf_filename = %prebuilt.hf_filename,
        "corpus-mcp: pull-if-absent"
    );
    let result = engine
        .ingest(&CorpusSpec::Builtin(id.to_string()), None)
        .await
        .with_context(|| format!("pulling corpus `{id}`"))?;
    eprintln!(
        "corpus-mcp: corpus `{id}` installed — {} chunks, {} MB",
        result.chunks_created,
        result.index_size_bytes / 1_048_576
    );
    Ok(())
}
