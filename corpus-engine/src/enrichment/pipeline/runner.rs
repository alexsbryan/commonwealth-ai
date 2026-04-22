//! Per-phase runner — glues the `Pipeline` trait, `ExemplarBank`,
//! `PhaseCache`, `RunOutputWriter`, and injected `EmbedFn` +
//! `ChatCompletionFn` into an executor the CLI calls per subcommand.
//!
//! Landing 2 implements phase 1 (per-chapter question extraction).
//! Subsequent phases land incrementally; each `phase_N_*` method is
//! additive and does not break the others.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::exemplar_bank::{Exemplar, ExemplarBank};
use super::phase_cache::PhaseCache;
use super::run_output::RunOutputWriter;
use super::trait_def::Pipeline;
use super::types::*;
use super::vector_clustering::cluster_vectors;
use crate::error::{Error, Result};
use crate::types::EmbedFn;

/// Which chapters the developer wants phase 1 to run against.
#[derive(Debug, Clone)]
pub enum ChapterSelection {
    /// Just the given chapter IDs. Output lands in `runs/` but the
    /// cache is NOT updated — subset runs are diagnostic, not ground
    /// truth for phases 2+.
    Subset(Vec<String>),
    /// Every chapter in the manifest. Updates the cache.
    Full,
}

impl ChapterSelection {
    pub fn mode_label(&self) -> &'static str {
        match self {
            Self::Subset(_) => "subset",
            Self::Full => "full",
        }
    }

    pub fn should_update_cache(&self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Streaming progress events for phase 1.
#[derive(Debug, Clone)]
pub enum Phase1Progress<'a> {
    Start {
        total: usize,
        exemplars_loaded: usize,
    },
    ChapterStart {
        i: usize,
        total: usize,
        chapter_id: &'a str,
    },
    ChapterDone {
        chapter_id: &'a str,
        question_count: usize,
    },
    ChapterFailed {
        chapter_id: &'a str,
        reason: &'a str,
    },
    Done {
        produced: usize,
        failed: usize,
        run_path: &'a Path,
    },
}

/// Outcome of one phase-1 call.
#[derive(Debug, Clone)]
pub struct Phase1RunResult {
    pub output: Phase1Output,
    pub run_path: PathBuf,
    pub cache_updated: bool,
    pub failures: Vec<Phase1Failure>,
}

#[derive(Debug, Clone)]
pub struct Phase1Failure {
    pub chapter_id: String,
    pub reason: String,
}

/// Executor for one admin-harness pipeline run.
///
/// The CLI constructs a `PhaseRunner` once per subcommand (cheap)
/// and invokes the phase method matching its command. Heavy state
/// (loaded exemplars, LanceDB handles) lives behind the injected
/// closures and the `Arc<dyn Pipeline>` so this struct stays cheap
/// to clone.
pub struct PhaseRunner {
    pipeline: Arc<dyn Pipeline>,
    embed: EmbedFn,
    chat: ChatCompletionFn,
    cache: PhaseCache,
    runs: RunOutputWriter,
    exemplars_dir: PathBuf,
}

impl PhaseRunner {
    pub fn new(
        pipeline: Arc<dyn Pipeline>,
        embed: EmbedFn,
        chat: ChatCompletionFn,
        cache: PhaseCache,
        runs: RunOutputWriter,
        exemplars_dir: impl AsRef<Path>,
    ) -> Self {
        Self {
            pipeline,
            embed,
            chat,
            cache,
            runs,
            exemplars_dir: exemplars_dir.as_ref().to_path_buf(),
        }
    }

    pub fn pipeline(&self) -> &Arc<dyn Pipeline> {
        &self.pipeline
    }

    pub fn cache(&self) -> &PhaseCache {
        &self.cache
    }

    pub fn runs(&self) -> &RunOutputWriter {
        &self.runs
    }

    pub fn exemplar_path(&self, phase: PipelinePhase) -> PathBuf {
        self.exemplars_dir.join(format!("{}.json", phase.id()))
    }

    /// Run phase 1 against the supplied chapters.
    ///
    /// The caller assembles `ChapterInput`s from the corpus manifest +
    /// source text (CLI-side responsibility) and hands them in here.
    /// The runner is then pure orchestration: select exemplars, compose
    /// prompts, call chat, parse, persist.
    pub async fn phase_1_extract_questions<F>(
        &self,
        chapters: &[ChapterInput],
        selection: &ChapterSelection,
        progress: F,
    ) -> Result<Phase1RunResult>
    where
        F: Fn(Phase1Progress<'_>),
    {
        // Resolve which chapters to run.
        let targets: Vec<&ChapterInput> = match selection {
            ChapterSelection::Full => chapters.iter().collect(),
            ChapterSelection::Subset(ids) => {
                let mut picked = Vec::with_capacity(ids.len());
                for id in ids {
                    let found = chapters.iter().find(|c| c.chapter_id == *id).ok_or_else(
                        || Error::InvalidInput(format!("chapter not found in manifest: {id}")),
                    )?;
                    picked.push(found);
                }
                picked
            }
        };
        if targets.is_empty() {
            return Err(Error::InvalidInput(
                "phase 1 was asked to run with zero target chapters".into(),
            ));
        }

        // Load the exemplar bank. Bank presence is optional — phase 1
        // runs with an empty bank (no few-shot context) the first time
        // through.
        let exemplar_path = self.exemplar_path(PipelinePhase::Questions);
        let bank = ExemplarBank::load_embedded(
            &exemplar_path,
            PipelinePhase::Questions,
            &self.embed,
        )
        .await?;
        let k = self.pipeline.top_k_exemplars(PipelinePhase::Questions);

        progress(Phase1Progress::Start {
            total: targets.len(),
            exemplars_loaded: bank.len(),
        });

        let mut extracted: Vec<ExtractedQuestion> = Vec::with_capacity(targets.len());
        let mut failures: Vec<Phase1Failure> = Vec::new();

        for (i, chapter) in targets.iter().enumerate() {
            progress(Phase1Progress::ChapterStart {
                i: i + 1,
                total: targets.len(),
                chapter_id: &chapter.chapter_id,
            });

            // Build the query-side embedding used to score exemplars
            // against this chapter.
            let query_text = phase1_query_text(chapter);
            let picked: Vec<&Exemplar> = if bank.is_empty() {
                Vec::new()
            } else {
                let query_emb = (self.embed)(&query_text).await?;
                bank.select_top_k(&query_emb, k)
            };

            let prompt = self.pipeline.compose_phase1(chapter, &picked);

            let response = match (self.chat)(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    let reason = format!("chat error: {e}");
                    progress(Phase1Progress::ChapterFailed {
                        chapter_id: &chapter.chapter_id,
                        reason: &reason,
                    });
                    failures.push(Phase1Failure {
                        chapter_id: chapter.chapter_id.clone(),
                        reason,
                    });
                    continue;
                }
            };

            let parsed = match self.pipeline.parse_phase1(&response) {
                Ok(p) => p,
                Err(e) => {
                    let reason = format!("parse error: {e}");
                    progress(Phase1Progress::ChapterFailed {
                        chapter_id: &chapter.chapter_id,
                        reason: &reason,
                    });
                    failures.push(Phase1Failure {
                        chapter_id: chapter.chapter_id.clone(),
                        reason,
                    });
                    continue;
                }
            };

            progress(Phase1Progress::ChapterDone {
                chapter_id: &chapter.chapter_id,
                question_count: parsed.questions.len(),
            });

            extracted.push(ExtractedQuestion {
                chapter_id: chapter.chapter_id.clone(),
                questions: parsed.questions,
                reveals: parsed.reveals,
                thematic_carriers: parsed.thematic_carriers,
            });
        }

        // Assemble the Phase1Output.
        let output = Phase1Output {
            schema_version: Phase1Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            questions_by_chapter: extracted.clone(),
            written_at: now_rfc3339(),
        };

        // Write the run file (always) + update the cache (Full only).
        let run_path = self.runs.write(
            PipelinePhase::Questions,
            selection.mode_label(),
            &output,
        )?;
        let cache_updated = if selection.should_update_cache() {
            self.cache.write(PipelinePhase::Questions, &output)?;
            true
        } else {
            false
        };

        progress(Phase1Progress::Done {
            produced: extracted.len(),
            failed: failures.len(),
            run_path: &run_path,
        });

        Ok(Phase1RunResult {
            output,
            run_path,
            cache_updated,
            failures,
        })
    }
}

// ── Phases 2-7 result types ───────────────────────────────

#[derive(Debug, Clone)]
pub struct PhaseRunResult<T> {
    pub output: T,
    pub run_path: std::path::PathBuf,
    pub cache_updated: bool,
    pub failures: Vec<PhaseFailure>,
}

#[derive(Debug, Clone)]
pub struct PhaseFailure {
    pub context: String,
    pub reason: String,
}

pub type Phase2RunResult = PhaseRunResult<Phase2Output>;
pub type Phase3RunResult = PhaseRunResult<Phase3Output>;
pub type Phase4RunResult = PhaseRunResult<Phase4Output>;
pub type Phase5RunResult = PhaseRunResult<Phase5Output>;
pub type Phase6RunResult = PhaseRunResult<Phase6Output>;
pub type Phase7RunResult = PhaseRunResult<Phase7Output>;

/// A single cascade step's outcome, one variant per phase that can run.
#[derive(Debug, Clone)]
pub enum CascadeStep {
    Phase1(Phase1RunResult),
    Phase2(Phase2RunResult),
    Phase3(Phase3RunResult),
    Phase4(Phase4RunResult),
    Phase5(Phase5RunResult),
    Phase6(Phase6RunResult),
    Phase7(Phase7RunResult),
}

#[derive(Debug, Clone)]
pub struct CascadeResult {
    pub steps: Vec<CascadeStep>,
}

// ── PhaseRunner phase 2-7 + cascade ───────────────────────────

impl PhaseRunner {
    /// Phase 2 — cluster every question from phase 1 by embedding
    /// similarity. Reads `Questions` cache, embeds each question,
    /// runs HDBSCAN, writes `QuestionClusters` cache.
    pub async fn phase_2_cluster_questions(&self) -> Result<Phase2RunResult> {
        let phase1: Phase1Output = self
            .cache
            .read(PipelinePhase::Questions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Questions))?;

        // Flatten into (ref, text) pairs.
        let mut refs: Vec<(QuestionRef, String)> = Vec::new();
        for entry in &phase1.questions_by_chapter {
            for (idx, q) in entry.questions.iter().enumerate() {
                refs.push((
                    QuestionRef {
                        chapter_id: entry.chapter_id.clone(),
                        question_index: idx,
                    },
                    q.clone(),
                ));
            }
        }
        if refs.is_empty() {
            return Err(Error::InvalidInput(
                "phase 1 cache has no questions to cluster".into(),
            ));
        }

        // Embed.
        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(refs.len());
        for (_, text) in &refs {
            embeddings.push((self.embed)(text).await?);
        }

        // Cluster.
        let config = self.pipeline.question_clustering_config();
        let result = cluster_vectors(&embeddings, &config)?;

        // Group.
        let mut clusters: std::collections::HashMap<i32, Vec<QuestionRef>> =
            std::collections::HashMap::new();
        let mut unclustered: Vec<QuestionRef> = Vec::new();
        for (i, label) in result.labels.iter().enumerate() {
            let r = refs[i].0.clone();
            if *label < 0 {
                unclustered.push(r);
            } else {
                clusters.entry(*label).or_default().push(r);
            }
        }
        let mut cluster_vec: Vec<QuestionCluster> = clusters
            .into_iter()
            .map(|(id, members)| QuestionCluster {
                id: format!("qc_{:04}", id + 1),
                question_refs: members,
            })
            .collect();
        cluster_vec.sort_by(|a, b| a.id.cmp(&b.id));

        let output = Phase2Output {
            schema_version: Phase2Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            clusters: cluster_vec,
            unclustered,
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(
            PipelinePhase::QuestionClusters,
            "full",
            &output,
        )?;
        self.cache.write(PipelinePhase::QuestionClusters, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures: Vec::new(),
        })
    }

    /// Phase 3 — name the canonical concern for each question cluster.
    pub async fn phase_3_name_concerns(
        &self,
        ctx: &CorpusContext,
    ) -> Result<Phase3RunResult> {
        let phase1: Phase1Output = self
            .cache
            .read(PipelinePhase::Questions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Questions))?;
        let phase2: Phase2Output = self
            .cache
            .read(PipelinePhase::QuestionClusters)?
            .ok_or_else(|| missing_upstream(PipelinePhase::QuestionClusters))?;

        let bank = ExemplarBank::load_embedded(
            &self.exemplar_path(PipelinePhase::Concerns),
            PipelinePhase::Concerns,
            &self.embed,
        )
        .await?;
        let k = self.pipeline.top_k_exemplars(PipelinePhase::Concerns);

        let mut concerns: Vec<CanonicalConcern> = Vec::with_capacity(phase2.clusters.len());
        let mut failures: Vec<PhaseFailure> = Vec::new();

        for (ci, cluster) in phase2.clusters.iter().enumerate() {
            // Pull chapter excerpts for the first few refs.
            let excerpts: Vec<&ChapterInput> = cluster
                .question_refs
                .iter()
                .take(3)
                .filter_map(|r| ctx.chapters.iter().find(|c| c.chapter_id == r.chapter_id))
                .collect();

            // Query text for exemplar selection = the first question's text.
            let query_text = first_question_text(&phase1, &cluster.question_refs)
                .unwrap_or_else(|| "canonical concern".to_string());
            let picked: Vec<&Exemplar> = if bank.is_empty() {
                Vec::new()
            } else {
                let query_emb = (self.embed)(&query_text).await?;
                bank.select_top_k(&query_emb, k)
            };

            let prompt = self.pipeline.compose_phase3(cluster, &excerpts, &picked);
            let response = match (self.chat)(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    failures.push(PhaseFailure {
                        context: cluster.id.clone(),
                        reason: format!("chat: {e}"),
                    });
                    continue;
                }
            };
            let parsed = match self.pipeline.parse_phase3(&response) {
                Ok(p) => p,
                Err(e) => {
                    failures.push(PhaseFailure {
                        context: cluster.id.clone(),
                        reason: format!("parse: {e}"),
                    });
                    continue;
                }
            };
            concerns.push(CanonicalConcern {
                id: format!("cc_{:04}", ci + 1),
                cluster_id: cluster.id.clone(),
                concern_text: parsed.concern_text,
                scope: parsed.scope,
                primary_arcs: parsed.primary_arcs,
            });
        }

        let output = Phase3Output {
            schema_version: Phase3Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            concerns,
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::Concerns, "full", &output)?;
        self.cache.write(PipelinePhase::Concerns, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures,
        })
    }

    /// Phase 4 — cluster paragraph-level chunk embeddings. Embeds every
    /// chunk on-the-fly (can be slow; admin corpora are in the low
    /// thousands of chunks).
    pub async fn phase_4_cluster_chunks(
        &self,
        ctx: &CorpusContext,
    ) -> Result<Phase4RunResult> {
        if ctx.chunks.is_empty() {
            return Err(Error::InvalidInput(
                "phase 4 requires paragraph chunks in the corpus context".into(),
            ));
        }

        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(ctx.chunks.len());
        for chunk in &ctx.chunks {
            embeddings.push((self.embed)(&chunk.text).await?);
        }

        let config = self.pipeline.chunk_clustering_config();
        let result = cluster_vectors(&embeddings, &config)?;

        // Group into ChunkClusters with centroids.
        let members = result.members_by_cluster();
        let mut clusters: Vec<ChunkCluster> = Vec::with_capacity(members.len());
        for (label, indices) in &members {
            let chunk_ids: Vec<u64> = indices.iter().map(|&i| ctx.chunks[i].id).collect();
            let centroid = mean_vector(
                &indices.iter().map(|&i| embeddings[i].clone()).collect::<Vec<_>>(),
            );
            clusters.push(ChunkCluster {
                id: format!("kc_{:04}", label + 1),
                chunk_ids,
                noise: false,
                centroid,
            });
        }
        clusters.sort_by(|a, b| a.id.cmp(&b.id));

        // Collect noise as a synthetic "kc_noise" cluster (optional, for audit).
        let noise_ids: Vec<u64> = result
            .labels
            .iter()
            .enumerate()
            .filter_map(|(i, l)| if *l < 0 { Some(ctx.chunks[i].id) } else { None })
            .collect();
        if !noise_ids.is_empty() {
            clusters.push(ChunkCluster {
                id: "kc_noise".into(),
                chunk_ids: noise_ids,
                noise: true,
                centroid: Vec::new(),
            });
        }

        let output = Phase4Output {
            schema_version: Phase4Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            clusters,
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::ChunkClusters, "full", &output)?;
        self.cache.write(PipelinePhase::ChunkClusters, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures: Vec::new(),
        })
    }

    /// Phase 5 — extract grounded positions. For each canonical concern,
    /// align to the top-K chunk clusters by centroid cosine similarity
    /// (embedding of the concern text vs cluster centroid), then for
    /// each aligned cluster compose+call+parse.
    pub async fn phase_5_extract_positions(
        &self,
        ctx: &CorpusContext,
    ) -> Result<Phase5RunResult> {
        let concerns_out: Phase3Output = self
            .cache
            .read(PipelinePhase::Concerns)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Concerns))?;
        let chunks_out: Phase4Output = self
            .cache
            .read(PipelinePhase::ChunkClusters)?
            .ok_or_else(|| missing_upstream(PipelinePhase::ChunkClusters))?;

        if concerns_out.concerns.is_empty() {
            return Err(Error::InvalidInput(
                "phase 3 cache has no canonical concerns — re-run `sovereign enrich name-concerns`"
                    .into(),
            ));
        }
        let usable_clusters: Vec<&ChunkCluster> =
            chunks_out.clusters.iter().filter(|c| !c.noise).collect();
        if usable_clusters.is_empty() {
            return Err(Error::InvalidInput(
                "phase 4 cache has no non-noise chunk clusters".into(),
            ));
        }

        let bank = ExemplarBank::load_embedded(
            &self.exemplar_path(PipelinePhase::Positions),
            PipelinePhase::Positions,
            &self.embed,
        )
        .await?;
        let k_exemplars = self.pipeline.top_k_exemplars(PipelinePhase::Positions);
        const ALIGN_TOP_K: usize = 3;

        // Map chunk id → text for grounding lookups.
        let chunk_lookup: std::collections::HashMap<u64, &ChunkRecord> =
            ctx.chunks.iter().map(|c| (c.id, c)).collect();

        let mut positions: Vec<Position> = Vec::new();
        let mut failures: Vec<PhaseFailure> = Vec::new();
        let mut pos_ordinal = 0usize;

        for concern in &concerns_out.concerns {
            let concern_emb = (self.embed)(&concern.concern_text).await?;

            // Score each cluster by centroid cosine; take top-K.
            let mut scored: Vec<(f32, &ChunkCluster)> = usable_clusters
                .iter()
                .map(|cl| (cosine_similarity(&concern_emb, &cl.centroid), *cl))
                .collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(ALIGN_TOP_K);

            for (_, cluster) in scored {
                // Build (chunk_id, text) pairs for the cluster.
                let texts: Vec<(u64, String)> = cluster
                    .chunk_ids
                    .iter()
                    .take(8)
                    .filter_map(|id| {
                        chunk_lookup.get(id).map(|c| (*id, c.text.clone()))
                    })
                    .collect();
                if texts.is_empty() {
                    continue;
                }

                let picked: Vec<&Exemplar> = if bank.is_empty() {
                    Vec::new()
                } else {
                    bank.select_top_k(&concern_emb, k_exemplars)
                };

                let prompt =
                    self.pipeline
                        .compose_phase5(concern, cluster, &texts, &picked);
                let response = match (self.chat)(&prompt).await {
                    Ok(r) => r,
                    Err(e) => {
                        failures.push(PhaseFailure {
                            context: format!("{}:{}", concern.id, cluster.id),
                            reason: format!("chat: {e}"),
                        });
                        continue;
                    }
                };
                let parsed = match self.pipeline.parse_phase5(&response) {
                    Ok(p) => p,
                    Err(e) => {
                        failures.push(PhaseFailure {
                            context: format!("{}:{}", concern.id, cluster.id),
                            reason: format!("parse: {e}"),
                        });
                        continue;
                    }
                };
                // Backfill section_id on grounding entries when the
                // model omitted it.
                let grounding: Vec<Grounding> = parsed
                    .grounding
                    .into_iter()
                    .map(|mut g| {
                        if g.section_id.is_empty() {
                            if let Some(rec) = chunk_lookup.get(&g.chunk_id) {
                                g.section_id = rec.section_id.clone();
                            }
                        }
                        g
                    })
                    .collect();

                pos_ordinal += 1;
                positions.push(Position {
                    id: format!("pos_{:04}", pos_ordinal),
                    concern_id: concern.id.clone(),
                    chunk_cluster_id: cluster.id.clone(),
                    position_text: parsed.position_text,
                    grounding,
                    extensions: parsed.extensions,
                });
            }
        }

        let output = Phase5Output {
            schema_version: Phase5Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            positions,
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::Positions, "full", &output)?;
        self.cache.write(PipelinePhase::Positions, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures,
        })
    }

    /// Phase 6 — pairwise tension detection between positions aligned
    /// to the SAME canonical concern. Positions from different concerns
    /// are not paired (no structural signal there).
    pub async fn phase_6_detect_tensions(&self) -> Result<Phase6RunResult> {
        let pos_out: Phase5Output = self
            .cache
            .read(PipelinePhase::Positions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Positions))?;

        let bank = ExemplarBank::load_embedded(
            &self.exemplar_path(PipelinePhase::Tensions),
            PipelinePhase::Tensions,
            &self.embed,
        )
        .await?;
        let k = self.pipeline.top_k_exemplars(PipelinePhase::Tensions);

        // Group positions by concern_id.
        let mut by_concern: std::collections::BTreeMap<String, Vec<&Position>> =
            std::collections::BTreeMap::new();
        for p in &pos_out.positions {
            by_concern
                .entry(p.concern_id.clone())
                .or_default()
                .push(p);
        }

        let mut tensions: Vec<Tension> = Vec::new();
        let mut failures: Vec<PhaseFailure> = Vec::new();
        let mut t_ordinal = 0usize;

        for (_concern_id, positions) in &by_concern {
            if positions.len() < 2 {
                continue;
            }
            for i in 0..positions.len() {
                for j in (i + 1)..positions.len() {
                    let a = positions[i];
                    let b = positions[j];

                    let picked: Vec<&Exemplar> = if bank.is_empty() {
                        Vec::new()
                    } else {
                        // Query = concatenation of the two position texts.
                        let query = format!("{}\n\n{}", a.position_text, b.position_text);
                        let q_emb = (self.embed)(&query).await?;
                        bank.select_top_k(&q_emb, k)
                    };

                    let prompt = self.pipeline.compose_phase6(a, b, &picked);
                    let response = match (self.chat)(&prompt).await {
                        Ok(r) => r,
                        Err(e) => {
                            failures.push(PhaseFailure {
                                context: format!("{}×{}", a.id, b.id),
                                reason: format!("chat: {e}"),
                            });
                            continue;
                        }
                    };
                    let parsed = match self.pipeline.parse_phase6(&response) {
                        Ok(p) => p,
                        Err(e) => {
                            failures.push(PhaseFailure {
                                context: format!("{}×{}", a.id, b.id),
                                reason: format!("parse: {e}"),
                            });
                            continue;
                        }
                    };
                    if let Some(t) = parsed {
                        t_ordinal += 1;
                        tensions.push(Tension {
                            id: format!("t_{:04}", t_ordinal),
                            position_a_id: a.id.clone(),
                            position_b_id: b.id.clone(),
                            description: t.description,
                            specific_disagreement: t.specific_disagreement,
                            structural_type: t.structural_type,
                        });
                    }
                }
            }
        }

        let output = Phase6Output {
            schema_version: Phase6Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            tensions,
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::Tensions, "full", &output)?;
        self.cache.write(PipelinePhase::Tensions, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures,
        })
    }

    /// Phase 7 — gap detection. Single call; model sees concerns,
    /// positions, and chapter titles.
    pub async fn phase_7_detect_gaps(
        &self,
        ctx: &CorpusContext,
    ) -> Result<Phase7RunResult> {
        let concerns_out: Phase3Output = self
            .cache
            .read(PipelinePhase::Concerns)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Concerns))?;
        let pos_out: Phase5Output = self
            .cache
            .read(PipelinePhase::Positions)?
            .ok_or_else(|| missing_upstream(PipelinePhase::Positions))?;
        // Tensions are part of the atlas too but the prompt doesn't strictly
        // require them; check cache exists for staleness but don't block on
        // empty tensions.
        let _tensions_exists = self.cache.read::<Phase6Output>(PipelinePhase::Tensions)?;

        let bank = ExemplarBank::load_embedded(
            &self.exemplar_path(PipelinePhase::Gaps),
            PipelinePhase::Gaps,
            &self.embed,
        )
        .await?;
        let k = self.pipeline.top_k_exemplars(PipelinePhase::Gaps);

        let picked: Vec<&Exemplar> = if bank.is_empty() {
            Vec::new()
        } else {
            // Query: "gap detection" summary — cheap & stable.
            let q = "gap detection across canonical concerns".to_string();
            let q_emb = (self.embed)(&q).await?;
            bank.select_top_k(&q_emb, k)
        };

        let prompt = self.pipeline.compose_phase7(
            &concerns_out.concerns,
            &pos_out.positions,
            &ctx.chapter_titles,
            &picked,
        );
        let response = (self.chat)(&prompt).await?;
        let parsed = self.pipeline.parse_phase7(&response)?;

        let gaps: Vec<Gap> = parsed
            .into_iter()
            .enumerate()
            .map(|(i, p)| Gap {
                id: format!("gap_{:04}", i + 1),
                gap_text: p.gap_text,
                evidence: p.evidence,
                significance: p.significance,
            })
            .collect();

        let output = Phase7Output {
            schema_version: Phase7Output::SCHEMA_VERSION,
            pipeline_id: self.pipeline.id().to_string(),
            gaps,
            written_at: now_rfc3339(),
        };
        let run_path = self.runs.write(PipelinePhase::Gaps, "full", &output)?;
        self.cache.write(PipelinePhase::Gaps, &output)?;
        Ok(PhaseRunResult {
            output,
            run_path,
            cache_updated: true,
            failures: Vec::new(),
        })
    }

    /// Run every phase downstream of (and including) `from` in
    /// ordinal order. Phase 1 needs a chapter selection — when
    /// `from == Questions`, the caller must pass a non-empty
    /// `selection`. Other phases derive their inputs from `ctx` +
    /// upstream caches.
    pub async fn cascade(
        &self,
        from: PipelinePhase,
        ctx: &CorpusContext,
        phase1_selection: Option<ChapterSelection>,
    ) -> Result<CascadeResult> {
        let mut steps = Vec::new();

        for phase in PipelinePhase::ALL {
            if phase.ordinal() < from.ordinal() {
                continue;
            }
            match phase {
                PipelinePhase::Ingest => {
                    // Ingest is not an LLM phase; skip silently.
                }
                PipelinePhase::Questions => {
                    let sel = phase1_selection.clone().unwrap_or(ChapterSelection::Full);
                    let r = self
                        .phase_1_extract_questions(&ctx.chapters, &sel, |_| {})
                        .await?;
                    steps.push(CascadeStep::Phase1(r));
                }
                PipelinePhase::QuestionClusters => {
                    let r = self.phase_2_cluster_questions().await?;
                    steps.push(CascadeStep::Phase2(r));
                }
                PipelinePhase::Concerns => {
                    let r = self.phase_3_name_concerns(ctx).await?;
                    steps.push(CascadeStep::Phase3(r));
                }
                PipelinePhase::ChunkClusters => {
                    let r = self.phase_4_cluster_chunks(ctx).await?;
                    steps.push(CascadeStep::Phase4(r));
                }
                PipelinePhase::Positions => {
                    let r = self.phase_5_extract_positions(ctx).await?;
                    steps.push(CascadeStep::Phase5(r));
                }
                PipelinePhase::Tensions => {
                    let r = self.phase_6_detect_tensions().await?;
                    steps.push(CascadeStep::Phase6(r));
                }
                PipelinePhase::Gaps => {
                    let r = self.phase_7_detect_gaps(ctx).await?;
                    steps.push(CascadeStep::Phase7(r));
                }
            }
        }

        Ok(CascadeResult { steps })
    }
}

// ── Helpers ───────────────────────────────────────────────────

fn missing_upstream(phase: PipelinePhase) -> Error {
    Error::InvalidInput(format!(
        "phase '{}' cache is missing — run its upstream command first \
         (see `sovereign enrich status <corpus>` for phase states)",
        phase.id()
    ))
}

fn first_question_text(phase1: &Phase1Output, refs: &[QuestionRef]) -> Option<String> {
    let first = refs.first()?;
    let entry = phase1
        .questions_by_chapter
        .iter()
        .find(|e| e.chapter_id == first.chapter_id)?;
    entry.questions.get(first.question_index).cloned()
}

fn mean_vector(vecs: &[Vec<f32>]) -> Vec<f32> {
    if vecs.is_empty() {
        return Vec::new();
    }
    let dims = vecs[0].len();
    let mut sum = vec![0.0_f64; dims];
    for v in vecs {
        for (i, &x) in v.iter().enumerate() {
            sum[i] += x as f64;
        }
    }
    let n = vecs.len() as f64;
    sum.into_iter().map(|s| (s / n) as f32).collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Build the query-side text a chapter is scored against when picking
/// exemplars. We want a short, shape-agnostic handle — the chapter
/// title plus its opening prose. Longer bodies don't improve
/// selection and cost extra embed time.
fn phase1_query_text(chapter: &ChapterInput) -> String {
    let mut out = String::new();
    out.push_str(&chapter.title);
    out.push_str("\n\n");
    let mut budget = 800usize;
    for ch in chapter.text.chars() {
        if budget == 0 {
            break;
        }
        out.push(ch);
        budget -= 1;
    }
    out
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::pipelines::literary::LiteraryPipeline;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    fn chapter(id: &str, title: &str, body: &str) -> ChapterInput {
        ChapterInput {
            chapter_id: id.into(),
            title: title.into(),
            text: body.into(),
            metadata: HashMap::new(),
            approx_tokens: body.len() / 4,
        }
    }

    /// Deterministic embed: returns a 3-dim vector keyed by the first
    /// ASCII letter. Lets tests verify top-K selection without real
    /// embeddings.
    fn alphabet_embed() -> EmbedFn {
        Arc::new(move |s: &str| {
            let c = s.chars().next().unwrap_or('z');
            let v = match c {
                'a'..='i' => vec![1.0_f32, 0.0, 0.0],
                'j'..='r' => vec![0.0, 1.0, 0.0],
                _ => vec![0.0, 0.0, 1.0],
            };
            Box::pin(async move { Ok(v) })
        })
    }

    /// Deterministic chat: returns a fixed Phase1-shaped JSON keyed
    /// by the chapter title embedded in the user prompt. Fails for
    /// a chapter whose title includes "FAIL".
    fn canned_chat() -> ChatCompletionFn {
        Arc::new(move |prompt: &ChatPrompt| {
            let user = prompt.user.clone();
            let body: String = if user.contains("FAIL") {
                // Respond with something that doesn't parse.
                "not-json at all".into()
            } else if user.contains("NOJSON") {
                "```\ngarbage\n```".into()
            } else {
                let q = if user.contains("Two") {
                    r#"{"questions":["q-a","q-b"]}"#
                } else {
                    r#"{"questions":["only-q"]}"#
                };
                q.into()
            };
            Box::pin(async move { Ok(body) })
        })
    }

    fn runner_under_test(root: &Path) -> PhaseRunner {
        let cache = PhaseCache::new(root.join("cache"));
        let runs = RunOutputWriter::new(root.join("runs"));
        PhaseRunner::new(
            Arc::new(LiteraryPipeline::new()),
            alphabet_embed(),
            canned_chat(),
            cache,
            runs,
            root.join("exemplars"),
        )
    }

    #[tokio::test]
    async fn phase_1_full_writes_run_and_cache() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![
            chapter("ch_01", "Chapter 1", "A body with One question."),
            chapter("ch_02", "Chapter 2", "A body with Two questions."),
        ];
        let progress_count = AtomicUsize::new(0);
        let res = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_ev| {
                progress_count.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 2);
        assert!(res.cache_updated);
        assert!(res.run_path.exists());
        // Cache file should exist and round-trip through PhaseCache.
        let back: Option<Phase1Output> =
            runner.cache().read(PipelinePhase::Questions).unwrap();
        assert!(back.is_some());
        assert!(progress_count.load(Ordering::Relaxed) >= 4); // Start + 2 chapters + Done at minimum
    }

    #[tokio::test]
    async fn phase_1_subset_writes_run_but_not_cache() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![
            chapter("ch_01", "Chapter 1", "Body one"),
            chapter("ch_02", "Chapter 2", "Body two"),
            chapter("ch_03", "Chapter 3", "Body three"),
        ];
        let res = runner
            .phase_1_extract_questions(
                &chapters,
                &ChapterSelection::Subset(vec!["ch_01".into(), "ch_03".into()]),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 2);
        assert_eq!(res.output.questions_by_chapter[0].chapter_id, "ch_01");
        assert_eq!(res.output.questions_by_chapter[1].chapter_id, "ch_03");
        assert!(!res.cache_updated);
        assert!(res.run_path.exists());
        // Cache should NOT have been written by a subset run.
        assert!(runner
            .cache()
            .read::<Phase1Output>(PipelinePhase::Questions)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn phase_1_subset_rejects_unknown_chapter_id() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![chapter("ch_01", "Chapter 1", "body")];
        let err = runner
            .phase_1_extract_questions(
                &chapters,
                &ChapterSelection::Subset(vec!["nope".into()]),
                |_| {},
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("chapter not found"));
    }

    #[tokio::test]
    async fn phase_1_parse_failure_captured_as_failure_not_run_failure() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters = vec![
            chapter("ch_01", "Chapter 1", "A body with one question."),
            // The chat mock replies with non-JSON when title contains FAIL.
            chapter("ch_02", "FAIL Chapter", "body"),
        ];
        let res = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();
        assert_eq!(res.output.questions_by_chapter.len(), 1);
        assert_eq!(res.failures.len(), 1);
        assert_eq!(res.failures[0].chapter_id, "ch_02");
        assert!(res.failures[0].reason.contains("parse error"));
    }

    #[tokio::test]
    async fn phase_1_zero_chapters_errors_cleanly() {
        let dir = tempdir().unwrap();
        let runner = runner_under_test(dir.path());
        let chapters: Vec<ChapterInput> = Vec::new();
        let err = runner
            .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("zero target chapters"));
    }

    /// A chat mock that returns well-formed JSON for every phase 1-7
    /// call. Branches on which system preamble is present in the prompt
    /// to return the right shape.
    fn multiphase_chat() -> ChatCompletionFn {
        Arc::new(move |prompt: &ChatPrompt| {
            let sys = prompt.system.to_string();
            let body = if sys.contains("Phase 1") {
                // Echo the first word of the chapter body so different
                // chapters produce questions that embed into different
                // groups under `four_group_embed`.
                let seed = prompt
                    .user
                    .split("**Body:**")
                    .nth(1)
                    .and_then(|b| b.split_whitespace().next())
                    .unwrap_or("question")
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_string();
                format!(r#"{{"questions":["{seed} question for this chapter"]}}"#)
            } else if sys.contains("Phase 3") {
                r#"{"concern_text":"Can meaning survive defiance?","scope":"novel-wide"}"#
                    .to_string()
            } else if sys.contains("Phase 5") {
                // Echo any chunk_id the prompt mentions; grab the first
                // `chunk_id=N` token.
                let cid = prompt
                    .user
                    .split("chunk_id=")
                    .nth(1)
                    .and_then(|s| s.split('`').next())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                format!(
                    r#"{{"position_text":"a position","grounding":[{{"chunk_id":{cid},"section_id":"sec_0001","summary":"s"}}]}}"#
                )
            } else if sys.contains("Phase 6") {
                r#"{"tension":true,"description":"structural parallel","structural_type":"parallel_contrast"}"#.to_string()
            } else if sys.contains("Phase 7") {
                r#"{"gaps":[{"gap_text":"Vronsky social world fades","evidence":"few refs","significance":"medium"}]}"#.to_string()
            } else {
                r#"{"ok":true}"#.to_string()
            };
            Box::pin(async move { Ok(body) })
        })
    }

    /// Deterministic embed that maps text to a 4-dim vector keyed by the
    /// FIRST non-whitespace letter — enough variety to let HDBSCAN
    /// produce two+ clusters when inputs span two letter groups.
    fn four_group_embed() -> EmbedFn {
        Arc::new(move |text: &str| {
            let c = text
                .chars()
                .find(|c| !c.is_whitespace())
                .unwrap_or('z')
                .to_ascii_lowercase();
            let v: Vec<f32> = match c {
                'a'..='g' => vec![1.0, 0.0, 0.0, 0.0],
                'h'..='m' => vec![0.0, 1.0, 0.0, 0.0],
                'n'..='t' => vec![0.0, 0.0, 1.0, 0.0],
                _ => vec![0.0, 0.0, 0.0, 1.0],
            };
            // Add a tiny per-character jitter so HDBSCAN doesn't reject
            // identical vectors as degenerate.
            let len = text.len() as f32;
            let jitter: Vec<f32> = v
                .iter()
                .enumerate()
                .map(|(i, x)| x + 0.001 * (len + i as f32))
                .collect();
            Box::pin(async move { Ok(jitter) })
        })
    }

    fn multiphase_runner(root: &Path) -> PhaseRunner {
        let cache = PhaseCache::new(root.join("cache"));
        let runs = RunOutputWriter::new(root.join("runs"));
        PhaseRunner::new(
            Arc::new(LiteraryPipeline::new()),
            four_group_embed(),
            multiphase_chat(),
            cache,
            runs,
            root.join("exemplars"),
        )
    }

    fn synth_context() -> CorpusContext {
        // Three dense embed-groups × 3 chapters each so HDBSCAN (default
        // `min_cluster_size=3` on LiteraryPipeline) finds at least one
        // cluster per group.
        let groups = [
            ("apples", "Apples and acorns abound here."),
            ("hills", "Hills hide hopeful hares here."),
            ("nectar", "Nectar never numbs nerves here."),
        ];
        let mut chapters = Vec::new();
        let mut chapter_titles = Vec::new();
        for gi in 0..3 {
            for ci in 0..3 {
                let id = format!("ch_{:02}", gi * 3 + ci + 1);
                let title = format!("Chapter {}", gi * 3 + ci + 1);
                chapter_titles.push(title.clone());
                chapters.push(chapter(
                    &id,
                    &title,
                    &format!("{}, variation {}.", groups[gi].1, ci),
                ));
            }
        }
        let mut chunks: Vec<ChunkRecord> = Vec::new();
        let mut cid = 0u64;
        for gi in 0..3 {
            for ci in 0..6 {
                chunks.push(ChunkRecord {
                    id: cid,
                    section_id: format!("sec_{:04}", gi + 1),
                    text: format!("{} variation {}", groups[gi].1, ci),
                });
                cid += 1;
            }
        }
        CorpusContext { chapters, chunks, chapter_titles }
    }

    #[tokio::test]
    async fn phase_2_clusters_questions_from_cache() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();

        // Seed phase 1 with --full.
        runner
            .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();

        let res = runner.phase_2_cluster_questions().await.unwrap();
        assert!(res.cache_updated);
        assert!(res.run_path.exists());
        // Each chapter produced one question; groups are keyed on text's
        // first letter via four_group_embed. Clusters may coalesce
        // depending on HDBSCAN density — we only assert the output
        // shape is coherent, not a specific count.
        let total: usize =
            res.output.clusters.iter().map(|c| c.question_refs.len()).sum::<usize>()
                + res.output.unclustered.len();
        assert_eq!(total, 9);
    }

    #[tokio::test]
    async fn phase_3_requires_phase_2_cache() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        let err = runner.phase_3_name_concerns(&ctx).await.unwrap_err();
        assert!(format!("{err}").contains("cache is missing"));
    }

    #[tokio::test]
    async fn phase_4_clusters_chunks() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        let res = runner.phase_4_cluster_chunks(&ctx).await.unwrap();
        assert!(res.cache_updated);
        // Every non-noise cluster should carry a centroid.
        for c in &res.output.clusters {
            if !c.noise {
                assert!(!c.centroid.is_empty(), "non-noise cluster {} missing centroid", c.id);
            }
        }
    }

    #[tokio::test]
    async fn cascade_from_questions_runs_all_phases() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        let res = runner
            .cascade(
                PipelinePhase::Questions,
                &ctx,
                Some(ChapterSelection::Full),
            )
            .await
            .unwrap();
        // We expect 7 non-Ingest steps.
        assert_eq!(res.steps.len(), 7, "cascade should produce 7 steps");
        // Every phase cache should be populated.
        for phase in [
            PipelinePhase::Questions,
            PipelinePhase::QuestionClusters,
            PipelinePhase::Concerns,
            PipelinePhase::ChunkClusters,
            PipelinePhase::Positions,
            PipelinePhase::Tensions,
            PipelinePhase::Gaps,
        ] {
            let path = runner.cache().path(phase);
            assert!(path.exists(), "cache for {:?} not written", phase);
        }
    }

    #[tokio::test]
    async fn cascade_from_positions_only_reruns_downstream() {
        let dir = tempdir().unwrap();
        let runner = multiphase_runner(dir.path());
        let ctx = synth_context();
        // Seed phases 1-4 first.
        runner
            .phase_1_extract_questions(&ctx.chapters, &ChapterSelection::Full, |_| {})
            .await
            .unwrap();
        runner.phase_2_cluster_questions().await.unwrap();
        runner.phase_3_name_concerns(&ctx).await.unwrap();
        runner.phase_4_cluster_chunks(&ctx).await.unwrap();

        let res = runner
            .cascade(PipelinePhase::Positions, &ctx, None)
            .await
            .unwrap();
        // Positions, Tensions, Gaps — three steps.
        assert_eq!(res.steps.len(), 3);
        for step in &res.steps {
            match step {
                CascadeStep::Phase5(_) | CascadeStep::Phase6(_) | CascadeStep::Phase7(_) => {}
                other => panic!("unexpected cascade step: {other:?}"),
            }
        }
    }

    #[test]
    fn phase1_query_text_clamps_to_budget() {
        let body = "x".repeat(5000);
        let ch = chapter("ch", "Title", &body);
        let q = phase1_query_text(&ch);
        // Title (5) + "\n\n" (2) + 800 chars of body = 807. Allow some
        // slack for char vs byte counting.
        assert!(q.chars().count() <= 810);
        assert!(q.starts_with("Title"));
    }
}
