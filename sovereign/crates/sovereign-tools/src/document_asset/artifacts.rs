// SPDX-License-Identifier: AGPL-3.0-or-later
//! Derived artifacts built from an ingested asset: the RAPTOR node tree and
//! the atlas, each with a checkpointed variant so a long build can resume.

// One cooperating unit split for size (ARCH §3.2), not independent modules:
// the manager, its three phases and the skeleton free functions all name each
// other's types. The import surface stays in `mod.rs`.
use super::*;

/// Build the RAPTOR atlas + motif index for an asset and persist them.
/// Runs inside the T3 phase of the tiered ingest pipeline. By this
/// point chunks-with-embeddings are guaranteed to be in the store
/// (T1 persisted them; T2 ran on them).
///
/// Emits `IngestProgress::BuildingSkeleton` progress events at coarse
/// phase boundaries (chunks-fetched, RAPTOR tree built, RAPTOR
/// persisted, motifs done) so the UI's progress bar moves through
/// the ~5-min T3 window. The progress fractions are mapped onto
/// `chunks_total` so the existing UI math (chunks_done / chunks_total)
/// continues to work — without this the bar would stay at 0/N for
/// the entire T3 duration, which made the May-22 fresh-ingest probe
/// look stuck on MultiHopReady.
///
/// Errors are logged and swallowed: the T2 skeleton is the durable
/// retrieval surface, RAPTOR is additive. A RAPTOR build failure
/// degrades briefing quality at Ready but never breaks attach.
/// Pure corpus-free RAPTOR + motif builder. Takes pre-fetched chunks
/// + embeddings and returns the artifacts the persistent variants
/// (attached-doc `build_and_persist_raptor_atlas`, folder
/// `FolderTieredProvider`) write into their respective tables.
///
/// Returns `Ok((nodes, motifs))` on success. `Err` is reserved for
/// RAPTOR-tree-build failures — motif extraction + classification is
/// best-effort (returns empty motif vec on classifier failure rather
/// than failing the whole call) because the briefing layer renders
/// motifs as additive: a missing motif index degrades signposts but
/// doesn't break retrieval.
///
/// `chunks` and `embeddings` MUST be the same length and in matching
/// order; the caller is responsible for filtering out chunks with
/// no embedding.
pub(crate) async fn build_atlas_artifacts(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
) -> Result<(Vec<RaptorNode>, Vec<AssetMotif>)> {
    build_atlas_artifacts_with_checkpoint(
        inference,
        chunks,
        embeddings,
        doc_type,
        None,
        None,
        None,
        crate::raptor_atlas::SummaryMode::Abstractive,
        None,
    )
    .await
}

/// RAPTOR tree only — no motif pass.
///
/// This is the entry point for the **folder/vault** path, and it is a
/// separate function rather than a flag on purpose: that path's motif
/// table (`conv_motifs`) had one INSERT, two DELETEs and no reader
/// anywhere in the workspace, while the pass itself cost **42.8% of a
/// cold vault build** (22.3m of 52m03s, 330 notes, measured
/// 2026-08-02). Deleting the write is only half the fix — as long as a
/// caller *could* ask this builder for motifs, the expensive pass can
/// come back by accident. It can't: the folder path calls a function
/// that has no motif concept in its return type.
///
/// The attached-document path keeps motifs and calls
/// [`build_atlas_artifacts_with_checkpoint`] instead — `asset_motifs`
/// is a different table with a real reader (`list_asset_motifs`) that
/// the document briefing renders.
pub(crate) async fn build_raptor_nodes_with_checkpoint(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
    checkpoint: Option<&crate::raptor_checkpoint::RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink>>,
    // User-authored summary correction, threaded to the RAPTOR
    // summarization prompt (the "flag a wrong summary" revision loop).
    correction_hint: Option<&str>,
    summary_mode: crate::raptor_atlas::SummaryMode,
    // T1 P1.2 override: `None` = the default gate (verify every
    // abstractive summary). Corpus-scale callers pass `Sample(p)` for
    // SP3 economics, or `Off` to opt out explicitly.
    verify_policy: Option<crate::summary_verify::VerifyPolicy>,
) -> Result<Vec<RaptorNode>> {
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    // RAPTOR tree — the long sub-phase. Errors propagate so callers
    // can transition state to Failed.
    //
    // Attached-document abstractive summaries are VERIFIER-GATED
    // (T1 P1.2): every LLM summary is decomposed into claims and
    // judged against its own member texts before persisting — pass,
    // one steered retry, or the extractive floor. Policy `On` because
    // per-document trees are small (tens of nodes); the SP3 sampling
    // economics apply to corpus-scale builds, which go through
    // `enrich raptor --verify-summaries` instead. Extractive builds
    // skip the gate by construction (quotes need no verification).
    let policy = verify_policy.unwrap_or(crate::summary_verify::VerifyPolicy::On);
    let verify = match (summary_mode, policy) {
        (crate::raptor_atlas::SummaryMode::Extractive, _)
        | (_, crate::summary_verify::VerifyPolicy::Off) => None,
        (crate::raptor_atlas::SummaryMode::Abstractive, policy) => {
            Some(Arc::new(crate::summary_verify::VerifyCtx {
                verifier: Arc::new(crate::summary_verify::JudgeSummaryVerifier::new(
                    Arc::clone(inference),
                )),
                policy,
                stats: Arc::new(crate::summary_verify::VerifyStats::default()),
            }))
        }
    };
    let t_tree = std::time::Instant::now();
    let nodes = crate::raptor_atlas::build_raptor_atlas_with_verify(
        inference,
        chunks,
        embeddings,
        doc_type.clone(),
        checkpoint,
        progress,
        correction_hint,
        summary_mode,
        verify.clone(),
    )
    .await
    .map_err(|e| Error::Execution(format!("build_raptor_atlas: {e}")))?;
    let tree_s = t_tree.elapsed().as_secs_f32();
    if let Some(ctx) = verify.as_ref() {
        tracing::info!(
            stats = %ctx.stats.summary_line(),
            "document_asset: summary verification gate (T1 P1.2)"
        );
    }

    // [t3-profile] turbocharge-arc phase split (2026-07-24) — stderr on
    // the driving process; promote to allowlisted tracing spans when the
    // arc lands.
    eprintln!(
        "      [t3-profile] raptor_tree={tree_s:.1}s (nodes={})",
        nodes.len()
    );

    Ok(nodes)
}

/// RAPTOR tree **plus** the TF-IDF motif index — the attached-document
/// path, whose `asset_motifs` rows the document briefing actually
/// renders.
///
/// See [`build_raptor_nodes_with_checkpoint`] for why the folder/vault
/// path deliberately cannot reach this function.
pub(crate) async fn build_atlas_artifacts_with_checkpoint(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
    checkpoint: Option<&crate::raptor_checkpoint::RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink>>,
    correction_hint: Option<&str>,
    summary_mode: crate::raptor_atlas::SummaryMode,
    verify_policy: Option<crate::summary_verify::VerifyPolicy>,
) -> Result<(Vec<RaptorNode>, Vec<AssetMotif>)> {
    if chunks.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let nodes = build_raptor_nodes_with_checkpoint(
        inference,
        chunks,
        embeddings,
        doc_type.clone(),
        checkpoint,
        progress,
        correction_hint,
        summary_mode,
        verify_policy,
    )
    .await?;

    // Convert ChunkInput → TextChunk for the existing motif extractor.
    let t_motifs = std::time::Instant::now();
    let text_chunks: Vec<TextChunk> = chunks
        .iter()
        .map(|c| TextChunk {
            content: c.content.clone(),
            index: c.chunk_id as usize,
        })
        .collect();
    // Wider candidate pool (was 100) since the df>=1 floor lets
    // rare-but-distinctive scene markers reach the LLM classifier.
    let candidates = extract_motif_candidates(&text_chunks, 200);
    let motifs = classify_motifs(inference, candidates, doc_type).await;
    // `motifs→` is the classified count; the old label said
    // `motif_candidates→` and was reading the wrong side of
    // `classify_motifs`.
    eprintln!(
        "      [t3-profile] motifs={:.1}s (motifs→{})",
        t_motifs.elapsed().as_secs_f32(),
        motifs.len(),
    );

    Ok((nodes, motifs))
}

pub(super) async fn build_and_persist_raptor_atlas(
    inference: &Arc<dyn InferenceProvider>,
    store: &Arc<dyn StateStore>,
    asset_id: &str,
    source_key: &str,
    doc_type: DocumentTypeTag,
    on_progress: &Arc<dyn Fn(IngestProgress) + Send + Sync>,
    chunks_total: usize,
) {
    let started = std::time::Instant::now();
    tracing::info!(asset_id, "raptor_atlas: starting T3 build");

    // Helper to emit + persist a coarse progress checkpoint. The
    // fractions are deliberate guesses — RAPTOR's leaf-summarisation
    // doesn't expose per-cluster progress, so we mark phase
    // boundaries instead. UI shows monotonic movement; users see
    // "something is happening" instead of a frozen 0/N bar.
    let emit = |fraction: f32| {
        let done = ((chunks_total as f32 * fraction).round() as usize).min(chunks_total);
        on_progress(IngestProgress::BuildingSkeleton {
            done,
            total: chunks_total,
        });
        let asset_id = asset_id.to_string();
        let store = Arc::clone(store);
        // Fire-and-forget the state update — failure is non-fatal,
        // the UI just doesn't show this checkpoint. Spawn so we
        // don't block the T3 build path on the write.
        tokio::spawn(async move {
            let _ = store
                .update_asset_state(
                    &asset_id,
                    &AssetState::BuildingSkeleton {
                        chunks_done: done,
                        chunks_total,
                    },
                )
                .await;
        });
    };

    // Fetch chunks (which carry embeddings from the embed phase).
    let chunks = match store.get_chunks_by_source(source_key).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(asset_id, error = %e, "raptor_atlas: get_chunks_by_source failed");
            return;
        }
    };
    emit(0.20);
    let total = chunks.len();
    let mut raptor_chunks: Vec<ChunkInput> = Vec::with_capacity(total);
    let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(total);
    for c in &chunks {
        if let Some(emb) = c.embedding.as_ref() {
            raptor_chunks.push(ChunkInput {
                chunk_id: c.chunk_index as u32,
                content: c.content.clone(),
            });
            embeddings.push(emb.clone());
        }
    }
    if raptor_chunks.is_empty() {
        tracing::warn!(
            asset_id,
            total,
            "raptor_atlas: no embedded chunks; skipping"
        );
        return;
    }

    // Build artifacts via the corpus-free helper. Errors here are
    // RAPTOR-tree-build failures (the only Err path); motif extraction
    // is best-effort inside the helper and returns an empty vec on
    // classifier failure.
    let (nodes, motifs) =
        match build_atlas_artifacts(inference, &raptor_chunks, &embeddings, doc_type).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(asset_id, error = %e, "raptor_atlas: build_atlas_artifacts failed");
                return;
            }
        };
    let node_count = nodes.len();
    let motif_count = motifs.len();
    let distinctive_count = motifs.iter().filter(|m| m.is_distinctive).count();
    // RAPTOR + motif build complete — mark ~75% so the bar moved
    // through the longest opaque wait.
    emit(0.75);

    if let Err(e) = store.save_raptor_nodes(asset_id, &nodes).await {
        tracing::warn!(asset_id, error = %e, "raptor_atlas: save_raptor_nodes failed");
        return;
    }
    emit(0.80);

    if let Err(e) = store.save_asset_motifs(asset_id, &motifs).await {
        tracing::warn!(asset_id, error = %e, "raptor_atlas: save_asset_motifs failed");
        return;
    }
    emit(0.95);

    tracing::info!(
        asset_id,
        chunks = raptor_chunks.len(),
        nodes = node_count,
        motif_candidates = motif_count,
        distinctive_motifs = distinctive_count,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "raptor_atlas: T3 build complete"
    );
}
