// SPDX-License-Identifier: AGPL-3.0-or-later
//! `FieldModelEngine` — domain-agnostic coordinator for the field model
//! enrichment pipeline.
//!
//! Contains **zero domain-specific logic**. The only match on domain
//! strings is in `from_recipe()`, which is a factory method.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::types::{EmbedFn, InferenceFn};

use super::checkpoint::{EnrichmentCheckpoint, EnrichmentPhase};
use super::clustering::{cluster_embeddings, ClusterResult, EnrichmentProgress, FieldModelStats};
use super::domain::{Domain, SkeletonStorage};
use super::fault_lines::detect_fault_lines;
use super::open_questions::{detect_open_questions, OpenQuestion};
use super::skeleton::{
    CanonicalQuestion, FieldSkeleton, PartialSkeleton, SkeletonOpenQuestion, SkeletonQuestion,
};
use super::skeleton_parse::{
    deduplicate_questions, extract_json_from_response, parse_skeleton_response, ParseResult,
};

/// The domain-agnostic field model enrichment engine.
///
/// Orchestrates five phases:
/// 1. Skeleton extraction from overview chunks
/// 2. HDBSCAN clustering (no inference)
/// 2b. Cluster labeling
/// 3. Skeleton ↔ cluster alignment
/// 4. Fault line detection
/// 5. Open question detection
pub struct FieldModelEngine {
    embed: EmbedFn,
    inference: InferenceFn,
    domain: Arc<dyn Domain>,
}

impl std::fmt::Debug for FieldModelEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldModelEngine")
            .field("domain", &self.domain.id())
            .finish()
    }
}

impl FieldModelEngine {
    pub fn new(embed: EmbedFn, inference: InferenceFn, domain: Arc<dyn Domain>) -> Self {
        Self {
            embed,
            inference,
            domain,
        }
    }

    /// Construct from recipe configuration.
    /// Looks up the domain via `DomainRegistry::builtin()`.
    pub fn from_recipe(
        recipe: &crate::recipe::Recipe,
        embed: EmbedFn,
        inference: InferenceFn,
    ) -> Result<Self> {
        let domain_id = recipe
            .enrichment
            .as_ref()
            .and_then(|e| e.domain.as_deref())
            .unwrap_or("philosophy");

        let registry = super::domain_registry::DomainRegistry::builtin();
        let domain = registry
            .get(domain_id)
            .ok_or_else(|| Error::UnknownEnrichmentDomain(domain_id.to_string()))?;

        Ok(Self::new(embed, inference, domain))
    }

    /// Run the full enrichment pipeline.
    /// Resumes from the last completed phase if a checkpoint exists.
    pub async fn enrich(
        &self,
        index: &CorpusIndex,
        progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
    ) -> Result<FieldModelStats> {
        let index_dir = index.path();

        // ── Load or create checkpoint ─────────────────────────────────
        let mut checkpoint =
            EnrichmentCheckpoint::load(&index_dir)?.unwrap_or_else(|| EnrichmentCheckpoint {
                schema_version: 1,
                corpus_id: index.corpus_id().to_string(),
                domain_id: self.domain.id().to_string(),
                prompt_version: "1.0.0".to_string(),
                started_at: chrono::Utc::now().to_rfc3339(),
                last_updated: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            });

        let start_phase = checkpoint.next_phase();
        if start_phase == EnrichmentPhase::Complete {
            // No per-table stats are computed anywhere (the old
            // `compute_stats` was a zeros stub) — completion returns the
            // same default the skeleton records.
            return Ok(FieldModelStats::default());
        }

        if checkpoint.interrupted {
            progress(EnrichmentProgress::Resuming {
                from_phase: format!("{:?}", start_phase),
            });
            checkpoint.interrupted = false;
        }

        // ── Phase 1: Skeleton from overview chunks ────────────────────
        let skeleton = if checkpoint.phase_1_complete {
            progress(EnrichmentProgress::PhaseSkipped {
                phase: 1,
                name: "Skeleton extraction",
            });
            // Reload skeleton from file
            index
                .load_field_skeleton()?
                .map(|s| {
                    let mut ps = PartialSkeleton::new(self.domain.id());
                    for q in &s.canonical_questions {
                        ps.questions.push(SkeletonQuestion {
                            id: q.id.clone(),
                            question: q.question.clone(),
                            question_type: q.question_type.clone(),
                            status: q.status.clone(),
                            primary_article_ids: q.primary_entries.clone(),
                            positions: q.positions.clone(),
                        });
                    }
                    ps
                })
                .unwrap_or_else(|| PartialSkeleton::new(self.domain.id()))
        } else {
            if checkpoint.phase_1_batches_done > 0 {
                progress(EnrichmentProgress::Resuming {
                    from_phase: format!(
                        "Skeleton extraction (batch {})",
                        checkpoint.phase_1_batches_done
                    ),
                });
            }
            progress(EnrichmentProgress::Phase {
                phase: 1,
                name: "Skeleton extraction",
                note: "",
            });
            let overview = self.get_overview_chunks(index).await?;
            let skeleton = self
                .extract_skeleton_phase(&overview, index, &mut checkpoint, progress)
                .await?;
            checkpoint.phase_1_complete = true;
            checkpoint.phase_1_batches_done = 0; // clear for cleanliness
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
            skeleton
        };

        // ── Phase 1b: Entity extraction (opt-in per domain) ──────────
        //
        // Personal + Conversational domains override
        // `entity_extraction_prompt` to produce typed Person /
        // Organization / Initiative atoms with Involves edges. All
        // other domains use the default impl (returns None) and the
        // step is a no-op — the existing five phases run unchanged.
        //
        // Resumable: passing `&mut checkpoint` lets the inner
        // driver persist each batch's parsed response to
        // `_phase_1b_parsed.jsonl` and update
        // `phase_1b_batches_done`. A process killed mid-phase
        // resumes on the next run rather than re-inferring
        // already-completed batches.
        if !checkpoint.phase_1b_complete {
            if checkpoint.phase_1b_batches_done > 0 {
                progress(EnrichmentProgress::Resuming {
                    from_phase: format!(
                        "Entity extraction (batch {})",
                        checkpoint.phase_1b_batches_done
                    ),
                });
            }
            let all_chunks = index.all_chunks().await?;
            let result = super::entity_extraction::run_and_write_entity_extraction(
                &all_chunks,
                self.domain.as_ref(),
                self.inference.clone(),
                &index_dir,
                Some(&mut checkpoint),
                progress,
            )
            .await?;
            tracing::info!(
                domain = self.domain.id(),
                entities = result.entities.len(),
                edges = result.edges.len(),
                failures = result.failures.len(),
                batches_run = result.batches_run,
                "phase_1b: entity extraction complete"
            );
            checkpoint.phase_1b_complete = true;
            checkpoint.phase_1b_batches_done = 0; // clear for cleanliness
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
        } else {
            progress(EnrichmentProgress::PhaseSkipped {
                phase: 2,
                name: "Entity extraction",
            });
        }

        // ── Phase 2: Cluster embeddings ───────────────────────────────
        let clusters = if checkpoint.phase_2_complete {
            progress(EnrichmentProgress::PhaseSkipped {
                phase: 2,
                name: "Clustering",
            });
            // Re-clustering is expensive; for now just return empty on resume.
            // A full implementation would reload cluster state from the table.
            ClusterResult {
                assignments: std::collections::HashMap::new(),
                clusters: Vec::new(),
                noise_count: 0,
            }
        } else {
            let clusters =
                cluster_embeddings(index, &self.domain.clustering_config(), progress).await?;
            checkpoint.phase_2_complete = true;
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
            clusters
        };

        // ── Phase 2b: Label clusters ──────────────────────────────────
        let clusters = if checkpoint.phase_2b_complete {
            progress(EnrichmentProgress::PhaseSkipped {
                phase: 3,
                name: "Cluster labeling",
            });
            clusters
        } else {
            let labeled = self.label_clusters_phase(index, clusters, progress).await?;
            checkpoint.phase_2b_complete = true;
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
            labeled
        };

        // ── Phase 3: Alignment ────────────────────────────────────────
        if !checkpoint.phase_3_complete {
            let _alignment = super::alignment::align_clusters(
                index,
                &clusters,
                &skeleton,
                &self.embed,
                &self.inference,
                &self.domain.alignment_config(),
                progress,
            )
            .await?;
            checkpoint.phase_3_complete = true;
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
        } else {
            progress(EnrichmentProgress::PhaseSkipped {
                phase: 4,
                name: "Alignment",
            });
        }

        // ── Phase 4: Fault line detection ─────────────────────────────
        if !checkpoint.phase_4_complete {
            let _alignment_result = super::alignment::AlignmentResult {
                aligned: std::collections::HashMap::new(),
                unaligned_promoted: 0,
            };
            detect_fault_lines(
                index,
                &clusters,
                &_alignment_result,
                &self.inference,
                &self.domain.fault_line_config(),
                self.domain.as_ref(),
                progress,
            )
            .await?;
            checkpoint.phase_4_complete = true;
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
        } else {
            progress(EnrichmentProgress::PhaseSkipped {
                phase: 5,
                name: "Fault lines",
            });
        }

        // ── Phase 5: Open questions ───────────────────────────────────
        // I3: the detected open questions are retained and threaded into the
        // JSON skeleton below so `render_landscape` can surface them. Before
        // this they were computed and dropped at both this site and the
        // skeleton writer.
        let open_questions = if !checkpoint.phase_5_complete {
            let detected = detect_open_questions(
                index,
                &clusters,
                &self.inference,
                self.domain.as_ref(),
                progress,
            )
            .await?;
            checkpoint.phase_5_complete = true;
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
            detected
        } else {
            progress(EnrichmentProgress::PhaseSkipped {
                phase: 6,
                name: "Open questions",
            });
            // On resume, clusters are not reloaded (Phase 2 returns an empty
            // `ClusterResult`), so re-detection here would yield nothing. The
            // JSON skeleton therefore carries open questions only after an
            // uninterrupted run — the same limitation fault-line persistence
            // has. A crash between Phase 5 and finalize is the sole window
            // where this drops data; a clean re-enrich recovers it.
            Vec::new()
        };

        // ── Finalize ──────────────────────────────────────────────────
        // `field_stats` in the skeleton has always been zeros (the old
        // `compute_stats` stub never queried anything) — keep the shape,
        // state the truth.
        let stats = FieldModelStats::default();

        // Write JSON skeleton if the domain requests it.
        if let SkeletonStorage::JsonAndLance = self.domain.skeleton_storage() {
            self.write_json_skeleton(index, &skeleton, &stats, &open_questions)?;
        }

        // Clear checkpoint — clean completion.
        EnrichmentCheckpoint::clear(&index_dir)?;

        Ok(stats)
    }

    // ── Private helpers ─────────────────────────────────────────────

    async fn get_overview_chunks(
        &self,
        index: &CorpusIndex,
    ) -> Result<Vec<crate::index::StoredChunk>> {
        let filter = self.domain.overview_filter();
        let mut all = index.all_chunks().await?;
        let total = all.len();

        // Sort by ID so we process chunks in ingestion order.
        all.sort_by_key(|c| c.id);

        let min_words = filter.min_token_count.unwrap_or(0);

        // Tier 3 item 4: when the domain declares metadata-based
        // predicates (metadata_in / metadata_compare / legacy
        // metadata_key_values), fetch the metadata sidecar and keep
        // only chunks whose metadata passes. Done here before the
        // title/sampling branches so both downstream paths inherit
        // the narrowing.
        //
        // Legacy domains (no metadata predicates) skip this entirely
        // — `requires_metadata()` returns false, we don't load the
        // heavier column, and the function behaves as before.
        if filter.requires_metadata() {
            let meta_chunks = index.all_chunks_with_raw_metadata().await?;
            let allowed_ids: std::collections::HashSet<u64> = meta_chunks
                .iter()
                .filter_map(|m| {
                    let raw = m.metadata_raw.as_deref()?;
                    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
                    if filter.evaluate_metadata(&parsed) {
                        Some(m.id)
                    } else {
                        None
                    }
                })
                .collect();
            let before = all.len();
            all.retain(|c| allowed_ids.contains(&c.id));
            tracing::info!(
                before,
                after = all.len(),
                "Overview filter: metadata predicates narrowed candidates"
            );
        }

        // Count distinct titles to decide if title-based dedup is viable.
        let distinct_titles = {
            let mut titles: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for chunk in &all {
                titles.insert(chunk.title.as_deref().unwrap_or(""));
            }
            titles.len()
        };

        tracing::info!(
            total_chunks = total,
            distinct_titles = distinct_titles,
            "Overview chunk title analysis"
        );

        // Count how many chunks have non-empty titles.
        let titled_count = all
            .iter()
            .filter(|c| c.title.as_ref().is_some_and(|t| !t.is_empty()))
            .count();

        let filtered: Vec<_> = if filter.is_first_in_entry == Some(true) && titled_count > 10 {
            // Titles are available — keep the first chunk per distinct title.
            tracing::info!(titled_count, "Using title-based overview selection");
            let mut seen_titles: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            all.into_iter()
                .filter(|chunk| {
                    let word_count = chunk.content.split_whitespace().count();
                    if word_count < min_words {
                        return false;
                    }
                    let title_key = chunk.title.as_deref().unwrap_or("").to_lowercase();
                    if title_key.is_empty() {
                        return false;
                    }
                    seen_titles.insert(title_key)
                })
                .collect()
        } else {
            // No usable titles — take the lowest-ID chunk per source_doc_id
            // window. Since chunks are sorted by ID, we sample one per N
            // to approximate "first chunk of each article".
            // Target ~1,800 overview samples for SEP-scale corpora.
            let target_overviews = 1800usize;
            let sample_rate = (total / target_overviews).max(1);
            eprintln!(
                "[enrichment] No chunk titles available — sampling every {}th chunk ({} of {} as overview candidates)",
                sample_rate,
                total / sample_rate,
                total,
            );
            all.into_iter()
                .enumerate()
                .filter(|(i, chunk)| {
                    if i % sample_rate != 0 {
                        return false;
                    }
                    let word_count = chunk.content.split_whitespace().count();
                    word_count >= min_words
                })
                .map(|(_, chunk)| chunk)
                .collect()
        };

        tracing::info!(
            total_chunks = total,
            overview_chunks = filtered.len(),
            "Overview chunk selection complete"
        );

        Ok(filtered)
    }

    async fn extract_skeleton_phase(
        &self,
        overview_chunks: &[crate::index::StoredChunk],
        index: &CorpusIndex,
        checkpoint: &mut EnrichmentCheckpoint,
        progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
    ) -> Result<PartialSkeleton> {
        use futures::stream::{FuturesUnordered, StreamExt};
        use std::pin::Pin;

        const CONCURRENCY: usize = 4;

        type InferenceFuture =
            Pin<Box<dyn futures::Future<Output = (usize, crate::error::Result<String>)> + Send>>;

        // Resume from partial skeleton if we have checkpoint progress.
        let resume_from = checkpoint.phase_1_batches_done;
        let mut skeleton = if resume_from > 0 {
            // Load the partial skeleton that was flushed to disk.
            let existing = index.load_field_skeleton()?;
            let loaded = existing
                .map(|fs| {
                    let mut ps = PartialSkeleton::new(self.domain.id());
                    for q in fs.canonical_questions {
                        ps.questions.push(SkeletonQuestion {
                            id: q.id,
                            question: q.question,
                            question_type: q.question_type,
                            status: q.status,
                            primary_article_ids: q.primary_entries,
                            positions: q.positions,
                        });
                    }
                    ps
                })
                .unwrap_or_else(|| PartialSkeleton::new(self.domain.id()));
            tracing::info!(
                resume_from_batch = resume_from,
                existing_questions = loaded.questions.len(),
                "Resuming skeleton extraction from batch {resume_from}"
            );
            loaded
        } else {
            PartialSkeleton::new(self.domain.id())
        };

        let batches: Vec<_> = overview_chunks.chunks(4).collect();
        let total_batches = batches.len();
        let mut questions_found: usize = 0;
        let mut positions_found: usize = 0;
        let mut inference_errors: usize = 0;
        let mut parse_errors: usize = 0;
        let mut batches_done: usize = resume_from;

        let failures_path = index.path().join("_skeleton_failures.ndjson");

        // Build prompts only for batches we haven't processed yet.
        let prompts: Vec<(usize, String)> = batches
            .iter()
            .enumerate()
            .skip(resume_from)
            .map(|(i, batch)| {
                let refs: Vec<&crate::index::StoredChunk> = batch.iter().collect();
                (i, self.domain.skeleton_extraction_prompt(&refs))
            })
            .collect();

        let spawn_inference =
            |inference: InferenceFn, batch_idx: usize, prompt: String| -> InferenceFuture {
                Box::pin(async move {
                    // Skeleton extraction is currently free-form (no
                    // schema); Phase 1b is the only path that opts in
                    // via `Domain::entity_extraction_schema`. Pass None
                    // here to keep the rest of the field-engine pipeline
                    // unchanged. Future schema work for skeleton extract
                    // would add an analogous domain hook.
                    let result = (inference)(&prompt, None).await;
                    (batch_idx, result)
                })
            };

        // Process in concurrent windows.
        let mut prompt_iter = prompts.into_iter();
        let mut in_flight: FuturesUnordered<InferenceFuture> = FuturesUnordered::new();

        // Seed the initial window.
        for _ in 0..CONCURRENCY {
            if let Some((batch_idx, prompt)) = prompt_iter.next() {
                in_flight.push(spawn_inference(self.inference.clone(), batch_idx, prompt));
            }
        }

        // Process results as they arrive, refilling the window.
        while let Some((batch_idx, result)) = in_flight.next().await {
            // Refill: start the next batch immediately.
            if let Some((next_idx, next_prompt)) = prompt_iter.next() {
                in_flight.push(spawn_inference(
                    self.inference.clone(),
                    next_idx,
                    next_prompt,
                ));
            }

            batches_done += 1;

            let response = match result {
                Ok(r) => r,
                Err(e) => {
                    inference_errors += 1;
                    tracing::warn!(batch = batch_idx, error = %e, "Skeleton extraction inference failed");
                    continue;
                }
            };

            // Parse response.
            let extracted = parse_skeleton_response(batch_idx, &response, &failures_path);
            match extracted {
                ParseResult::Ok(passages) => {
                    for passage in passages {
                        positions_found += passage.positions.len();
                        questions_found += 1;
                        skeleton.questions.push(passage);
                    }
                }
                ParseResult::Repaired(passages, salvaged) => {
                    tracing::info!(
                        batch = batch_idx,
                        "Repaired truncated JSON — salvaged {salvaged} passages"
                    );
                    for passage in passages {
                        positions_found += passage.positions.len();
                        questions_found += 1;
                        skeleton.questions.push(passage);
                    }
                }
                ParseResult::Failed => {
                    parse_errors += 1;
                }
            }

            // Progress every 10 batches or at the end.
            if batches_done.is_multiple_of(10) || batches_done == total_batches {
                progress(EnrichmentProgress::Phase1Progress {
                    batches_done,
                    batches_total: total_batches,
                });
            }

            // Flush skeleton + checkpoint every 50 batches for resume support.
            if batches_done.is_multiple_of(50) {
                if let Err(e) = self.write_partial_skeleton(index, &skeleton) {
                    tracing::warn!(error = %e, "Failed to flush partial skeleton");
                }
                checkpoint.phase_1_batches_done = batches_done;
                checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
                let _ = checkpoint.save(&index.path());
            }
        }

        // Final flush.
        self.write_partial_skeleton(index, &skeleton)?;

        tracing::info!(
            questions = questions_found,
            positions = positions_found,
            inference_errors = inference_errors,
            parse_errors = parse_errors,
            "Skeleton extraction complete"
        );

        deduplicate_questions(&mut skeleton);

        Ok(skeleton)
    }

    fn write_partial_skeleton(
        &self,
        index: &CorpusIndex,
        skeleton: &PartialSkeleton,
    ) -> Result<()> {
        let field_skeleton = FieldSkeleton {
            schema_version: 1,
            corpus_id: index.corpus_id().to_string(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            extraction_method: "dual_pass_v1".into(),
            prompt_version: "1.0.0".into(),
            domain_id: self.domain.id().to_string(),
            canonical_questions: skeleton
                .questions
                .iter()
                .map(|q| CanonicalQuestion {
                    id: q.id.clone(),
                    question: q.question.clone(),
                    status: q.status.clone(),
                    question_type: q.question_type.clone(),
                    primary_entries: q.primary_article_ids.clone(),
                    positions: q.positions.clone(),
                    fault_lines: Vec::new(),
                })
                .collect(),
            open_questions: Vec::new(),
            field_stats: FieldModelStats::default(),
        };
        index.write_field_skeleton(&field_skeleton)
    }

    async fn label_clusters_phase(
        &self,
        index: &CorpusIndex,
        mut clusters: ClusterResult,
        progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
    ) -> Result<ClusterResult> {
        let total = clusters.clusters.len();
        // Stuck detection. The pre-2026-05-20 loop logged each
        // inference failure at `warn!` and continued; one persistently
        // failing inference path (e.g. the MTP-quarantine bug fixed
        // in embedded.rs) would silently retry every cluster, every
        // 2-3s, indefinitely, with no surfacing to the operator.
        // Track consecutive failures and bail when they exceed the
        // threshold. Per-cluster progress is emitted on every
        // iteration so the daemon's HTTP progress endpoint can
        // describe the stall to a watching operator.
        const STUCK_THRESHOLD: usize = 16;
        let mut clusters_failed = 0usize;
        let mut consecutive_failures = 0usize;
        let mut last_error: Option<String> = None;

        for (idx, cluster) in clusters.clusters.iter_mut().enumerate() {
            let chunks = index.get_chunks(&cluster.central_chunks).await?;
            let refs: Vec<&crate::index::StoredChunk> = chunks.iter().collect();
            let prompt = self.domain.cluster_labeling_prompt(&refs);

            match (self.inference)(&prompt, None).await {
                Ok(response) => {
                    let json_str = extract_json_from_response(&response);
                    match serde_json::from_str(json_str) {
                        Ok(label) => {
                            cluster.label = Some(label);
                            consecutive_failures = 0;
                            last_error = None;
                        }
                        Err(e) => {
                            tracing::warn!(cluster = cluster.id, error = %e, "Cluster label parse failed");
                            clusters_failed += 1;
                            consecutive_failures += 1;
                            last_error = Some(format!("parse: {e}"));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(cluster = cluster.id, error = %e, "Cluster label inference failed");
                    clusters_failed += 1;
                    consecutive_failures += 1;
                    last_error = Some(e.to_string());
                }
            }

            progress(EnrichmentProgress::Phase2bProgress {
                clusters_done: idx + 1,
                clusters_total: total,
                clusters_failed,
                consecutive_failures,
                last_error: last_error.clone(),
            });

            if consecutive_failures >= STUCK_THRESHOLD {
                let err_msg = last_error.clone().unwrap_or_else(|| "<no error>".into());
                tracing::error!(
                    cluster = cluster.id,
                    consecutive_failures,
                    threshold = STUCK_THRESHOLD,
                    last_error = %err_msg,
                    "label_clusters_phase: bailing — {STUCK_THRESHOLD} consecutive failures. \
                     Likely upstream inference issue (e.g. MTP-quarantine on the chat slot). \
                     Restart the daemon to reset slot state, then resume enrichment."
                );
                return Err(Error::Embed(format!(
                    "label_clusters_phase stuck: {consecutive_failures} consecutive inference \
                     failures (last: {err_msg}). Resolve the upstream inference issue \
                     (e.g. restart the daemon to clear MTP quarantine) and resume enrichment."
                )));
            }
        }

        // Write chunk_role to chunks.lance based on cluster labels.
        let mut role_assignments: std::collections::HashMap<u64, &str> =
            std::collections::HashMap::new();
        for cluster in &clusters.clusters {
            if let Some(label) = &cluster.label {
                let role = self.domain.classify_chunk_role(label);
                let role_str = role.as_str();
                for (&chunk_id, &cluster_id) in &clusters.assignments {
                    if cluster_id == cluster.id {
                        role_assignments.insert(chunk_id, role_str);
                    }
                }
            }
        }

        index
            .bulk_update_str_column("chunk_role", &role_assignments)
            .await?;

        progress(EnrichmentProgress::Phase2bComplete {
            labeled_count: total,
        });
        Ok(clusters)
    }

    fn write_json_skeleton(
        &self,
        index: &CorpusIndex,
        skeleton: &PartialSkeleton,
        stats: &FieldModelStats,
        open_questions: &[OpenQuestion],
    ) -> Result<()> {
        let field_skeleton = FieldSkeleton {
            schema_version: 1,
            corpus_id: index.corpus_id().to_string(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            extraction_method: "dual_pass_v1".into(),
            prompt_version: "1.0.0".into(),
            domain_id: self.domain.id().to_string(),
            canonical_questions: skeleton
                .questions
                .iter()
                .map(|q| CanonicalQuestion {
                    id: q.id.clone(),
                    question: q.question.clone(),
                    status: q.status.clone(),
                    question_type: q.question_type.clone(),
                    primary_entries: q.primary_article_ids.clone(),
                    positions: q.positions.clone(),
                    fault_lines: Vec::new(),
                })
                .collect(),
            open_questions: open_questions
                .iter()
                .map(open_question_to_skeleton)
                .collect(),
            field_stats: stats.clone(),
        };

        index.write_field_skeleton(&field_skeleton)?;
        Ok(())
    }
}

/// Map a detected [`OpenQuestion`] onto the persisted [`SkeletonOpenQuestion`]
/// shape (I3). `domain_id` is dropped — the skeleton already records the
/// domain once at the top level — and `question_type` is carried across for
/// fidelity (`String` → `Option<String>`, always `Some` here since the
/// detector always populates it).
fn open_question_to_skeleton(oq: &OpenQuestion) -> SkeletonOpenQuestion {
    SkeletonOpenQuestion {
        id: oq.id.clone(),
        question: oq.question.clone(),
        status: oq.status.clone(),
        question_type: Some(oq.question_type.clone()),
        related_question_id: oq.related_question_id.clone(),
        representative_chunk_ids: oq.representative_chunk_ids.clone(),
    }
}

/// Reprocess `_skeleton_failures.ndjson` with the improved parser and merge
/// any salvaged questions into the existing skeleton.
///
/// Returns `(salvaged_count, still_failed_count)`.
///
/// Usage from any frontend:
/// ```ignore
/// let (salvaged, failed) = reprocess_skeleton_failures(index)?;
/// println!("Recovered {salvaged} questions, {failed} still unrecoverable");
/// ```
pub fn reprocess_skeleton_failures(index: &CorpusIndex) -> Result<(usize, usize)> {
    let failures_path = index.path().join("_skeleton_failures.ndjson");
    if !failures_path.exists() {
        return Ok((0, 0));
    }

    let contents = std::fs::read_to_string(&failures_path)?;

    let mut salvaged_questions: Vec<SkeletonQuestion> = Vec::new();
    let mut still_failed = 0_usize;
    let mut still_failed_entries: Vec<String> = Vec::new();

    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                still_failed += 1;
                still_failed_entries.push(line.to_string());
                continue;
            }
        };

        let batch_idx = entry["batch"].as_u64().unwrap_or(0) as usize;
        let raw = entry["raw_response_truncated"].as_str().unwrap_or("");

        // Run through the improved parse pipeline (with unquoted-string repair).
        let dummy_path = std::path::PathBuf::from("/dev/null");
        match parse_skeleton_response(batch_idx, raw, &dummy_path) {
            ParseResult::Ok(questions) | ParseResult::Repaired(questions, _) => {
                if questions.is_empty() {
                    still_failed += 1;
                    still_failed_entries.push(line.to_string());
                } else {
                    tracing::info!(
                        batch = batch_idx,
                        count = questions.len(),
                        "Reprocessed failure — salvaged questions"
                    );
                    salvaged_questions.extend(questions);
                }
            }
            ParseResult::Failed => {
                still_failed += 1;
                still_failed_entries.push(line.to_string());
            }
        }
    }

    let salvaged_count = salvaged_questions.len();

    if salvaged_count > 0 {
        // Load existing skeleton and merge.
        if let Some(mut existing) = index.load_field_skeleton()? {
            for q in &salvaged_questions {
                // Check for duplicate question IDs before merging.
                if let Some(existing_q) = existing
                    .canonical_questions
                    .iter_mut()
                    .find(|eq| eq.id == q.id)
                {
                    // Merge positions.
                    for pos in &q.positions {
                        if !existing_q.positions.iter().any(|p| p.id == pos.id) {
                            existing_q.positions.push(pos.clone());
                        }
                    }
                } else {
                    existing.canonical_questions.push(
                        crate::enrichment::skeleton::CanonicalQuestion {
                            id: q.id.clone(),
                            question: q.question.clone(),
                            status: q.status.clone(),
                            question_type: q.question_type.clone(),
                            primary_entries: q.primary_article_ids.clone(),
                            positions: q.positions.clone(),
                            fault_lines: Vec::new(),
                        },
                    );
                }
            }
            existing.generated_at = chrono::Utc::now().to_rfc3339();
            index.write_field_skeleton(&existing)?;
            tracing::info!(
                salvaged = salvaged_count,
                total_questions = existing.canonical_questions.len(),
                "Merged reprocessed questions into skeleton"
            );
        }

        // Rewrite the failures file with only the entries that still failed.
        if still_failed_entries.is_empty() {
            let _ = std::fs::remove_file(&failures_path);
        } else {
            std::fs::write(&failures_path, still_failed_entries.join("\n") + "\n")?;
        }
    }

    Ok((salvaged_count, still_failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_question_to_skeleton_maps_fields_and_drops_domain_id() {
        let oq = OpenQuestion {
            id: "oq_0".into(),
            question: "What grounds moral realism?".into(),
            status: "active_research".into(),
            question_type: "conceptual".into(),
            related_question_id: Some("q_ethics".into()),
            representative_chunk_ids: vec![11, 22],
            domain_id: "philosophy".into(),
        };

        let mapped = open_question_to_skeleton(&oq);

        assert_eq!(mapped.id, "oq_0");
        assert_eq!(mapped.question, "What grounds moral realism?");
        assert_eq!(mapped.status, "active_research");
        // question_type carried across (String -> Option<String>).
        assert_eq!(mapped.question_type.as_deref(), Some("conceptual"));
        assert_eq!(mapped.related_question_id.as_deref(), Some("q_ethics"));
        assert_eq!(mapped.representative_chunk_ids, vec![11, 22]);
        // domain_id is deliberately dropped — SkeletonOpenQuestion has no such
        // field; the skeleton records the domain once at the top level.
    }

    /// The runtime layer still rejects an unregistered domain. Reached via
    /// `recipe_with_domain`, which bypasses the loader on purpose — see its
    /// doc comment for why this is not the same test as
    /// `tests/recipe_domain_gate.rs`.
    #[test]
    fn from_recipe_unknown_domain() {
        let recipe = recipe_with_domain("astrology");

        let embed: EmbedFn =
            Arc::new(|_| Box::pin(async { Ok(vec![0.0; crate::DEFAULT_EMBED_DIM]) }));
        let inference: InferenceFn =
            Arc::new(|_, _: Option<&serde_json::Value>| Box::pin(async { Ok(String::new()) }));
        let result = FieldModelEngine::from_recipe(&recipe, embed, inference);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(format!("{err}").contains("astrology"));
    }

    #[test]
    fn from_recipe_default_domain() {
        let recipe = crate::recipe::Recipe::from_toml(
            r#"
[corpus]
id = "test"
name = "Test"

[acquire]
type = "local_file"
path = "/tmp/test"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"

[enrichment]
enabled = true
"#,
        )
        .unwrap();

        let embed: EmbedFn =
            Arc::new(|_| Box::pin(async { Ok(vec![0.0; crate::DEFAULT_EMBED_DIM]) }));
        let inference: InferenceFn =
            Arc::new(|_, _: Option<&serde_json::Value>| Box::pin(async { Ok(String::new()) }));
        let engine = FieldModelEngine::from_recipe(&recipe, embed, inference).unwrap();
        assert_eq!(engine.domain.id(), "philosophy");
    }

    /// Build a recipe carrying `domain`, deliberately BYPASSING the load-time
    /// gate.
    ///
    /// `Recipe::from_toml` now refuses an unregistered field-model domain
    /// (`recipe_parsing::check_enrichment_domain`), so these tests can no
    /// longer express their input as TOML — and should not. They exercise the
    /// *second* layer: the runtime check in [`FieldModelEngine::from_recipe`],
    /// which still has to hold for any `Recipe` that reaches the engine without
    /// passing the loader (assembled in memory, arriving over the mesh, or via
    /// a future load path that forgets the gate). Load with a registered
    /// domain, then overwrite the field.
    fn recipe_with_domain(domain: &str) -> crate::recipe::Recipe {
        let mut recipe = crate::recipe::Recipe::from_toml(
            r#"
[corpus]
id = "test"
name = "Test"

[acquire]
type = "local_file"
path = "/tmp/test"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"

[enrichment]
enabled = true
domain = "philosophy"
"#,
        )
        .expect("the registered-domain baseline must load");
        recipe
            .enrichment
            .as_mut()
            .expect("baseline has an [enrichment] block")
            .domain = Some(domain.to_string());
        recipe
    }

    fn test_embed_infer() -> (EmbedFn, InferenceFn) {
        let embed: EmbedFn =
            Arc::new(|_| Box::pin(async { Ok(vec![0.0; crate::DEFAULT_EMBED_DIM]) }));
        let inference: InferenceFn =
            Arc::new(|_, _: Option<&serde_json::Value>| Box::pin(async { Ok(String::new()) }));
        (embed, inference)
    }

    #[test]
    fn from_recipe_all_known_domains() {
        // Only fully-implemented, registered domains. Stub domains were deleted
        // (2026-07-13); their construction is covered by the rejection test below.
        let domains = [
            "philosophy",
            "personal",
            "conversational",
            "business_email",
            "institutional",
        ];
        for domain in &domains {
            let recipe = recipe_with_domain(domain);
            let (embed, inference) = test_embed_infer();
            let engine = FieldModelEngine::from_recipe(&recipe, embed, inference);
            assert!(
                engine.is_ok(),
                "from_recipe should succeed for domain '{domain}'"
            );
            assert_eq!(engine.unwrap().domain.id(), *domain);
        }
    }

    #[test]
    fn from_recipe_rejects_deleted_stub_domains() {
        // The deleted stubs must fail at selection with a clean error — never a
        // `todo!()` panic mid-enrichment. This is the whole point of removing them.
        for domain in [
            "science",
            "policy",
            "legal",
            "community",
            "multi",
            "engineering",
        ] {
            let recipe = recipe_with_domain(domain);
            let (embed, inference) = test_embed_infer();
            let engine = FieldModelEngine::from_recipe(&recipe, embed, inference);
            match engine {
                Err(crate::error::Error::UnknownEnrichmentDomain(d)) => {
                    assert_eq!(d, domain, "wrong domain in error");
                }
                other => panic!(
                    "domain '{domain}' should be rejected as unknown, got {:?}",
                    other.map(|_| "Ok").map_err(|e| e.to_string())
                ),
            }
        }
    }
}
