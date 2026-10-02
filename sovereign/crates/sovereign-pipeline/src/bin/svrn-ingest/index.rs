// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn-ingest index` — build one index from a recipe file into a named
//! directory, or re-index a list of files in one (phase-b pb-code-index).
//!
//! Code's `code index` execs this: code writes the recipe and decides which
//! files changed; ingest owns the engine that extracts, chunks, embeds and
//! writes. Unlike `ingest`, the recipe is read where it lies and never
//! registered, nothing is enriched, and the index lands at
//! `<index-dir>/<corpus-id>/`.
//!
//! The embedder comes from the one decider, `corpus_index::host`: the
//! `--base-url` endpoint if named, else the discovery ladder. `--fts-only`
//! probes nothing and builds a keyword-only index, stamped
//! [`FTS_ONLY_EMBEDDING_MODEL`] so no reader takes its zero vectors for a
//! model's space. The flag must agree with the index: a full build refuses a
//! recipe whose `[index] vector` says otherwise, and a file-list run refuses
//! an index stamped otherwise, so one index never mixes the two.
//!
//! The last line of stdout is the result as one JSON object, for the caller
//! to render; everything a person reads goes to stderr.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use corpus_engine::engine::reindex::ReindexResult;
use corpus_engine::{CorpusEngine, CorpusSpec, Recipe};
use corpus_index::corpus::Corpus;
use corpus_index::host;
use corpus_index::types::{EmbedFn, DEFAULT_EMBED_DIM, FTS_ONLY_EMBEDDING_MODEL};

#[derive(Debug, clap::Args)]
pub struct IndexArgs {
    /// Directory the index is written under, as `<dir>/<corpus-id>/`.
    #[arg(long)]
    pub index_dir: PathBuf,

    /// Build the whole index from this recipe. Read here; never registered.
    #[arg(
        long,
        required_unless_present = "files_from",
        conflicts_with = "files_from"
    )]
    pub recipe: Option<PathBuf>,

    /// Re-index only the files listed in this file, one path per line
    /// relative to `--root`, in the existing index `--corpus`. A listed file
    /// that no longer exists has its chunks deleted.
    #[arg(long, requires_all = ["corpus", "root"])]
    pub files_from: Option<PathBuf>,

    /// The existing index a `--files-from` run updates.
    #[arg(long)]
    pub corpus: Option<String>,

    /// The source root the listed files are relative to.
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// One OpenAI-compatible endpoint serving embeddings. Default: the
    /// discovery ladder `ingest` and `corpus-mcp serve` walk.
    #[arg(long)]
    pub base_url: Option<String>,

    /// Embedding model id. Default: what the endpoint lists that embeds.
    #[arg(long)]
    pub embed_model: Option<String>,

    /// Build a keyword-only (FTS) index: no endpoint is probed and nothing is
    /// embedded.
    #[arg(long, conflicts_with_all = ["base_url", "embed_model"])]
    pub fts_only: bool,
}

pub async fn run(args: IndexArgs) -> Result<()> {
    match &args.recipe {
        Some(recipe) => build(&args, recipe).await,
        None => reindex(&args).await,
    }
}

/// The embedder and the model name the index is stamped with.
async fn embedder(args: &IndexArgs) -> Result<(EmbedFn, String)> {
    if args.fts_only {
        eprintln!(
            "svrn-ingest: --fts-only: no endpoint probed; the index is keyword-only (FTS) and \
             holds no vectors"
        );
        // Zeros only because the chunk schema has a vector column; the stamp
        // below is what tells a reader they are not a space.
        let zero: EmbedFn = Arc::new(|_text: &str| {
            Box::pin(async { Ok::<Vec<f32>, corpus_index::Error>(vec![0.0; DEFAULT_EMBED_DIM]) })
        });
        return Ok((zero, FTS_ONLY_EMBEDDING_MODEL.to_string()));
    }
    let profile = host::discover_and_probe(args.base_url.as_deref(), args.embed_model.clone())
        .await
        .context("no embedder; pass --fts-only to build a keyword-only index")?;
    Ok((profile.embed_document_fn(), profile.embed_model))
}

async fn build(args: &IndexArgs, recipe_path: &Path) -> Result<()> {
    let recipe = Recipe::from_file(recipe_path)
        .with_context(|| format!("loading {}", recipe_path.display()))?;
    if recipe.index.vector == args.fts_only {
        bail!(
            "{} says `[index] vector = {}` but --fts-only is {}: a keyword-only build writes \
             zero vectors, and a vector index must not hold them. Make the two agree.",
            recipe_path.display(),
            recipe.index.vector,
            if args.fts_only { "set" } else { "not set" },
        );
    }
    let (embed, model) = embedder(args).await?;
    std::fs::create_dir_all(&args.index_dir)
        .with_context(|| format!("creating {}", args.index_dir.display()))?;
    let recipes_dir = recipe_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let engine =
        CorpusEngine::new(recipes_dir, args.index_dir.clone(), embed).with_embedding_model(&model);
    tracing::debug!(
        recipe = %recipe_path.display(),
        index_dir = %args.index_dir.display(),
        model = %model,
        "svrn-ingest index: full build"
    );
    let result = engine
        .ingest(
            &CorpusSpec::RecipePath(recipe_path.to_path_buf()),
            Some(Box::new(crate::ingest::render_ingest_progress)),
        )
        .await
        .with_context(|| format!("ingesting {}", recipe_path.display()))?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

async fn reindex(args: &IndexArgs) -> Result<()> {
    // clap's `requires_all` guarantees all three with `--files-from`.
    let (Some(list), Some(corpus_id), Some(root)) = (&args.files_from, &args.corpus, &args.root)
    else {
        bail!("--files-from needs --corpus and --root");
    };
    let files: Vec<String> = std::fs::read_to_string(list)
        .with_context(|| format!("reading {}", list.display()))?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    let meta_path = Corpus::meta_in(args.index_dir.join(corpus_id));
    let stamped = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["embedding_model"].as_str().map(str::to_string))
        .with_context(|| format!("no index to update: cannot read {}", meta_path.display()))?;
    let keyword_only = stamped == FTS_ONLY_EMBEDDING_MODEL;
    if keyword_only != args.fts_only {
        bail!(
            "index `{corpus_id}` is stamped `{stamped}`, and --fts-only is {}: re-indexing \
             would mix zero vectors with a model's in one index. Rebuild it whole instead.",
            if args.fts_only { "set" } else { "not set" },
        );
    }

    let (embed, model) = embedder(args).await?;
    let engine = CorpusEngine::new(args.index_dir.clone(), args.index_dir.clone(), embed)
        .with_embedding_model(&model);
    tracing::debug!(
        corpus = %corpus_id,
        files = files.len(),
        model = %model,
        "svrn-ingest index: file-list run"
    );

    let (mut updated, mut unchanged, mut deleted, mut skipped, mut failed) = (0, 0, 0, 0, 0);
    let mut chunks_written = 0usize;
    for (n, rel) in files.iter().enumerate() {
        match engine.reindex_file(corpus_id, &root.join(rel), root).await {
            Ok(ReindexResult::Updated {
                chunks_written: w, ..
            }) => {
                // `reindex_file` reports 0 written when every chunk hash-matched
                // a committed row — the whole point of the delta path. Counting
                // that as "updated" would overstate the work done.
                if w == 0 {
                    unchanged += 1;
                } else {
                    updated += 1;
                    chunks_written += w;
                }
            }
            Ok(ReindexResult::Deleted { .. }) => deleted += 1,
            Ok(ReindexResult::Skipped) => skipped += 1,
            Err(e) => {
                failed += 1;
                eprintln!("  ! {rel}: {e}");
            }
        }
        if files.len() > 20 && (n + 1) % 20 == 0 {
            eprintln!("  … {}/{} files", n + 1, files.len());
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "updated": updated,
            "unchanged": unchanged,
            "deleted": deleted,
            "skipped": skipped,
            "failed": failed,
            "chunks_written": chunks_written,
        })
    );
    if failed > 0 {
        bail!("{failed} file(s) failed to re-index");
    }
    Ok(())
}
