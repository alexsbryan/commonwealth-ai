// SPDX-License-Identifier: AGPL-3.0-or-later
//! `DocumentAssetManager` — the type, its construction, and phase 1 INGEST.
//!
//! Parse, chunk, embed and build a structural skeleton. Embedding and skeleton
//! extraction run concurrently: once embedding completes RAG is available while
//! the skeleton keeps building.

// One cooperating unit split for size (ARCH §3.2), not independent modules:
// the manager, its three phases and the skeleton free functions all name each
// other's types. The import surface stays in `mod.rs`.
use super::*;

// ─── Manager ─────────────────────────────────────────────────

/// Manages the lifecycle of document assets: ingest, route, execute.
///
/// Holds references to inference (for embedding + LLM calls) and
/// storage (for persisting assets and chunks). Does not own a
/// CorpusEngine — document assets use the existing `DocumentStore`
/// chunk storage with FTS5 search, not LanceDB corpus indexes.
pub struct DocumentAssetManager {
    pub(super) inference: Arc<dyn InferenceProvider>,
    pub(super) store: Arc<dyn StateStore>,
    /// Optional local NER model for the T2 entity pass. See
    /// [`Self::with_entity_extractor`]. `None` keeps the LLM path.
    pub(super) entity_extractor: Option<Arc<dyn EntityExtractor>>,
}

impl DocumentAssetManager {
    pub fn new(inference: Arc<dyn InferenceProvider>, store: Arc<dyn StateStore>) -> Self {
        Self {
            inference,
            store,
            entity_extractor: None,
        }
    }

    /// Use a local NER model for the T2 entity pass instead of the LLM.
    ///
    /// The window pass asks a 4B generative model to do one job — "list
    /// the named entities in this text" — which is what an NER model is
    /// for. On the 301-chunk bench subset that pass was ~50.9k of 77.6k
    /// total prompt tokens (66%), and token volume is what dominates
    /// ingest cost on a CPU-only host, where there is no idle batch
    /// capacity for scheduling tricks to harvest.
    ///
    /// Injected as `dyn EntityExtractor` rather than depending on
    /// `sovereign-gliner` directly: this crate stays free of the ONNX
    /// dependency, and hosts without the model installed simply don't
    /// call this. Extraction still degrades to the LLM per-window when
    /// the extractor returns nothing (see `build_skeleton`), so a
    /// not-yet-warm `LazyGlinerExtractor` cannot silently empty the
    /// skeleton.
    pub fn with_entity_extractor(mut self, extractor: Arc<dyn EntityExtractor>) -> Self {
        self.entity_extractor = Some(extractor);
        self
    }

    /// Parse + chunk + create the `Pending` asset record. No inference,
    /// so it's fast enough to call inline before returning to the UI.
    ///
    /// Returns a [`PreparedIngest`] whose `asset.id` is the id that
    /// [`run_ingest`](Self::run_ingest) will emit every progress event
    /// under. Callers that want to surface a live banner (the desktop
    /// upload command) call `prepare`, return `prepared.asset` to the
    /// UI, then spawn `run_ingest` — UI and events share one id.
    pub async fn prepare(&self, file_path: &std::path::Path) -> Result<PreparedIngest> {
        let parsed = parse_file(file_path)?;
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document")
            .to_string();

        let text_chunks = chunk_text(&parsed.content);
        let word_count = parsed.content.split_whitespace().count();
        let chunk_count = text_chunks.len();
        let file_size_mb = std::fs::metadata(file_path)
            .map(|m| m.len() as f32 / (1024.0 * 1024.0))
            .unwrap_or(0.0);

        let asset_id = uuid::Uuid::new_v4().to_string();
        let index_id = format!("doc-{asset_id}");

        // Infer title from filename (strip extension).
        let title = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&filename)
            .replace(['_', '-'], " ");

        let asset = DocumentAsset {
            id: asset_id,
            title,
            filename,
            file_size_mb,
            word_count,
            chunk_count,
            document_type: DocumentTypeTag::Unknown,
            ingested_at: chrono::Utc::now(),
            index_id,
            skeleton: None,
            state: AssetState::Pending,
            owner: None,
        };
        self.store.save_document_asset(&asset).await?;

        Ok(PreparedIngest { asset, text_chunks })
    }

    /// Parse, chunk, embed, and enrich a file in one call. Convenience
    /// wrapper over `prepare` + `run_ingest` for callers (server, CLI,
    /// tests) that wait for completion and don't need the id early.
    ///
    /// The progress callback fires at each phase boundary so the
    /// frontend can update the UI in real time.
    pub async fn ingest(
        &self,
        file_path: &std::path::Path,
        on_progress: impl Fn(IngestProgress) + Send + Sync + 'static,
    ) -> Result<DocumentAsset> {
        let prepared = self.prepare(file_path).await?;
        self.run_ingest(prepared, on_progress).await
    }

    /// Run the embed + tiered-enrichment pipeline on an already-prepared
    /// asset. Emits every `IngestProgress` under `prepared.asset.id`.
    /// Long-running: embeds all chunks then builds the RAPTOR atlas.
    pub async fn run_ingest(
        &self,
        prepared: PreparedIngest,
        on_progress: impl Fn(IngestProgress) + Send + Sync + 'static,
    ) -> Result<DocumentAsset> {
        let PreparedIngest { asset, text_chunks } = prepared;
        let on_progress: Arc<dyn Fn(IngestProgress) + Send + Sync> = Arc::new(on_progress);

        let asset_id = asset.id.clone();
        let filename = asset.filename.clone();
        let word_count = asset.word_count;
        let chunk_count = asset.chunk_count;
        let file_size_mb = asset.file_size_mb;
        let index_id = asset.index_id.clone();

        on_progress(IngestProgress::Started {
            word_count,
            chunk_count,
            filename: filename.clone(),
            asset_id: asset_id.clone(),
        });

        // ── Concurrent: embedding + skeleton ────────────────
        //
        // Embedding and skeleton extraction run in parallel via
        // tokio::join!. Embedding uses batch calls for throughput.
        // Once embedding finishes, RAG queries work even while the
        // skeleton is still building.

        let source_id = format!("asset:{asset_id}");
        let text_chunks = Arc::new(text_chunks);

        // ── Embedding future ────────────────────────────────
        let embed_future = {
            let inference = Arc::clone(&self.inference);
            let store = Arc::clone(&self.store);
            let asset_id = asset_id.clone();
            let source_id = source_id.clone();
            let text_chunks = Arc::clone(&text_chunks);
            let on_progress = Arc::clone(&on_progress);

            async move {
                store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::Indexing {
                            chunks_done: 0,
                            chunks_total: chunk_count,
                        },
                    )
                    .await?;
                // Emit at 0% *before* the first batch so the banner flips
                // off "Queued…" the instant embedding starts — the embed
                // slot's lazy model load (tens of seconds, cold) lands in
                // this window, and without a 0% tick the UI looks frozen.
                on_progress(IngestProgress::Indexing {
                    done: 0,
                    total: chunk_count,
                });

                let now_ts = chrono::Utc::now().timestamp();
                let mut doc_chunks = Vec::with_capacity(chunk_count);

                // Batch embed in groups of 64 for throughput.
                const EMBED_BATCH: usize = 64;
                let mut all_embeddings: Vec<Option<Vec<f32>>> = Vec::with_capacity(chunk_count);

                for batch_start in (0..chunk_count).step_by(EMBED_BATCH) {
                    let batch_end = (batch_start + EMBED_BATCH).min(chunk_count);
                    let texts: Vec<String> = text_chunks[batch_start..batch_end]
                        .iter()
                        .map(|c| c.content.clone())
                        .collect();

                    match inference.embed_batch(&texts).await {
                        Ok(embeddings) => {
                            for emb in embeddings {
                                all_embeddings.push(Some(emb));
                            }
                        }
                        Err(_) => {
                            // Fallback: mark these as no-embedding.
                            for _ in batch_start..batch_end {
                                all_embeddings.push(None);
                            }
                        }
                    }

                    on_progress(IngestProgress::Indexing {
                        done: batch_end,
                        total: chunk_count,
                    });
                    let _ = store
                        .update_asset_state(
                            &asset_id,
                            &AssetState::Indexing {
                                chunks_done: batch_end,
                                chunks_total: chunk_count,
                            },
                        )
                        .await;
                }

                // Build DocumentChunk records.
                for (i, tc) in text_chunks.iter().enumerate() {
                    doc_chunks.push(DocumentChunk {
                        id: format!("{source_id}:{}", tc.index),
                        source: source_id.clone(),
                        content: tc.content.clone(),
                        chunk_index: tc.index,
                        embedding: all_embeddings.get(i).cloned().flatten(),
                        created_at: now_ts,
                        source_type: SourceType::UserDocument,
                        version: 0,
                        deleted_at: None,
                    });
                }

                store.store_chunks(&doc_chunks).await?;

                // RAG is now available.
                store
                    .update_asset_state(&asset_id, &AssetState::PartiallyReady)
                    .await?;
                on_progress(IngestProgress::RagAvailable {
                    asset_id: asset_id.clone(),
                });

                Ok::<(), sovereign_core::error::Error>(())
            }
        };

        // ── Tiered enrichment future (T2 → MultiHopReady → T3) ──
        //
        // Splits the prior monolithic skeleton phase into the two
        // tiered states defined in the proper-curried-peach plan:
        //
        //   T2: lean entity extraction + action atoms — yields a
        //       partial skeleton (entity_index + main_entities +
        //       actions + sections + structural_moments). Asset
        //       transitions to MultiHopReady. PPR multi-hop
        //       retrieval becomes available at this point.
        //
        //   T3: TextTiling segments + RAPTOR atlas + motif index +
        //       overview generation — fills in the remaining
        //       skeleton fields (segments, overview) AND populates
        //       the raptor_nodes + asset_motifs tables. Asset
        //       transitions to Ready. Full briefing-driven synthesis
        //       becomes available.
        //
        // Both phases run in the SAME future (not parallel with
        // embedding — they depend on chunks being persisted). The
        // foreground T3 is a deliberate change from the prior
        // background spawn: we want Ready to actually mean "all
        // enrichment landed," not "skeleton landed and RAPTOR is
        // still cooking."
        let skeleton_future = {
            let inference = Arc::clone(&self.inference);
            let store = Arc::clone(&self.store);
            let asset_id = asset_id.clone();
            let text_chunks = Arc::clone(&text_chunks);
            let on_progress = Arc::clone(&on_progress);
            let entity_extractor = self.entity_extractor.clone();

            async move {
                let doc_type = detect_document_type(&inference, &text_chunks).await;

                // ── T2 — entity extraction + action atoms ──────
                store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done: 0,
                            chunks_total: chunk_count,
                        },
                    )
                    .await?;

                let mut skeleton = build_skeleton(
                    &inference,
                    &store,
                    &asset_id,
                    &text_chunks,
                    &doc_type,
                    &on_progress,
                    entity_extractor.as_ref(),
                )
                .await?;

                // Persist partial skeleton + transition to
                // MultiHopReady so queries arriving in the
                // T3-window can use PPR.
                store
                    .save_asset_skeleton(&asset_id, &skeleton, &doc_type)
                    .await?;
                store
                    .update_asset_state(&asset_id, &AssetState::MultiHopReady)
                    .await?;
                on_progress(IngestProgress::MultiHopReady {
                    asset_id: asset_id.clone(),
                });

                // ── T3 — RAPTOR atlas + motifs + segments + overview ──
                // Re-emit BuildingSkeleton state so the UI's progress
                // bar reactivates for the T3 enrichment phase. The
                // chunks_done counter resets at this milestone — by
                // design (the visual reset signals a real capability
                // checkpoint, not just continuous work).
                store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done: 0,
                            chunks_total: chunk_count,
                        },
                    )
                    .await?;

                // Segments (TextTiling) + overview run concurrently —
                // both touch all chunks, neither depends on the other.
                // T1's persisted per-chunk embeddings are fetched and
                // reused for TextTiling (same model, same texts) — the
                // fetch is a local store read, the re-embed it replaces
                // was ~30s of embed-slot time per 300 chunks.
                let stored_embeddings: Option<Vec<Vec<f32>>> = {
                    let source_key = format!("asset:{asset_id}");
                    match store.get_chunks_by_source(&source_key).await {
                        Ok(mut docs) if docs.len() == text_chunks.len() => {
                            docs.sort_by_key(|d| d.chunk_index);
                            let embs: Vec<Vec<f32>> =
                                docs.into_iter().filter_map(|d| d.embedding).collect();
                            (embs.len() == text_chunks.len()).then_some(embs)
                        }
                        _ => None,
                    }
                };
                let segments_future = extract_segments(
                    &inference,
                    &text_chunks,
                    &skeleton.main_entities,
                    doc_type.clone(),
                    stored_embeddings,
                );
                let overview_future = generate_overview(&inference, &text_chunks, &doc_type);
                let (segments, overview) = tokio::join!(segments_future, overview_future);
                skeleton.segments = segments;
                skeleton.overview = overview;
                // Coarse progress checkpoint after segments+overview —
                // small fraction of T3 wall-clock but worth a tick so
                // the UI doesn't look frozen during the embedding
                // window of TextTiling + the single overview LLM call.
                on_progress(IngestProgress::BuildingSkeleton {
                    done: (chunk_count as f32 * 0.10).round() as usize,
                    total: chunk_count,
                });
                let _ = store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done: (chunk_count as f32 * 0.10).round() as usize,
                            chunks_total: chunk_count,
                        },
                    )
                    .await;

                // RAPTOR + motif build. Failures are logged inside
                // the helper and degrade quality without breaking
                // ingest — the partial skeleton from T2 is still a
                // valid retrieval surface. Progress events fire at
                // coarse phase boundaries inside this helper so the
                // UI bar continues to advance through the ~4-min
                // window.
                let source_key = format!("asset:{asset_id}");
                build_and_persist_raptor_atlas(
                    &inference,
                    &store,
                    &asset_id,
                    &source_key,
                    doc_type.clone(),
                    &on_progress,
                    chunk_count,
                )
                .await;

                // Final skeleton save (with overview + segments now
                // populated) + Ready transition.
                store
                    .save_asset_skeleton(&asset_id, &skeleton, &doc_type)
                    .await?;
                store
                    .update_asset_state(&asset_id, &AssetState::Ready)
                    .await?;
                on_progress(IngestProgress::Ready {
                    asset_id: asset_id.clone(),
                    main_entities: skeleton.main_entities.len(),
                    structural_moments: skeleton.structural_moments.len(),
                });

                Ok::<(DocumentSkeleton, DocumentTypeTag), sovereign_core::error::Error>((
                    skeleton, doc_type,
                ))
            }
        };

        // Embedding + tiered enrichment run concurrently. Embedding
        // typically finishes first (pure computation), flipping the
        // asset to PartiallyReady (T1 done) so cosine retrieval works
        // immediately. The enrichment future then walks T2 → T3.
        let (embed_result, skeleton_result) = tokio::join!(embed_future, skeleton_future);

        embed_result?;
        let (skeleton, doc_type) = skeleton_result?;

        // (T3 used to be a tokio::spawn background task here. It
        // now runs inside skeleton_future above, so Ready means
        // *all* enrichment has landed.)

        Ok(DocumentAsset {
            id: asset_id,
            title: asset.title,
            filename,
            file_size_mb,
            word_count,
            chunk_count,
            document_type: doc_type,
            ingested_at: asset.ingested_at,
            index_id,
            skeleton: Some(skeleton),
            state: AssetState::Ready,
            owner: None,
        })
    }

    /// Rebuild the skeleton for an already-ingested asset, working entirely
    /// from stored chunks — no file path required, no re-parsing, no
    /// re-embedding.
    ///
    /// Used two ways:
    /// 1. The `rebuild_document_skeleton` Tauri command (user-initiated).
    /// 2. Auto-heal: when `ask_document` sees a skeleton-less asset, it
    ///    spawns this in the background so subsequent turns get smarter
    ///    routing without the user doing anything.
    ///
    /// Returns the freshly-built skeleton; the asset's stored skeleton and
    /// `document_type` are updated atomically via `save_asset_skeleton`, and
    /// the asset state transitions to `Ready` on success.
    pub async fn rebuild_skeleton(&self, asset_id: &str) -> Result<DocumentSkeleton> {
        tracing::info!(asset_id = %asset_id, "rebuild_skeleton — begin");

        let asset = self
            .store
            .get_document_asset(asset_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("document asset {asset_id}")))?;

        let source_id = asset.source_key();
        let mut doc_chunks = self.store.get_chunks_by_source(&source_id).await?;

        if doc_chunks.is_empty() {
            tracing::warn!(
                asset_id = %asset_id,
                source_id = %source_id,
                "rebuild_skeleton — no chunks found; cannot rebuild"
            );
            return Err(Error::NotFound(format!(
                "no chunks for document asset {asset_id} — needs re-ingest from source file"
            )));
        }

        // DocumentChunks come back in insertion order but we want them
        // ordered by chunk_index so the skeleton batches reflect the
        // document's narrative order.
        doc_chunks.sort_by_key(|c| c.chunk_index);

        let text_chunks: Vec<TextChunk> = doc_chunks
            .into_iter()
            .map(|c| TextChunk {
                index: c.chunk_index,
                content: c.content,
            })
            .collect();
        let chunk_count = text_chunks.len();

        tracing::debug!(
            asset_id = %asset_id,
            chunks = chunk_count,
            "rebuild_skeleton — chunks loaded from store"
        );

        self.store
            .update_asset_state(
                asset_id,
                &AssetState::BuildingSkeleton {
                    chunks_done: 0,
                    chunks_total: chunk_count,
                },
            )
            .await?;

        let doc_type = detect_document_type(&self.inference, &text_chunks).await;

        // No UI progress on rebuilds — state updates inside build_skeleton
        // are the only signal. Callers who want per-batch feedback should
        // run a full re-ingest.
        let noop_progress: Arc<dyn Fn(IngestProgress) + Send + Sync> = Arc::new(|_| ());

        let skeleton = build_skeleton(
            &self.inference,
            &self.store,
            asset_id,
            &text_chunks,
            &doc_type,
            &noop_progress,
            self.entity_extractor.as_ref(),
        )
        .await?;

        self.store
            .save_asset_skeleton(asset_id, &skeleton, &doc_type)
            .await?;
        self.store
            .update_asset_state(asset_id, &AssetState::Ready)
            .await?;

        tracing::info!(
            asset_id = %asset_id,
            doc_type = ?doc_type,
            sections = skeleton.sections.len(),
            entities = skeleton.main_entities.len(),
            "rebuild_skeleton — done"
        );

        Ok(skeleton)
    }

    /// Rebuild ONLY the RAPTOR atlas + motif index for an existing
    /// Ready asset, leaving the legacy skeleton untouched. Used by
    /// the bench (`--rebuild-raptor`) and by future admin paths to
    /// populate the new atlas on documents ingested before the
    /// RAPTOR pipeline shipped — without paying for a full ~20-min
    /// skeleton rebuild.
    ///
    /// Returns `Ok(())` on success. Errors propagate from the store
    /// (no chunks, write failure) or the inference layer (embed /
    /// summarize failures). Per `build_and_persist_raptor_atlas`'s
    /// own contract, internal failures inside RAPTOR or motif
    /// extraction are logged + swallowed; this entry point only
    /// errors on upfront preconditions.
    pub async fn rebuild_raptor_atlas(&self, asset_id: &str) -> Result<()> {
        tracing::info!(asset_id = %asset_id, "rebuild_raptor_atlas — begin");
        let asset = self
            .store
            .get_document_asset(asset_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("document asset {asset_id}")))?;
        let source_key = asset.source_key();
        let doc_type = asset.document_type.clone();
        let chunk_count = asset.chunk_count;
        // Rebuild path has no UI progress channel — supply a noop
        // callback so the helper's signature can stay uniform with
        // the main ingest path.
        let noop_progress: Arc<dyn Fn(IngestProgress) + Send + Sync> = Arc::new(|_| ());
        build_and_persist_raptor_atlas(
            &self.inference,
            &self.store,
            asset_id,
            &source_key,
            doc_type,
            &noop_progress,
            chunk_count,
        )
        .await;
        Ok(())
    }

    /// Route a user's question to the right operation type, then
    /// execute it and return the response with source citations.
    pub async fn ask(
        &self,
        asset: &DocumentAsset,
        request: &str,
        on_progress: impl Fn(OperationProgress) + Send + Sync,
    ) -> Result<(String, DocumentAssetOperation, Vec<String>)> {
        let start = std::time::Instant::now();
        tracing::info!(
            asset_id = %asset.id,
            title = %asset.title,
            doc_type = ?asset.document_type,
            has_skeleton = asset.skeleton.is_some(),
            request_chars = request.len(),
            "DocumentAssetManager::ask — begin"
        );

        let operation = self.route(asset, request).await?;

        tracing::info!(
            asset_id = %asset.id,
            operation = %operation.label(),
            "DocumentAssetManager::ask — routed"
        );

        on_progress(OperationProgress::Routing {
            operation: operation.label().to_string(),
        });

        let output = self
            .execute_operation(asset, request, &operation, &on_progress)
            .await?;

        tracing::info!(
            asset_id = %asset.id,
            operation = %operation.label(),
            response_chars = output.text.len(),
            source_count = output.citations.len(),
            total_latency_ms = start.elapsed().as_millis() as u64,
            "DocumentAssetManager::ask — done"
        );

        // `ask()` stays on its old 3-tuple API for HTTP callers that only
        // need raw content strings. Tauri callers use `execute_operation`
        // directly and get the full `ExecutionOutput`.
        let sources: Vec<String> = output.citations.iter().map(|c| c.content.clone()).collect();

        Ok((output.text, operation, sources))
    }

    /// Delete an asset and its chunks.
    pub async fn delete(&self, id: &str) -> Result<()> {
        // Delete chunks from the document store.
        let source_id = format!("asset:{}", id);
        if let Ok(chunks) = self.store.get_chunks_by_source(&source_id).await {
            if !chunks.is_empty() {
                // Soft-delete by overwriting with empty + deleted_at.
                // The store's delete_chunks_by_corpus doesn't apply here
                // since these are UserDocument source type.
                // For now, we just delete the asset record — chunks are
                // orphaned but small. A future cleanup job can GC them.
            }
        }
        self.store.delete_document_asset(id).await
    }
}
