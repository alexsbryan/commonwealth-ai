// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn-ingest watch` — keep one code index current while its source tree
//! is edited (phase-b pb-code-clean).
//!
//! Code's `code watch` execs this: code resolves which corpus and which
//! source root; ingest owns the engine whose `CodeWatcher` re-indexes every
//! debounced file event. Runs until Ctrl-C.
//!
//! The watcher WRITES: every event embeds the changed chunks and inserts
//! them, so it takes the real embedder from the one decider,
//! `corpus_index::host`, exactly as `index` does. It never runs with a stub
//! embedder: zero vectors would silently degrade semantic search for
//! precisely the files being edited.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use corpus_engine::update::watch::CodeWatcher;
use corpus_engine::CorpusEngine;
use corpus_index::host;

#[derive(Debug, clap::Args)]
pub struct WatchArgs {
    /// Directory the corpus index lives under, as `<dir>/<corpus-id>/`.
    #[arg(long)]
    pub index_dir: PathBuf,

    /// The existing index the watcher keeps current.
    #[arg(long)]
    pub corpus: String,

    /// The source root whose edits are re-indexed.
    #[arg(long)]
    pub root: PathBuf,

    /// One OpenAI-compatible endpoint serving embeddings. Default: the
    /// discovery ladder `index` walks.
    #[arg(long)]
    pub base_url: Option<String>,

    /// Embedding model id. Default: what the endpoint lists that embeds.
    #[arg(long)]
    pub embed_model: Option<String>,
}

pub async fn run(args: WatchArgs) -> Result<()> {
    let profile = host::discover_and_probe(args.base_url.as_deref(), args.embed_model.clone())
        .await
        .context(
            "`watch` embeds every changed chunk with the node's embed model so the watcher's \
             writes land in the same embedding space as the rest of the corpus. Start an \
             embeddings endpoint (e.g. `svrn daemon run`) and re-run — the watcher will not run \
             with a stub embedder, because that would silently degrade the index it is meant to \
             keep current",
        )?;
    let embed_model_name = profile.embed_model.clone();
    // No recipe is read: the recipes dir is the index dir.
    let engine = Arc::new(
        CorpusEngine::new(
            args.index_dir.clone(),
            args.index_dir.clone(),
            profile.embed_document_fn(),
        )
        .with_embedding_model(&embed_model_name),
    );

    eprintln!(
        "Watching {} for corpus '{}'",
        args.root.display(),
        args.corpus
    );
    eprintln!("Embedding with {embed_model_name}.");
    eprintln!("Press Ctrl-C to stop.");

    let watcher = CodeWatcher::new(Arc::clone(&engine), args.corpus.clone(), args.root.clone());
    let handle = match watcher.start().await {
        Ok(h) => h,
        Err(e) => bail!("failed to start watcher: {e}"),
    };

    // Keep the process alive until Ctrl-C. The watcher handle aborts its
    // background task on drop.
    tokio::signal::ctrl_c()
        .await
        .context("failed to install ctrl-c handler")?;
    eprintln!("\nShutting down watcher...");
    handle.abort();
    Ok(())
}
