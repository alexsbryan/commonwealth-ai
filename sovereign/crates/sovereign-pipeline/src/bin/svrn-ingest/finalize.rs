// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn-ingest finalize` — promote a stranded `<corpus>-partition-local/`
//! Lance index into the canonical `<corpus>/` (phase-b pb-code-clean).
//!
//! Code's `code finalize` execs this: the promotion is the engine's
//! (`CorpusEngine::finalise_solo_ingest`), and ingest owns the engine.
//! It only inspects and renames on the filesystem, so the engine gets a
//! no-op embedder and no endpoint is probed. Safe to rerun.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use corpus_engine::CorpusEngine;
use corpus_index::types::EmbedFn;

#[derive(Debug, clap::Args)]
pub struct FinalizeArgs {
    /// The corpus whose partition-local index is promoted.
    pub corpus_id: String,

    /// Directory the corpus index lives under, as `<dir>/<corpus-id>/`.
    #[arg(long)]
    pub index_dir: PathBuf,
}

pub async fn run(args: FinalizeArgs) -> Result<()> {
    let corpus_id = &args.corpus_id;
    let noop_embed: EmbedFn = Arc::new(|_text: &str| Box::pin(async move { Ok(vec![0.0_f32; 1]) }));
    // No recipe is read: the recipes dir is the index dir, as `index` does
    // for a recipe it never registers.
    let engine = CorpusEngine::new(args.index_dir.clone(), args.index_dir.clone(), noop_embed);
    let outcome = engine.finalise_solo_ingest(corpus_id);
    tracing::debug!(
        corpus = %corpus_id,
        index_dir = %args.index_dir.display(),
        ?outcome,
        "svrn-ingest finalize: promotion decided"
    );
    match outcome {
        Ok(true) => {
            eprintln!("Promoted {corpus_id}-partition-local/ → {corpus_id}/");
            Ok(())
        }
        Ok(false) => {
            eprintln!(
                "Nothing to do for '{corpus_id}': either no partition-local dir, \
                 a peer partition is present (use `coordinate_merge`), or canonical \
                 Lance is already finalized."
            );
            Ok(())
        }
        Err(e) => anyhow::bail!("finalize failed: {e}"),
    }
}
