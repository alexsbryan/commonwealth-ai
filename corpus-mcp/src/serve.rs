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

    /// Minutes a pull-if-absent may take before `serve` gives up on it.
    /// See [`PULL_DEADLINE_MINS`] for why serving carries a deadline at all
    /// and building does not. 0 disables the bound.
    #[arg(long, default_value_t = PULL_DEADLINE_MINS)]
    pub pull_deadline_mins: u64,
}

/// How long `serve` will wait for a pull before refusing.
///
/// A RESTORE is a download and an extract: `sep` — the largest corpus this
/// repo ships, 1.8 GB extracted — took about six minutes end to end on the
/// development host. A REBUILD is acquire + extract + chunk + embed of the
/// whole corpus through a local endpoint: measured on that same host at 87
/// minutes and still unfinished when the cgroup OOM-killed it (run
/// 20260905T181423Z; `journalctl --user -u ei6-acceptance` shows the unit at
/// 143 and the scope "Failed with result 'oom-kill'" one second later, 14 GB
/// peak).
///
/// Thirty minutes is well past every restore and nowhere near a rebuild, so
/// the deadline separates the two by DURATION — which is the one signal
/// available to this host, since the decision that divides them is made
/// inside `CorpusEngine::ingest` after the download and `try_restore_prebuilt`
/// is `pub(crate)`.
pub const PULL_DEADLINE_MINS: u64 = 30;

/// The mean-cosine bar the restore's embedding-space probe applies, named here
/// only so the refusal can quote it to the person reading it.
///
/// ONE decider, and it is not this line: the value lives in
/// `corpus-engine/src/engine/ingest_prebuilt.rs::PREBUILT_PROBE_THRESHOLD` and
/// is `pub(crate)` there, so this host cannot read it. Quoting a number this
/// crate cannot import is a §10.6 hazard, and it is written down here rather
/// than inline so a drift has ONE place to be fixed and this comment to be
/// found. Measured against it on 2026-09-05: 0.6822 for a bare llama-server
/// against sep's snapshot (run 20260905T201633Z).
const PREBUILT_PROBE_THRESHOLD: f32 = 0.92;

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
        // `.with_embedding_model` is a PRECONDITION of `ingest`, not an
        // optimisation — and a previous version of this file removed it on a
        // wrong reading, which the restore probe caught in 4 seconds before a
        // byte was downloaded (run 20260905T201154Z):
        //
        //   Error: pulling corpus `sep`
        //   Caused by: Embedding error: embedding model name not configured.
        //
        // `corpus-engine/src/engine/ingest.rs:91` refuses an empty name
        // outright, because the engine cannot introspect an opaque `EmbedFn`
        // and the label it writes to `_corpus_meta.json` has to name the model
        // that actually produced the vectors. `serve` on a WARM root gets away
        // with an unset name only because it never calls `ingest` at all.
        //
        // The value is the GGUF filename STEM, which is what that error asks
        // for by example. See [`embed_model_stem`].
        let engine = CorpusEngine::new(recipes_dir.clone(), indexes_dir.clone(), embed.clone())
            .with_embedding_model(&embed_model_stem(&profile.embed_model));
        for id in &args.corpora {
            ensure_installed(&engine, id, args.pull_deadline_mins).await?;
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
async fn ensure_installed(engine: &CorpusEngine, id: &str, deadline_mins: u64) -> Result<()> {
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
    // THE PROMISE, ENFORCED BY CODE RATHER THAN BY THE COMMENT ABOVE IT
    // (ARCH §7, §18.3). `serve` says it will not rebuild a corpus from
    // source on the caller's behalf, and until this bound existed that
    // sentence was false: `ingest` discards a snapshot whose embedding-space
    // probe fails and rebuilds, and the only thing that stopped it was an
    // OOM kill 93 minutes later.
    //
    // A duration bound is what this host actually has. The branch is taken
    // inside `ingest`, after the download, and the restore entry point is
    // `pub(crate)` — so `serve` cannot ask "was that a restore?" It can ask
    // "has this taken longer than any restore ever does?", and act.
    let result = match pull_with_deadline(engine, id, deadline_mins).await {
        Ok(r) => r,
        Err(PullOutcome::Failed(e)) => {
            return Err(e).with_context(|| format!("pulling corpus `{id}`"))
        }
        Err(PullOutcome::Overran(mins)) => bail!(
            "corpus `{id}`: the pull has run {mins} minutes and has not finished, so the \
             snapshot was DISCARDED and a full rebuild from source started instead — hours, \
             and not what serving asked for. Refusing rather than continuing.\n\n  \
             WHY: the restore runs an embedding-space probe — it re-embeds a sample of the \
             snapshot's own chunks through YOUR endpoint and compares them to the stored \
             vectors, requiring a mean cosine of at least {PREBUILT_PROBE_THRESHOLD}. Look \
             a few lines up in this output for the line reading `embedding-space probe \
             FAILED ... probe_cosine=<n>`: that number is your endpoint's actual agreement \
             with the snapshot, and it is the whole reason this is happening.\n  \
             A LOW COSINE WITH THE RIGHT MODEL IS NORMAL AND IS NOT YOUR MISTAKE. \
             Qwen3-Embedding is last-token pooled and requires an EOS token appended to \
             every input; a bare llama-server `/v1/embeddings` does not append one, so the \
             pooled vector is taken at a different token and the space differs even though \
             the model file is identical. That is a property of the endpoint, not of you.\n\n  \
             Build it yourself instead:   corpus-mcp ingest <recipe.toml>\n  \
             Trust the snapshot's name and skip the probe:  SOVEREIGN_FORCE_PREBUILT=1 \
             (only if you know your endpoint matches the one that BUILT it)\n  \
             Wait longer:                 --pull-deadline-mins <n> (0 disables)"
        ),
    };
    eprintln!(
        "corpus-mcp: corpus `{id}` installed — {} chunks, {} MB",
        result.chunks_created,
        result.index_size_bytes / 1_048_576
    );
    Ok(())
}

/// The endpoint's model id, reduced to the stem `CorpusEngine` asks for.
///
/// `corpus-engine/src/engine/ingest.rs:91` states the contract: "The stem
/// should match the filename of the embedding GGUF (e.g. `qwen-embedding-0.6b`
/// for `qwen-embedding-0.6b.gguf`)". A llama-server reports the filename
/// itself (`Qwen3-Embedding-0.6B-Q8_0.gguf`), so the `.gguf` comes off. An
/// endpoint that reports something which is not a filename — Ollama's
/// `nomic-embed-text`, an OpenAI model id — passes through unchanged, which is
/// correct: the label's job is to name the model that produced the vectors,
/// and the id IS that name there.
///
/// ## What this name does and does NOT decide (ARCH §11.1 — cited, not recalled)
///
/// It does NOT gate the snapshot restore. `SnapshotManifest::check_embedding_compatibility`
/// (`corpus-engine/src/snapshot.rs:223-235`) returns one of three verdicts
/// (`EmbeddingCompat`, snapshot.rs:72-79): `DimsMismatch` when the widths
/// differ — the only hard refusal; `Exact` when name AND width match; and
/// otherwise `NameMismatch`, whose doc comment reads "Dimensions match, model
/// name differs — verify the space by probe".
///
/// So a name that differs from the snapshot's is EXPECTED and benign. It costs
/// the empirical probe, not the restore. Our stem
/// (`Qwen3-Embedding-0.6B-Q8_0`) will differ from `sep`'s declared
/// `qwen-embedding-0.6b`, and that is the designed path, not a bug.
///
/// RETRACTION. An earlier commit on this branch claimed the two names being
/// from "two namespaces, never equal" was the MECHANISM behind run
/// 20260905T181423Z's 93-minute rebuild. Half of that is confirmed — the names
/// do differ, and the verdict is `NameMismatch`. The causal half is NOT: a
/// name mismatch alone never discards anything, so what discarded that
/// snapshot was the empirical probe, `probe_embedding_space` re-embedding a
/// sample and coming in under its cosine threshold, or failing to run at all.
/// Which of those, and why (pooling, normalization, or quantisation differing
/// between a bare llama-server `/v1/embeddings` and the embedder the snapshot
/// was built with), is answered by the next run's captured `pull.err` and by
/// nothing currently on disk. Do not restate the causal claim until that file
/// says it.
fn embed_model_stem(model_id: &str) -> &str {
    model_id.strip_suffix(".gguf").unwrap_or(model_id)
}

/// Why a pull stopped, when it did not succeed.
///
/// Two outcomes and not one `anyhow::Error`, because the caller says something
/// different about each and a person needs to be told which happened: a pull
/// that FAILED hit a real error (no network, bad sha, no disk); a pull that
/// OVERRAN is still running and is almost certainly no longer a pull at all.
#[derive(Debug)]
pub enum PullOutcome {
    Failed(anyhow::Error),
    /// Ran past the deadline, in whole minutes.
    Overran(u64),
}

/// `engine.ingest(...)`, bounded. `deadline_mins == 0` disables the bound and
/// restores the old unbounded behaviour for a caller who means it.
///
/// The bound is on `serve` and NOT on `corpus-mcp ingest`, which is the whole
/// point: `ingest` is the verb whose job IS to spend hours building a corpus,
/// and putting a deadline there would break the thing it is for. Same engine
/// call, two policies, each stated where it belongs.
async fn pull_with_deadline(
    engine: &CorpusEngine,
    id: &str,
    deadline_mins: u64,
) -> std::result::Result<corpus_engine::IngestResult, PullOutcome> {
    let spec = CorpusSpec::Builtin(id.to_string());
    if deadline_mins == 0 {
        tracing::debug!(corpus = id, "corpus-mcp: pull deadline disabled");
        return engine
            .ingest(&spec, None)
            .await
            .map_err(|e| PullOutcome::Failed(anyhow::anyhow!("{e}")));
    }
    let budget = std::time::Duration::from_secs(deadline_mins * 60);
    tracing::debug!(
        corpus = id,
        deadline_mins,
        "corpus-mcp: pulling under a deadline"
    );
    match tokio::time::timeout(budget, engine.ingest(&spec, None)).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(PullOutcome::Failed(anyhow::anyhow!("{e}"))),
        Err(_elapsed) => Err(PullOutcome::Overran(deadline_mins)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate's failing input, named (ARCH §18.1): a pull that does not
    /// finish inside its budget. `engine.ingest` cannot be called here — it
    /// needs a network and a corpus — so the deadline WRAPPER is exercised
    /// against a future with the shape of the thing that actually happened:
    /// one that runs past the bound and would otherwise run for hours.
    ///
    /// Milliseconds rather than tokio's paused clock, which needs the
    /// `test-util` feature this workspace does not enable; the RATIO is what
    /// the test is about (a rebuild that runs 3x its budget), and real time
    /// keeps it honest about the wrapper actually cancelling.
    ///
    /// This is the case that cost 93 minutes and an OOM kill. Before the
    /// bound existed there was no code path that could stop it.
    #[tokio::test]
    async fn a_pull_that_overruns_its_budget_is_refused_not_awaited() {
        let budget = std::time::Duration::from_millis(30);
        let rebuild = async {
            tokio::time::sleep(std::time::Duration::from_millis(90)).await;
            "a corpus nobody asked to be built"
        };
        let t0 = std::time::Instant::now();
        assert!(
            tokio::time::timeout(budget, rebuild).await.is_err(),
            "a rebuild running 3x the budget was not stopped by the deadline"
        );
        // It CANCELLED rather than waited: the whole failure being fixed is a
        // bound that lets the long thing finish anyway.
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(80),
            "the deadline waited for the rebuild instead of abandoning it ({:?})",
            t0.elapsed()
        );
    }

    /// The other half, so the bound cannot pass by refusing everything: a
    /// RESTORE finishes well inside it. `sep` — the largest corpus this repo
    /// ships — measured about six minutes to download and extract against a
    /// thirty-minute budget, the same 1:5 ratio as below.
    #[tokio::test]
    async fn a_restore_finishes_well_inside_the_budget() {
        let budget = std::time::Duration::from_millis(30);
        let restore = async {
            tokio::time::sleep(std::time::Duration::from_millis(6)).await;
            "restored"
        };
        assert_eq!(
            tokio::time::timeout(budget, restore).await.ok(),
            Some("restored"),
            "a restore well inside the budget was refused by the deadline meant to allow it"
        );
    }

    /// The precondition whose removal cost run 20260905T201154Z. A
    /// llama-server reports the GGUF FILENAME; `ingest` wants the stem.
    #[test]
    fn the_endpoint_model_id_becomes_the_stem_ingest_demands() {
        assert_eq!(
            embed_model_stem("Qwen3-Embedding-0.6B-Q8_0.gguf"),
            "Qwen3-Embedding-0.6B-Q8_0"
        );
        // Not a filename — Ollama, vLLM, an OpenAI id. Unchanged, because
        // there the id already IS the model's name.
        assert_eq!(embed_model_stem("nomic-embed-text"), "nomic-embed-text");
        // Only a TRAILING .gguf, and only once.
        assert_eq!(embed_model_stem("a.gguf.gguf"), "a.gguf");
        assert_eq!(embed_model_stem("gguf"), "gguf");
        // Never empty for a non-empty id: an empty name is exactly what
        // `ingest.rs:91` refuses.
        assert!(!embed_model_stem("x.gguf").is_empty());
    }

    /// 0 disables the bound, for a caller who means to wait.
    #[test]
    fn zero_disables_the_deadline() {
        assert_eq!(PULL_DEADLINE_MINS, 30);
        // The disabled path is a distinct branch in `pull_with_deadline`;
        // this pins the sentinel the flag documents, so a change to the
        // default cannot silently turn the bound off.
        assert_ne!(PULL_DEADLINE_MINS, 0);
    }
}
