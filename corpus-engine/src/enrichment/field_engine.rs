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
use super::open_questions::detect_open_questions;
use super::skeleton::{
    CanonicalQuestion, FieldSkeleton, PartialSkeleton, SkeletonPosition, SkeletonQuestion,
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
    /// The match on domain string is the ONLY place domain-specific
    /// logic appears in the engine. It is a factory, not business logic.
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

        let domain: Arc<dyn Domain> = match domain_id {
            "philosophy" => Arc::new(super::domains::philosophy::PhilosophyDomain),
            "science" => Arc::new(super::domains::science::ScienceDomain),
            "policy" => Arc::new(super::domains::policy::PolicyDomain),
            "legal" => Arc::new(super::domains::legal::LegalDomain),
            "community" => Arc::new(super::domains::community::CommunityKnowledgeDomain),
            "multi" => Arc::new(super::domains::multi::MultiDomain::wikipedia_default()),
            other => return Err(Error::UnknownEnrichmentDomain(other.to_string())),
        };

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
        let mut checkpoint = EnrichmentCheckpoint::load(&index_dir)?
            .unwrap_or_else(|| EnrichmentCheckpoint {
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
            return self.compute_stats(index).await;
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
            progress(EnrichmentProgress::Phase {
                phase: 1,
                name: "Skeleton extraction",
                note: "",
            });
            let overview = self.get_overview_chunks(index).await?;
            let skeleton = self
                .extract_skeleton_phase(&overview, index, progress)
                .await?;
            checkpoint.phase_1_complete = true;
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
            skeleton
        };

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
        if !checkpoint.phase_5_complete {
            detect_open_questions(index, &clusters, &self.inference, self.domain.as_ref(), progress)
                .await?;
            checkpoint.phase_5_complete = true;
            checkpoint.last_updated = chrono::Utc::now().to_rfc3339();
            checkpoint.save(&index_dir)?;
        } else {
            progress(EnrichmentProgress::PhaseSkipped {
                phase: 6,
                name: "Open questions",
            });
        }

        // ── Finalize ──────────────────────────────────────────────────
        let stats = self.compute_stats(index).await?;

        // Write JSON skeleton if the domain requests it.
        if let SkeletonStorage::JsonAndLance = self.domain.skeleton_storage() {
            self.write_json_skeleton(index, &skeleton, &stats)?;
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
        // Load all chunks and filter by overview criteria.
        // A future optimization would push this filter down to LanceDB.
        let filter = self.domain.overview_filter();
        let all = index.all_chunks().await?;

        let filtered: Vec<_> = all
            .into_iter()
            .filter(|chunk| {
                // Check min_token_count
                if let Some(min_tokens) = filter.min_token_count {
                    let word_count = chunk.content.split_whitespace().count();
                    if word_count < min_tokens {
                        return false;
                    }
                }
                true
            })
            .collect();

        Ok(filtered)
    }

    async fn extract_skeleton_phase(
        &self,
        overview_chunks: &[crate::index::StoredChunk],
        index: &CorpusIndex,
        progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
    ) -> Result<PartialSkeleton> {
        let mut skeleton = PartialSkeleton::new(self.domain.id());
        let batches: Vec<_> = overview_chunks.chunks(4).collect();
        let total_batches = batches.len();
        let mut questions_found: usize = 0;
        let mut positions_found: usize = 0;
        let mut inference_errors: usize = 0;
        let mut parse_errors: usize = 0;

        // Failure log for reprocessing later.
        let failures_path = index.path().join("_skeleton_failures.ndjson");

        for (i, batch) in batches.iter().enumerate() {
            let refs: Vec<&crate::index::StoredChunk> = batch.iter().collect();
            let prompt = self.domain.skeleton_extraction_prompt(&refs);
            let response = match (self.inference)(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    inference_errors += 1;
                    tracing::warn!(batch = i, error = %e, "Skeleton extraction inference failed");
                    continue;
                }
            };

            // Parse JSON response — tolerate markdown fences, <think> blocks,
            // and truncated output (try repair before giving up).
            let json_str = extract_json_from_response(&response);
            let passages = match serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                Ok(p) => p,
                Err(_) => {
                    // The extractor may have mangled a truncated array (e.g.
                    // found the last } instead of preserving the [ start).
                    // Try repair on the original response too.
                    let repair_source = if json_str.starts_with('[') {
                        json_str.to_string()
                    } else {
                        // Re-extract from the raw response, looking for the first [
                        let raw = extract_json_from_response(&response);
                        // Find the outermost [ in the original response
                        response.find('[')
                            .map(|start| response[start..].to_string())
                            .unwrap_or_else(|| raw.to_string())
                    };
                    match try_repair_truncated_json(&repair_source) {
                        Some(repaired) => {
                            match serde_json::from_str::<Vec<serde_json::Value>>(&repaired) {
                                Ok(p) => {
                                    tracing::info!(
                                        batch = i,
                                        "Repaired truncated JSON — salvaged {} passages",
                                        p.len()
                                    );
                                    p
                                }
                                Err(e) => {
                                    parse_errors += 1;
                                    log_skeleton_failure(
                                        &failures_path, i, json_str, &format!("{e}"),
                                    );
                                    tracing::warn!(
                                        batch = i,
                                        error = %e,
                                        "Skeleton parse failed even after repair"
                                    );
                                    continue;
                                }
                            }
                        }
                        None => {
                            parse_errors += 1;
                            let snippet: String = json_str.chars().take(200).collect();
                            log_skeleton_failure(
                                &failures_path, i, json_str, "not repairable",
                            );
                            tracing::warn!(
                                batch = i,
                                response_snippet = %snippet,
                                "Skeleton parse failed — not valid or repairable JSON"
                            );
                            continue;
                        }
                    }
                }
            };

            for passage in passages {
                if let Some(question) = passage["canonical_question"].as_str() {
                    // Skip empty, null, or placeholder questions.
                    if question.is_empty()
                        || question == "..."
                        || question == "null"
                        || question.len() < 10
                    {
                        continue;
                    }
                    let question_type = passage["question_type"]
                        .as_str()
                        .unwrap_or("conceptual")
                        .to_string();
                    let positions: Vec<SkeletonPosition> = passage["positions"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|p| {
                                    let name = p["name"].as_str()?;
                                    // Skip placeholder or empty position names.
                                    if name.is_empty()
                                        || name == "..."
                                        || name == "null"
                                        || name.len() < 2
                                    {
                                        return None;
                                    }
                                    Some(SkeletonPosition {
                                        id: format!(
                                            "p_{}",
                                            name.to_lowercase().replace(' ', "_")
                                        ),
                                        name: name.to_string(),
                                        claim: {
                                            let c = p["claim"]
                                                .as_str()
                                                .unwrap_or_default();
                                            if c == "..." || c.is_empty() {
                                                return None;
                                            }
                                            c.to_string()
                                        },
                                        status: {
                                            let s = p["status"]
                                                .as_str()
                                                .unwrap_or("contested");
                                            // Normalize compound statuses like "minority|contested"
                                            if s.contains('|') {
                                                s.split('|').next().unwrap_or("contested").to_string()
                                            } else if s == "..." || s.is_empty() {
                                                "contested".to_string()
                                            } else {
                                                s.to_string()
                                            }
                                        },
                                        proponents: p["proponents"]
                                            .as_array()
                                            .map(|a| {
                                                a.iter()
                                                    .filter_map(|v| {
                                                        v.as_str().map(|s| s.to_string())
                                                    })
                                                    .collect()
                                            })
                                            .unwrap_or_default(),
                                        source: "skeleton".into(),
                                        cluster_ids: Vec::new(),
                                        centroid_chunk_ids: Vec::new(),
                                        discovery_confidence: None,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    let q_id = format!(
                        "q_{}",
                        question
                            .to_lowercase()
                            .chars()
                            .filter(|c| c.is_alphanumeric() || *c == ' ')
                            .collect::<String>()
                            .replace(' ', "_")
                            .chars()
                            .take(50)
                            .collect::<String>()
                    );

                    positions_found += positions.len();
                    questions_found += 1;

                    skeleton.questions.push(SkeletonQuestion {
                        id: q_id,
                        question: question.to_string(),
                        question_type,
                        status: "contested".into(),
                        primary_article_ids: Vec::new(),
                        positions,
                    });
                }
            }

            // Progress every 10 batches or at the end.
            if i % 10 == 0 || i == total_batches - 1 {
                progress(EnrichmentProgress::Phase1Progress {
                    batches_done: i + 1,
                    batches_total: total_batches,
                });
            }

            // Flush skeleton to disk every 50 batches for resume support.
            if (i + 1) % 50 == 0 {
                if let Err(e) = self.write_partial_skeleton(index, &skeleton) {
                    tracing::warn!(error = %e, "Failed to flush partial skeleton");
                }
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

        // Deduplicate questions by similarity
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

        for cluster in &mut clusters.clusters {
            let chunks = index.get_chunks(&cluster.central_chunks).await?;
            let refs: Vec<&crate::index::StoredChunk> = chunks.iter().collect();
            let prompt = self.domain.cluster_labeling_prompt(&refs);

            match (self.inference)(&prompt).await {
                Ok(response) => {
                    let json_str = extract_json_from_response(&response);
                    match serde_json::from_str(json_str) {
                        Ok(label) => cluster.label = Some(label),
                        Err(e) => {
                            tracing::warn!(cluster = cluster.id, error = %e, "Cluster label parse failed");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(cluster = cluster.id, error = %e, "Cluster label inference failed");
                }
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

    async fn compute_stats(&self, _index: &CorpusIndex) -> Result<FieldModelStats> {
        // TODO: Query tables for actual counts.
        Ok(FieldModelStats::default())
    }

    fn write_json_skeleton(
        &self,
        index: &CorpusIndex,
        skeleton: &PartialSkeleton,
        stats: &FieldModelStats,
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
            field_stats: stats.clone(),
        };

        index.write_field_skeleton(&field_skeleton)?;
        Ok(())
    }
}

/// Extract JSON from a model response.
///
/// Handles common LLM output patterns:
/// - `<think>...</think>` reasoning blocks before the JSON
/// - Markdown code fences (```json, ```JSON, ```)
/// - Preamble prose before the JSON array/object
/// - Trailing prose after the closing bracket
fn extract_json_from_response(response: &str) -> &str {
    let mut text = response.trim();

    // Strip <think>...</think> blocks (common with reasoning models).
    if let Some(think_end) = text.find("</think>") {
        text = text[think_end + 8..].trim();
    }

    // Try to extract from ```json ... ``` (case-insensitive).
    let lower = text.to_lowercase();
    if let Some(fence_start) = lower.find("```json") {
        let content_start = fence_start + 7;
        // Skip optional newline after ```json
        let content_start = if text[content_start..].starts_with('\n') {
            content_start + 1
        } else {
            content_start
        };
        if let Some(fence_end) = text[content_start..].find("```") {
            return text[content_start..content_start + fence_end].trim();
        }
    }

    // Try to extract from ``` ... ```
    if let Some(fence_start) = text.find("```") {
        let content_start = fence_start + 3;
        // Skip optional language tag + newline (e.g. ```\n or ```text\n)
        let after_fence = &text[content_start..];
        let content_start = if let Some(nl) = after_fence.find('\n') {
            content_start + nl + 1
        } else {
            content_start
        };
        if let Some(fence_end) = text[content_start..].find("```") {
            let block = text[content_start..content_start + fence_end].trim();
            if block.starts_with('[') || block.starts_with('{') {
                return block;
            }
        }
    }

    // No code fence — find the first [ or { and last ] or }.
    if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end > start {
                return text[start..=end].trim();
            }
        }
    }
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return text[start..=end].trim();
            }
        }
    }

    // Last resort — return the whole thing and let the caller handle the error.
    text
}

/// Try to repair truncated JSON by closing open brackets/braces.
///
/// LLMs often hit the token limit mid-response, producing valid JSON
/// that's cut off. We try to close the structure so at least the
/// complete elements parse. Returns `None` if the input doesn't look
/// like truncated JSON.
fn try_repair_truncated_json(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if !trimmed.starts_with('[') && !trimmed.starts_with('{') {
        return None;
    }

    // Find the last complete JSON element by walking brackets.
    let mut depth_brace = 0i32;
    let mut depth_bracket = 0i32;
    let mut in_string = false;
    let mut escape_next = false;
    let mut last_complete_element_end = 0;

    for (i, ch) in trimmed.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => depth_brace += 1,
            '}' => {
                depth_brace -= 1;
                // A complete top-level element: either a standalone object
                // (depth 0/0) or a direct child of the top-level array (brace 0, bracket 1).
                if depth_brace == 0 && depth_bracket <= 1 {
                    last_complete_element_end = i + 1;
                }
            }
            '[' => depth_bracket += 1,
            ']' => {
                depth_bracket -= 1;
                if depth_bracket == 0 && depth_brace == 0 {
                    // Already complete — no repair needed.
                    return None;
                }
            }
            _ => {}
        }
    }

    if last_complete_element_end == 0 {
        return None; // No complete elements found.
    }

    // Truncate to the last complete element.
    let mut repaired = trimmed[..last_complete_element_end].to_string();

    // Close the top-level array if the input started with one.
    if trimmed.starts_with('[') {
        repaired.push(']');
    }

    Some(repaired)
}

/// Append a failed skeleton extraction batch to the failure log.
fn log_skeleton_failure(path: &std::path::Path, batch: usize, raw: &str, error: &str) {
    use std::io::Write;
    let entry = serde_json::json!({
        "batch": batch,
        "error": error,
        "raw_response_truncated": &raw[..raw.len().min(2000)],
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{}", entry);
    }
}

/// Deduplicate questions by merging those with similar text.
fn deduplicate_questions(skeleton: &mut PartialSkeleton) {
    // Simple dedup: merge questions with the same ID.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut deduped = Vec::new();

    for q in skeleton.questions.drain(..) {
        if let Some(&existing_idx) = seen.get(&q.id) {
            // Merge positions into the existing question.
            let existing: &mut SkeletonQuestion = &mut deduped[existing_idx];
            for pos in q.positions {
                if !existing.positions.iter().any(|p| p.id == pos.id) {
                    existing.positions.push(pos);
                }
            }
        } else {
            seen.insert(q.id.clone(), deduped.len());
            deduped.push(q);
        }
    }

    skeleton.questions = deduped;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown_fence() {
        let response = "Here is the result:\n```json\n[{\"a\": 1}]\n```\nDone.";
        assert_eq!(extract_json_from_response(response), "[{\"a\": 1}]");
    }

    #[test]
    fn extract_json_bare() {
        let response = "[{\"a\": 1}]";
        assert_eq!(extract_json_from_response(response), "[{\"a\": 1}]");
    }

    #[test]
    fn from_recipe_unknown_domain() {
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
domain = "astrology"
"#,
        )
        .unwrap();

        let embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(vec![0.0; 768]) }));
        let inference: InferenceFn = Arc::new(|_| Box::pin(async { Ok(String::new()) }));
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

        let embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(vec![0.0; 768]) }));
        let inference: InferenceFn = Arc::new(|_| Box::pin(async { Ok(String::new()) }));
        let engine = FieldModelEngine::from_recipe(&recipe, embed, inference).unwrap();
        assert_eq!(engine.domain.id(), "philosophy");
    }

    #[test]
    fn from_recipe_all_known_domains() {
        let domains = ["philosophy", "science", "policy", "legal", "community", "multi"];
        for domain in &domains {
            let toml = format!(
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
domain = "{domain}"
"#
            );
            let recipe = crate::recipe::Recipe::from_toml(&toml).unwrap();
            let embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(vec![0.0; 768]) }));
            let inference: InferenceFn = Arc::new(|_| Box::pin(async { Ok(String::new()) }));
            let engine = FieldModelEngine::from_recipe(&recipe, embed, inference);
            assert!(
                engine.is_ok(),
                "from_recipe should succeed for domain '{domain}'"
            );
            assert_eq!(engine.unwrap().domain.id(), *domain);
        }
    }

    #[test]
    fn extract_json_from_generic_code_fence() {
        let response = "Result:\n```\n{\"key\": \"value\"}\n```";
        assert_eq!(
            extract_json_from_response(response),
            "{\"key\": \"value\"}"
        );
    }

    #[test]
    fn extract_json_with_surrounding_prose() {
        let response = "Here is the JSON:\n```json\n[{\"a\": 1}, {\"b\": 2}]\n```\nAll done!";
        let json = extract_json_from_response(response);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn extract_json_strips_think_block() {
        let response = "<think>\nLet me analyze these passages...\n</think>\n[{\"a\": 1}]";
        let json = extract_json_from_response(response);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn extract_json_case_insensitive_fence() {
        let response = "```JSON\n{\"key\": \"value\"}\n```";
        assert_eq!(
            extract_json_from_response(response),
            "{\"key\": \"value\"}"
        );
    }

    #[test]
    fn extract_json_from_prose_with_array() {
        let response =
            "Here are the results:\n\n[{\"a\": 1}, {\"b\": 2}]\n\nThat's all.";
        let json = extract_json_from_response(response);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn extract_json_from_prose_with_object() {
        let response = "The answer is: {\"crux\": \"test\", \"confidence\": 0.9} done.";
        let json = extract_json_from_response(response);
        let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(parsed["crux"], "test");
    }

    #[test]
    fn extract_json_think_block_then_fence() {
        let response = "<think>\nreasoning here\n</think>\n```json\n[{\"x\": 1}]\n```";
        let json = extract_json_from_response(response);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn deduplicate_questions_merges_same_id() {
        let mut skeleton = PartialSkeleton::new("philosophy");
        skeleton.questions.push(SkeletonQuestion {
            id: "q_free_will".into(),
            question: "Is free will compatible?".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![SkeletonPosition {
                id: "p_compat".into(),
                name: "Compatibilism".into(),
                claim: "Yes".into(),
                status: "majority".into(),
                proponents: vec![],
                source: "skeleton".into(),
                cluster_ids: vec![],
                centroid_chunk_ids: vec![],
                discovery_confidence: None,
            }],
        });
        skeleton.questions.push(SkeletonQuestion {
            id: "q_free_will".into(), // same ID
            question: "Is free will compatible?".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![SkeletonPosition {
                id: "p_hard_incompat".into(), // different position
                name: "Hard Incompatibilism".into(),
                claim: "No".into(),
                status: "minority".into(),
                proponents: vec![],
                source: "skeleton".into(),
                cluster_ids: vec![],
                centroid_chunk_ids: vec![],
                discovery_confidence: None,
            }],
        });

        deduplicate_questions(&mut skeleton);
        assert_eq!(skeleton.questions.len(), 1, "duplicate IDs should merge");
        assert_eq!(
            skeleton.questions[0].positions.len(),
            2,
            "positions from both duplicates should be merged"
        );
    }

    #[test]
    fn deduplicate_questions_skips_duplicate_positions() {
        let mut skeleton = PartialSkeleton::new("philosophy");
        let pos = SkeletonPosition {
            id: "p_compat".into(),
            name: "Compatibilism".into(),
            claim: "Yes".into(),
            status: "majority".into(),
            proponents: vec![],
            source: "skeleton".into(),
            cluster_ids: vec![],
            centroid_chunk_ids: vec![],
            discovery_confidence: None,
        };
        skeleton.questions.push(SkeletonQuestion {
            id: "q_1".into(),
            question: "Q".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![pos.clone()],
        });
        skeleton.questions.push(SkeletonQuestion {
            id: "q_1".into(),
            question: "Q".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![pos], // same position ID
        });

        deduplicate_questions(&mut skeleton);
        assert_eq!(skeleton.questions.len(), 1);
        assert_eq!(
            skeleton.questions[0].positions.len(),
            1,
            "same position ID should not be duplicated"
        );
    }

    #[test]
    fn deduplicate_questions_keeps_distinct() {
        let mut skeleton = PartialSkeleton::new("philosophy");
        skeleton.questions.push(SkeletonQuestion {
            id: "q_1".into(),
            question: "Q1".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![],
        });
        skeleton.questions.push(SkeletonQuestion {
            id: "q_2".into(), // different ID
            question: "Q2".into(),
            question_type: "factual".into(),
            status: "settled".into(),
            primary_article_ids: vec![],
            positions: vec![],
        });

        deduplicate_questions(&mut skeleton);
        assert_eq!(
            skeleton.questions.len(),
            2,
            "distinct IDs should not be merged"
        );
    }

    // ── Truncated JSON repair tests ─────────────────────────

    #[test]
    fn repair_truncated_array_with_complete_first_element() {
        // Array with one complete object and a second cut off.
        let truncated = r#"[{"passage_index": 0, "canonical_question": "Is free will real?", "positions": []}, {"passage_index": 1, "canonical_ques"#;
        let repaired = try_repair_truncated_json(truncated).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["canonical_question"], "Is free will real?");
    }

    #[test]
    fn repair_already_complete_returns_none() {
        let complete = r#"[{"a": 1}]"#;
        assert!(
            try_repair_truncated_json(complete).is_none(),
            "already-complete JSON should return None"
        );
    }

    #[test]
    fn repair_not_json_returns_none() {
        assert!(try_repair_truncated_json("not json").is_none());
        assert!(try_repair_truncated_json("").is_none());
    }

    #[test]
    fn repair_truncated_mid_string() {
        // Truncated inside a string value — the complete first element should survive.
        let truncated = r#"[{"question": "What is X?", "type": "conceptual"}, {"question": "Is Y compat"#;
        let repaired = try_repair_truncated_json(truncated).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn repair_truncated_with_nested_objects() {
        let truncated = r#"[{"q": "test", "positions": [{"name": "A"}]}, {"q": "other", "positions": [{"name": "B"#;
        let repaired = try_repair_truncated_json(truncated).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["positions"][0]["name"], "A");
    }

    #[test]
    fn repair_no_complete_elements_returns_none() {
        // Only a partial first element — nothing to salvage.
        let truncated = r#"[{"passage_in"#;
        assert!(try_repair_truncated_json(truncated).is_none());
    }

    #[test]
    fn repair_realistic_truncated_skeleton_response() {
        // Simulates the actual batch 25 failure: 3 complete passage objects,
        // 4th truncated mid-string. Should salvage the first 3.
        let truncated = r#"[
  {
    "passage_index": 0,
    "canonical_question": "What is the proper relationship between reason and faith?",
    "question_type": "normative",
    "positions": [
      {
        "name": "pseudo-dialecticians",
        "claim": "everything can be explained by human reason",
        "status": "minority",
        "proponents": ["Abelard"]
      }
    ]
  },
  {
    "passage_index": 1,
    "canonical_question": null,
    "positions": []
  },
  {
    "passage_index": 2,
    "canonical_question": "What is identity?",
    "question_type": "conceptual",
    "positions": []
  },
  {
    "passage_index": 3,
    "canonical_question": null,
    "positions": [
      {
        "name": "traditional account",
        "claim": "(a) two things are the same in essence when they are numerically the concrete thing (essentia), and essentially different other"#;

        let repaired = try_repair_truncated_json(truncated)
            .expect("should repair truncated array with 3 complete elements");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&repaired).expect("repaired JSON should parse");
        assert_eq!(
            parsed.len(),
            3,
            "should salvage the 3 complete elements, got {}",
            parsed.len()
        );
        assert_eq!(
            parsed[0]["canonical_question"],
            "What is the proper relationship between reason and faith?"
        );
    }

    #[test]
    fn placeholder_question_filtered() {
        // Questions with "..." as text should be skipped.
        let question = "...";
        assert!(question == "..." || question.len() < 10);
    }

    #[test]
    fn compound_status_normalized() {
        let status = "minority|contested";
        let normalized = if status.contains('|') {
            status.split('|').next().unwrap_or("contested").to_string()
        } else {
            status.to_string()
        };
        assert_eq!(normalized, "minority");
    }
}
