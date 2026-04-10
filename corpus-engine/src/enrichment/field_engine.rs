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
        _index: &CorpusIndex,
        progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
    ) -> Result<PartialSkeleton> {
        let mut skeleton = PartialSkeleton::new(self.domain.id());
        let batches: Vec<_> = overview_chunks.chunks(4).collect();
        let total_batches = batches.len();

        for (i, batch) in batches.iter().enumerate() {
            let refs: Vec<&crate::index::StoredChunk> = batch.iter().collect();
            let prompt = self.domain.skeleton_extraction_prompt(&refs);
            let response = match (self.inference)(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(batch = i, error = %e, "Skeleton extraction inference failed");
                    continue;
                }
            };

            // Parse JSON response — tolerate markdown code fences
            let json_str = extract_json_from_response(&response);
            match serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                Ok(passages) => {
                    for passage in passages {
                        if let Some(question) = passage["canonical_question"].as_str() {
                            if question.is_empty() {
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
                                            Some(SkeletonPosition {
                                                id: format!(
                                                    "p_{}",
                                                    p["name"]
                                                        .as_str()?
                                                        .to_lowercase()
                                                        .replace(' ', "_")
                                                ),
                                                name: p["name"].as_str()?.to_string(),
                                                claim: p["claim"]
                                                    .as_str()
                                                    .unwrap_or_default()
                                                    .to_string(),
                                                status: p["status"]
                                                    .as_str()
                                                    .unwrap_or("contested")
                                                    .to_string(),
                                                proponents: p["proponents"]
                                                    .as_array()
                                                    .map(|a| {
                                                        a.iter()
                                                            .filter_map(|v| {
                                                                v.as_str()
                                                                    .map(|s| s.to_string())
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
                }
                Err(e) => {
                    tracing::warn!(batch = i, error = %e, "Skeleton extraction parse failed");
                }
            }

            if i % 25 == 0 || i == total_batches - 1 {
                progress(EnrichmentProgress::Phase1Progress {
                    batches_done: i + 1,
                    batches_total: total_batches,
                });
            }
        }

        // Deduplicate questions by similarity
        deduplicate_questions(&mut skeleton);

        Ok(skeleton)
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

/// Extract JSON from a model response, stripping markdown code fences.
fn extract_json_from_response(response: &str) -> &str {
    let trimmed = response.trim();

    // Try to extract from ```json ... ```
    if let Some(start) = trimmed.find("```json") {
        let start = start + 7;
        if let Some(end) = trimmed[start..].find("```") {
            return trimmed[start..start + end].trim();
        }
    }

    // Try to extract from ``` ... ```
    if let Some(start) = trimmed.find("```") {
        let start = start + 3;
        if let Some(end) = trimmed[start..].find("```") {
            let block = trimmed[start..start + end].trim();
            if block.starts_with('[') || block.starts_with('{') {
                return block;
            }
        }
    }

    // Assume the whole response is JSON
    trimmed
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
    fn extract_json_ignores_non_json_code_fence() {
        let response = "```python\nprint('hello')\n```";
        // Should return the whole trimmed response since the code fence
        // content doesn't start with { or [
        assert_eq!(
            extract_json_from_response(response),
            "```python\nprint('hello')\n```"
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
}
