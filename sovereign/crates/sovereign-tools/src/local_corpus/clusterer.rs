//! Obsidian-only clustering pipeline.
//!
//! Wraps `corpus_engine::enrichment::cluster_embeddings` (pure vector
//! math — HDBSCAN) and adds an LLM labelling pass that generates
//! `domain/subtopic` tag paths using the spec §6.3 prompt.
//!
//! Intentionally narrower than `FieldModelEngine`:
//!   - No skeleton extraction (vaults don't have "overview chunks").
//!   - No alignment / fault lines (those are philosophical-corpus
//!     concerns; vaults need taxonomy, not debate mapping).
//!   - Open questions are deferred to M4b.
//!
//! The output `LabeledClusterResult` feeds `preview::build_preview`,
//! which produces the `VaultPreview` the UI renders on the Organize
//! screen.

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine::enrichment::clustering::{
    cluster_embeddings, ClusterResult as EngineClusterResult, EnrichmentProgress,
};
use corpus_engine::enrichment::domain::ClusteringConfig;
use corpus_engine::{CorpusEngine, InferenceFn};
use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};

use super::progress::{ClusterStage, LocalCorpusProgress};

// ─── Config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// HDBSCAN minimum cluster size. Smaller = more, tighter clusters.
    pub min_cluster_size: usize,
    /// Minimum cluster-assignment confidence a note must clear to be
    /// tagged. Notes below this threshold land in the outlier panel
    /// rather than being force-assigned to the nearest cluster.
    pub min_confidence: f32,
    /// Notes matching multiple clusters above this confidence are
    /// candidates for multi-tagging. v1 implements the `Dominant`
    /// strategy regardless; this field is wired for v2.
    pub multi_tag_threshold: f32,
    pub multi_cluster_strategy: MultiClusterStrategy,
    /// Minimum **distinct notes** per cluster after the chunk-to-note
    /// rollup. Clusters with fewer notes than this threshold are
    /// collapsed: their notes land in the outlier panel with reason
    /// `SingletonCluster`, and the cluster itself disappears from the
    /// preview. `#[serde(default)]` so callers written before this
    /// field existed still deserialise cleanly.
    #[serde(default = "default_min_notes_per_cluster")]
    pub min_notes_per_cluster: usize,
}

fn default_min_notes_per_cluster() -> usize {
    2
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MultiClusterStrategy {
    /// Tag only the highest-confidence cluster. v1 default.
    Dominant,
    /// Tag every cluster whose confidence exceeds `multi_tag_threshold`.
    All,
    /// Flag for manual review, no auto-tag.
    Flag,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: 5,
            min_confidence: 0.4,
            multi_tag_threshold: 0.6,
            multi_cluster_strategy: MultiClusterStrategy::Dominant,
            min_notes_per_cluster: default_min_notes_per_cluster(),
        }
    }
}

// ─── Output ──────────────────────────────────────────────────────────

/// Per-cluster label produced by the LLM. Structure mirrors spec §6.3
/// exactly: tag path (`domain/subtopic`), a display name for the UI,
/// and a 2–3 sentence description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledCluster {
    pub id: i32,
    pub tag_path: String,
    pub display_name: String,
    pub description: String,
    pub note_count: usize,
    /// Chunk IDs closest to the cluster centroid. Used to render the
    /// "representative notes" list in the review UI.
    pub centroid_chunk_ids: Vec<u64>,
}

/// Aggregated output of the clustering + labelling pass. Plus enough
/// per-chunk data for the preview builder to classify outliers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledClusterResult {
    pub clusters: Vec<LabeledCluster>,
    /// chunk_id → cluster_id (`-1` = HDBSCAN noise).
    pub chunk_assignments: HashMap<u64, i32>,
    /// chunk_id → confidence in its assignment (cosine similarity
    /// between the chunk embedding and its cluster centroid, clamped
    /// to [0, 1]). Noise chunks get their confidence against the
    /// nearest cluster so the preview can rank them.
    pub chunk_confidences: HashMap<u64, f32>,
    /// For every HDBSCAN-noise chunk, the id of the nearest cluster
    /// centroid. Lets `preview::build_preview` promote a
    /// "noise-but-actually-close" note to that cluster when the user
    /// drags `min_confidence` low enough. Without this, noise chunks
    /// stay outliers regardless of threshold — a user-visible bug
    /// reported during the first demo.
    #[serde(default)]
    pub noise_best_cluster: HashMap<u64, i32>,
    pub noise_chunks: Vec<u64>,
    /// Open-question detection is deferred to M4b. Empty vec for now.
    pub open_questions: Vec<OpenQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenQuestion {
    pub gap_description: String,
    pub relevant_cluster_ids: Vec<i32>,
}

// ─── The Clusterer ───────────────────────────────────────────────────

pub struct Clusterer {
    engine: Arc<CorpusEngine>,
    inference: InferenceFn,
}

impl Clusterer {
    pub fn new(engine: Arc<CorpusEngine>, inference: InferenceFn) -> Self {
        Self { engine, inference }
    }

    /// Run the full cluster + label pipeline.
    ///
    /// 1. Load chunk embeddings from the index.
    /// 2. HDBSCAN clustering (no inference).
    /// 3. For each cluster, call the LLM with the spec §6.3 prompt,
    ///    parse into `{tag_path, display_name, description}`.
    /// 4. Compute per-chunk confidences from centroid cosine.
    pub async fn run(
        &self,
        corpus_id: &str,
        config: &ClusterConfig,
        on_progress: Arc<dyn Fn(LocalCorpusProgress) + Send + Sync>,
    ) -> Result<LabeledClusterResult> {
        let index = self
            .engine
            .open_index_for_corpus(corpus_id)
            .await
            .map_err(|e| Error::Execution(format!("open index '{corpus_id}': {e}")))?;

        // ── 1 + 2: HDBSCAN via corpus-engine ─────────────────────────
        let cluster_cfg = ClusteringConfig {
            min_cluster_size: config.min_cluster_size,
            epsilon: 0.2,
            label_sample_size: 5,
            max_cluster_points: 10_000,
            reduced_dims: 0,
        };
        on_progress(LocalCorpusProgress::Clustering {
            stage: ClusterStage::EmbeddingMatrix,
        });
        let bridge_cb: Arc<dyn Fn(LocalCorpusProgress) + Send + Sync> =
            Arc::clone(&on_progress);
        let stage_cb = move |p: EnrichmentProgress| {
            if let EnrichmentProgress::ClusteringStep { step, .. } = &p {
                match *step {
                    "running-hdbscan" | "hdbscan" => {
                        bridge_cb(LocalCorpusProgress::Clustering {
                            stage: ClusterStage::HdbscanRun,
                        });
                    }
                    _ => {}
                }
            }
        };
        let cluster_result = cluster_embeddings(&index, &cluster_cfg, &stage_cb)
            .await
            .map_err(|e| Error::Execution(format!("cluster_embeddings: {e}")))?;

        // ── 3: LLM labelling pass ─────────────────────────────────────
        on_progress(LocalCorpusProgress::Clustering {
            stage: ClusterStage::LlmLabeling,
        });
        let mut labeled = Vec::with_capacity(cluster_result.clusters.len());
        for cluster in &cluster_result.clusters {
            let chunks = index
                .get_chunks(&cluster.central_chunks)
                .await
                .map_err(|e| Error::Execution(format!("get_chunks: {e}")))?;
            let prompt = build_label_prompt(&chunks);

            let raw = match (self.inference)(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        cluster_id = cluster.id,
                        "cluster labelling failed, using fallback: {e}"
                    );
                    // Fallback label — keeps the UI flow working even
                    // when the LLM call errors. Tag path is derived
                    // from the cluster id so the user can still edit
                    // it later (M4b).
                    labeled.push(LabeledCluster {
                        id: cluster.id,
                        tag_path: format!("uncategorized/cluster-{}", cluster.id),
                        display_name: format!("Cluster {}", cluster.id),
                        description: "Label could not be generated."
                            .into(),
                        note_count: cluster.size,
                        centroid_chunk_ids: cluster.central_chunks.clone(),
                    });
                    continue;
                }
            };

            let parsed = parse_label_response(&raw);
            labeled.push(LabeledCluster {
                id: cluster.id,
                tag_path: parsed.tag_path,
                display_name: parsed.display_name,
                description: parsed.description,
                note_count: cluster.size,
                centroid_chunk_ids: cluster.central_chunks.clone(),
            });
        }

        // ── 4: per-chunk confidences (centroid cosine) ───────────────
        // Fetch all chunk embeddings once and compute against every
        // cluster centroid. For each chunk, pick the best match.
        let (confidences, noise_best) =
            compute_confidences(&index, &cluster_result).await?;

        let noise_chunks: Vec<u64> = cluster_result
            .assignments
            .iter()
            .filter_map(|(id, cid)| if *cid < 0 { Some(*id) } else { None })
            .collect();

        Ok(LabeledClusterResult {
            clusters: labeled,
            chunk_assignments: cluster_result.assignments,
            chunk_confidences: confidences,
            noise_best_cluster: noise_best,
            noise_chunks,
            open_questions: Vec::new(),
        })
    }
}

// ─── Prompt + parsing ────────────────────────────────────────────────

fn build_label_prompt(chunks: &[corpus_engine::StoredChunk]) -> String {
    // Spec §6.3 prompt, verbatim structure. Keeps the tag-path grammar
    // predictable so the UI and the write-back layer agree on the
    // shape.
    let samples: String = chunks
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, c)| {
            let title = c.title.clone().unwrap_or_else(|| format!("chunk-{i}"));
            let body: String = c.content.chars().take(600).collect();
            format!("--- Sample {} ({}) ---\n{}", i + 1, title, body)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "Given these representative chunks from a cluster, provide:\n\
         1. A top-level domain (1-2 words, e.g. \"epistemology\", \"writing\", \"projects\")\n\
         2. A specific subtopic (2-4 words, e.g. \"philosophy-of-mind\")\n\
         3. A 2-3 sentence description of what unites these notes.\n\n\
         Format your response as JSON with exactly these keys:\n\
         {{\"domain\": \"...\", \"subtopic\": \"...\", \"description\": \"...\"}}\n\n\
         Rules for domain and subtopic: lowercase, hyphen-separated, no special characters.\n\
         Aim for labels a thoughtful librarian would choose.\n\n\
         Chunks:\n\n{samples}"
    )
}

pub(crate) struct ParsedLabel {
    pub tag_path: String,
    pub display_name: String,
    pub description: String,
}

pub(crate) fn parse_label_response(raw: &str) -> ParsedLabel {
    // Accept either a raw JSON object or one wrapped in code fences.
    let trimmed = extract_json_block(raw);
    #[derive(Deserialize)]
    struct Shape {
        domain: Option<String>,
        subtopic: Option<String>,
        description: Option<String>,
    }
    let parsed: Shape = serde_json::from_str(trimmed).unwrap_or(Shape {
        domain: None,
        subtopic: None,
        description: None,
    });
    let domain = normalise_tag_component(parsed.domain.as_deref().unwrap_or("uncategorized"));
    let subtopic = normalise_tag_component(parsed.subtopic.as_deref().unwrap_or("notes"));
    let description = parsed
        .description
        .unwrap_or_else(|| "No description generated.".into())
        .trim()
        .to_string();

    let tag_path = format!("{domain}/{subtopic}");
    let display_name = display_name_for(&subtopic);
    ParsedLabel {
        tag_path,
        display_name,
        description,
    }
}

pub(crate) fn normalise_tag_component(raw: &str) -> String {
    // Lowercase, replace non-alphanumeric with hyphens, collapse
    // runs of hyphens, trim leading/trailing hyphens. Empty input
    // yields "notes" so we never emit `sovereign//...`.
    let mut out = String::with_capacity(raw.len());
    let mut prev_hyphen = true;
    for ch in raw.chars().flat_map(|c| c.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_hyphen = false;
        } else if !prev_hyphen {
            out.push('-');
            prev_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "notes".into()
    } else {
        // Cap length so pathological LLM output can't produce
        // unusable tags. 48 chars is generous but finite.
        out.chars().take(48).collect()
    }
}

fn display_name_for(subtopic: &str) -> String {
    // `philosophy-of-mind` → `Philosophy Of Mind`.
    subtopic
        .split('-')
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let first_up: String = first.to_uppercase().collect();
                    format!("{first_up}{}", chars.as_str())
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_json_block(raw: &str) -> &str {
    // Find the first `{` and the matching last `}` — naive, but
    // handles most wrapping patterns (code fences, leading prose).
    let start = match raw.find('{') {
        Some(i) => i,
        None => return raw.trim(),
    };
    let end = match raw.rfind('}') {
        Some(i) => i + 1,
        None => return raw.trim(),
    };
    if end > start {
        &raw[start..end]
    } else {
        raw.trim()
    }
}

// ─── Confidence computation ──────────────────────────────────────────

/// For every chunk, compute `(confidence, optional nearest-cluster
/// id for noise)`. Assigned chunks score against their own cluster's
/// centroid; noise chunks score against the *nearest* cluster and
/// have their nearest-cluster-id recorded in the second map.
///
/// The nearest-cluster map is the key to live threshold adjustment:
/// when the user drags `min_confidence` below a noise chunk's
/// nearest-match score, `preview::build_preview` promotes that chunk
/// into the nearest cluster rather than leaving it as a permanent
/// outlier.
async fn compute_confidences(
    index: &corpus_engine::CorpusIndex,
    cluster_result: &EngineClusterResult,
) -> Result<(HashMap<u64, f32>, HashMap<u64, i32>)> {
    let (chunk_ids, embeddings) = index
        .stream_embedding_column()
        .await
        .map_err(|e| Error::Execution(format!("load embeddings: {e}")))?;

    let centroids: HashMap<i32, Vec<f32>> = cluster_result
        .clusters
        .iter()
        .map(|c| (c.id, c.centroid.clone()))
        .collect();

    let mut confidences: HashMap<u64, f32> = HashMap::with_capacity(chunk_ids.len());
    let mut noise_best: HashMap<u64, i32> = HashMap::new();
    for (chunk_id, embedding) in chunk_ids.into_iter().zip(embeddings.into_iter()) {
        let Some(assigned_cid) = cluster_result.assignments.get(&chunk_id) else {
            continue;
        };
        if *assigned_cid < 0 {
            if let Some((best_cid, score)) = best_centroid(&centroids, &embedding) {
                confidences.insert(chunk_id, score);
                noise_best.insert(chunk_id, best_cid);
            } else {
                confidences.insert(chunk_id, 0.0);
            }
        } else if let Some(centroid) = centroids.get(assigned_cid) {
            confidences.insert(chunk_id, cosine_sim(centroid, &embedding));
        }
    }
    Ok((confidences, noise_best))
}

fn best_centroid(centroids: &HashMap<i32, Vec<f32>>, v: &[f32]) -> Option<(i32, f32)> {
    centroids
        .iter()
        .map(|(id, c)| (*id, cosine_sim(c, v)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
}

pub(crate) fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = (na.sqrt() * nb.sqrt()).max(1e-6);
    (dot / denom).clamp(0.0, 1.0)
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_basic() {
        assert_eq!(normalise_tag_component("Philosophy Of Mind"), "philosophy-of-mind");
        assert_eq!(normalise_tag_component("writing!!"), "writing");
        assert_eq!(normalise_tag_component("  spaces  "), "spaces");
    }

    #[test]
    fn normalise_unicode_stripped() {
        // Non-ASCII chars get folded into hyphens, then collapsed.
        // `épistémologie` → "pist-mologie" after stripping accented
        // chars. Not ideal for non-English users but bounded and
        // predictable for v1.
        let out = normalise_tag_component("épistémologie");
        assert!(!out.is_empty());
        assert!(!out.contains(' '));
        assert!(out.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    #[test]
    fn normalise_empty_becomes_notes() {
        assert_eq!(normalise_tag_component(""), "notes");
        assert_eq!(normalise_tag_component("!!!"), "notes");
    }

    #[test]
    fn normalise_caps_length() {
        let long = "a".repeat(200);
        let out = normalise_tag_component(&long);
        assert_eq!(out.len(), 48);
    }

    #[test]
    fn parse_label_handles_code_fence() {
        let raw = r#"Here's the label:
```json
{"domain": "Epistemology", "subtopic": "Philosophy of Mind", "description": "Notes about consciousness."}
```"#;
        let parsed = parse_label_response(raw);
        assert_eq!(parsed.tag_path, "epistemology/philosophy-of-mind");
        assert_eq!(parsed.display_name, "Philosophy Of Mind");
        assert!(parsed.description.contains("consciousness"));
    }

    #[test]
    fn parse_label_handles_malformed_json() {
        // Model output with missing keys + trailing junk. Must not
        // panic; must produce a usable fallback label.
        let raw = "sorry I can't help with that";
        let parsed = parse_label_response(raw);
        assert_eq!(parsed.tag_path, "uncategorized/notes");
        assert!(!parsed.description.is_empty());
    }

    #[test]
    fn parse_label_handles_partial_json() {
        let raw = r#"{"domain": "Writing"}"#;
        let parsed = parse_label_response(raw);
        assert_eq!(parsed.tag_path, "writing/notes");
    }

    #[test]
    fn cosine_identical_vectors() {
        let v = vec![1.0, 0.0, 1.0];
        assert!((cosine_sim(&v, &v) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine_sim(&a, &b).abs() < 1e-4);
    }

    #[test]
    fn cosine_mismatched_length_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_sim(&a, &b), 0.0);
    }

    #[test]
    fn cluster_config_defaults_match_spec() {
        let c = ClusterConfig::default();
        assert_eq!(c.min_cluster_size, 5);
        assert!((c.min_confidence - 0.4).abs() < 1e-6);
        assert!((c.multi_tag_threshold - 0.6).abs() < 1e-6);
        assert!(matches!(c.multi_cluster_strategy, MultiClusterStrategy::Dominant));
    }
}
