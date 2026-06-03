//! Domain trait and supporting types for field model enrichment.
//!
//! A `Domain` encodes the epistemic conventions of a field of knowledge.
//! It is the single extension point for generalizing across corpora.
//!
//! Object-safe. All methods take `&self`. No associated types.
//! The engine holds `Arc<dyn Domain>` and calls these methods directly.

use crate::index::StoredChunk;
use serde::{Deserialize, Serialize};

/// Type alias so domain prompt code can use `Chunk` without importing index types.
pub type Chunk = StoredChunk;

/// Encodes the epistemic conventions of a field of knowledge.
/// The single extension point for generalizing across corpora.
///
/// Object-safe. All methods take &self. No associated types.
/// The engine holds Arc<dyn Domain> and calls these methods directly.
///
/// Implementations in this task:
///   PhilosophyDomain — fully implemented
///   MultiDomain      — constructor only, methods todo!()
///   ScienceDomain, PolicyDomain, LegalDomain,
///   CommunityKnowledgeDomain — empty structs, all methods todo!()
pub trait Domain: Send + Sync + 'static {
    // ── Identity ──────────────────────────────────────────────────────────
    fn id(&self) -> &str;
    fn name(&self) -> &str;

    // ── Epistemic vocabulary ──────────────────────────────────────────────
    fn position_statuses(&self) -> &PositionStatusVocab;
    fn question_types(&self) -> &[QuestionType];

    // ── Overview document identification ──────────────────────────────────
    fn overview_filter(&self) -> ChunkFilter;

    // ── Prompts ───────────────────────────────────────────────────────────
    // Each takes concrete chunk references; returns a complete prompt string.
    // The engine calls these; domain implementations define them.

    fn skeleton_extraction_prompt(&self, chunks: &[&Chunk]) -> String;
    fn cluster_labeling_prompt(&self, representative_chunks: &[&Chunk]) -> String;
    fn fault_line_detection_prompt(
        &self,
        chunks_a: &[&Chunk],
        chunks_b: &[&Chunk],
        position_a: &str,
        position_b: &str,
    ) -> String;
    fn open_question_prompt(&self, chunks: &[&Chunk]) -> String;

    // ── Optional entity-extraction prompt (Phase 1b) ──────────────────────
    //
    // Domains that opt in produce typed Entity atoms (Person /
    // Organization / Initiative) and Involves edges from each batch
    // of chunks. The default impl returns `None` — most domains
    // (philosophy, science, literary atlases) don't run this step.
    // Personal and Conversational override to return the tuned prompt.
    //
    // Expected JSON shape from the model:
    // ```json
    // {
    //   "persons":       [{"name":"…","affiliation":"…","role":"…","mentions":["chunk-id"]}],
    //   "organizations": [{"name":"…","relationship":"…","mentions":["chunk-id"]}],
    //   "initiatives":   [{"name":"…","status":"…","participants":["…"],"mentions":["chunk-id"]}]
    // }
    // ```
    // `mentions` carries the chunk ids the entity appears in — the
    // resolver builds Involves edges from those, and the timeline
    // assembler later joins chunk_id back to the source row's
    // `created_at` to attach a timestamp.
    fn entity_extraction_prompt(&self, _chunks: &[&Chunk]) -> Option<String> {
        None
    }

    /// Optional JSON Schema (per OpenAI's `structured_output` shape, a
    /// `serde_json::Value` describing the response object) the
    /// inference adapter should constrain Phase 1b output to. When
    /// `Some`, the daemon-side `inference_to_inference_fn` forwards it
    /// as `CompletionRequest.structured_output`, letting llama-cpp's
    /// grammar sampler force well-formed JSON — turning a 54%-parse-
    /// fail Phase 1b (observed on enron-sample-multi-wide post-cap-
    /// bump, 2026-05-29) into a near-0% failure path. `None` falls
    /// back to free-form generation (the legacy path; domains that
    /// haven't authored a schema keep working unchanged).
    fn entity_extraction_schema(&self) -> Option<serde_json::Value> {
        None
    }

    // ── Clustering and alignment parameters ───────────────────────────────
    fn clustering_config(&self) -> ClusteringConfig;
    fn alignment_config(&self) -> AlignmentConfig;
    fn fault_line_config(&self) -> FaultLineConfig;

    // ── Storage strategy ──────────────────────────────────────────────────
    fn skeleton_storage(&self) -> SkeletonStorage;

    // ── Chunk role classification ─────────────────────────────────────────
    // Default covers the common case. Override for domain-specific
    // role vocabularies (e.g. LegalDomain adds ChunkRole::Holding).
    fn classify_chunk_role(&self, label: &ClusterLabel) -> ChunkRole {
        if !label.is_argumentative {
            return ChunkRole::NonArgumentative;
        }
        if label.is_open_question {
            return ChunkRole::OpenQuestion;
        }
        if label.is_objection {
            return ChunkRole::Objection;
        }
        ChunkRole::Argument
    }
}

// ── Vocabulary types ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PositionStatusVocab {
    pub dominant: &'static str,
    pub minority: &'static str,
    pub contested: &'static str,
    pub settled: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QuestionType {
    Factual,
    Normative,
    Conceptual,
    Legal,
    Practical,
}

#[derive(Debug, Clone)]
pub enum SkeletonStorage {
    /// LanceDB tables AND field_skeleton.json export.
    /// Use for small bounded domains: SEP, CRS, CBO.
    JsonAndLance,
    /// LanceDB tables only. field_index.json carries stats only.
    /// Use for large unbounded domains: Wikipedia, Stack Exchange.
    LanceOnly,
}

#[derive(Debug, Clone, Default)]
pub struct ChunkFilter {
    pub is_first_in_entry: Option<bool>,
    pub section_name_in: Option<Vec<String>>,
    pub min_token_count: Option<usize>,
    /// AND-join of exact equality pairs. Kept for backwards compat —
    /// new code should prefer `metadata_in` (OR within key) or
    /// `metadata_compare` (numeric predicates).
    pub metadata_key_values: Vec<(String, String)>,
    /// OR-join per key: a chunk passes iff
    /// `chunk.metadata[key]` is one of `allowed_values`. Multiple
    /// entries for the same key are ANDed, so two disjoint IN lists
    /// can be combined. Typical use:
    /// `("skill_id", ["research-analyst", "epistemic-research"])`.
    pub metadata_in: Vec<(String, Vec<String>)>,
    /// Numeric comparison predicates. All entries AND together.
    /// Typical use: `confidence > 0.7`.
    pub metadata_compare: Vec<MetadataComparison>,
}

/// Numeric comparison predicate on a metadata field.
/// The field is parsed as `f64` at eval time; missing or
/// non-numeric values fail closed.
#[derive(Debug, Clone)]
pub struct MetadataComparison {
    pub key: String,
    pub op: ComparisonOp,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
}

impl ChunkFilter {
    /// True when the filter imposes any predicate that requires
    /// access to raw chunk metadata. Callers use this as a fast-path
    /// guard to decide whether to load the heavier
    /// `StoredChunkWithMetadata` variant instead of `StoredChunk`.
    pub fn requires_metadata(&self) -> bool {
        !self.metadata_key_values.is_empty()
            || !self.metadata_in.is_empty()
            || !self.metadata_compare.is_empty()
    }

    /// Evaluate all metadata-based predicates against `metadata`
    /// (expected to be a JSON object from the chunk's stored
    /// metadata blob). Returns true only when every declared
    /// predicate passes. Missing keys fail closed.
    ///
    /// Does NOT evaluate `is_first_in_entry`, `section_name_in`,
    /// or `min_token_count` — those live on the chunk itself, not
    /// the metadata map.
    pub fn evaluate_metadata(&self, metadata: &serde_json::Value) -> bool {
        for (k, v) in &self.metadata_key_values {
            match metadata.get(k).and_then(|x| x.as_str()) {
                Some(s) if s == v.as_str() => continue,
                _ => return false,
            }
        }
        for (k, allowed) in &self.metadata_in {
            match metadata.get(k).and_then(|x| x.as_str()) {
                Some(s) if allowed.iter().any(|a| a == s) => continue,
                _ => return false,
            }
        }
        for cmp in &self.metadata_compare {
            let actual = metadata.get(&cmp.key).and_then(|x| {
                // Accept numbers OR numeric strings ("0.85" stored as
                // JSON string — the SQLite acquirer emits all non-text
                // values as strings, so both shapes may appear in the
                // wild).
                x.as_f64()
                    .or_else(|| x.as_str().and_then(|s| s.parse::<f64>().ok()))
            });
            let Some(v) = actual else {
                return false;
            };
            let ok = match cmp.op {
                ComparisonOp::Gt => v > cmp.value,
                ComparisonOp::Ge => v >= cmp.value,
                ComparisonOp::Lt => v < cmp.value,
                ComparisonOp::Le => v <= cmp.value,
                ComparisonOp::Eq => (v - cmp.value).abs() < f64::EPSILON,
                ComparisonOp::Ne => (v - cmp.value).abs() >= f64::EPSILON,
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct ClusteringConfig {
    pub min_cluster_size: usize,
    pub epsilon: f32,
    pub label_sample_size: usize,
    /// Maximum points to cluster directly. If the corpus has more chunks,
    /// a random sample of this size is clustered and the remaining points
    /// are assigned to their nearest cluster centroid. 0 = no limit.
    pub max_cluster_points: usize,
    /// Target dimensionality for random projection before HDBSCAN.
    /// Reduces O(n²·d) distance computation cost. 0 = no reduction.
    pub reduced_dims: usize,
}

#[derive(Debug, Clone)]
pub struct AlignmentConfig {
    pub alignment_threshold: f32,
    pub min_chunks_for_discovery: usize,
}

#[derive(Debug, Clone)]
pub struct FaultLineConfig {
    pub proximity_threshold: f32,
    pub min_confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChunkRole {
    Argument,
    Objection,
    Evidence,
    Historical,
    Illustrative,
    Definition,
    OpenQuestion,
    NonArgumentative,
}

impl ChunkRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Argument => "argument",
            Self::Objection => "objection",
            Self::Evidence => "evidence",
            Self::Historical => "historical",
            Self::Illustrative => "illustrative",
            Self::Definition => "definition",
            Self::OpenQuestion => "open_question",
            Self::NonArgumentative => "non_argumentative",
        }
    }
}

/// Returned by the cluster labeling call. Every field is populated by the
/// model. The engine uses these to classify chunks and detect open questions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterLabel {
    pub topic: String,
    pub position_name: Option<String>,
    pub is_argumentative: bool,
    pub is_objection: bool,
    pub is_open_question: bool,
    pub is_coherent: bool,
    /// Set by MultiDomain only. PhilosophyDomain always sets this to "philosophy".
    #[serde(default)]
    pub domain_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ChunkFilter evaluation tests ──────────────────────────

    fn meta(json: &str) -> serde_json::Value {
        serde_json::from_str(json).expect("fixture json")
    }

    #[test]
    fn chunk_filter_requires_metadata_reflects_declared_predicates() {
        let empty = ChunkFilter::default();
        assert!(!empty.requires_metadata());

        let with_in = ChunkFilter {
            metadata_in: vec![("skill_id".into(), vec!["x".into()])],
            ..Default::default()
        };
        assert!(with_in.requires_metadata());

        let with_compare = ChunkFilter {
            metadata_compare: vec![MetadataComparison {
                key: "confidence".into(),
                op: ComparisonOp::Gt,
                value: 0.7,
            }],
            ..Default::default()
        };
        assert!(with_compare.requires_metadata());

        let with_kv = ChunkFilter {
            metadata_key_values: vec![("source".into(), "manual".into())],
            ..Default::default()
        };
        assert!(with_kv.requires_metadata());
    }

    #[test]
    fn evaluate_metadata_in_or_within_key() {
        let f = ChunkFilter {
            metadata_in: vec![(
                "skill_id".into(),
                vec!["research-analyst".into(), "epistemic-research".into()],
            )],
            ..Default::default()
        };
        assert!(f.evaluate_metadata(&meta(r#"{"skill_id":"research-analyst"}"#)));
        assert!(f.evaluate_metadata(&meta(r#"{"skill_id":"epistemic-research"}"#)));
        assert!(!f.evaluate_metadata(&meta(r#"{"skill_id":"inner-work"}"#)));
        assert!(
            !f.evaluate_metadata(&meta(r#"{"other":"value"}"#)),
            "missing key fails closed"
        );
    }

    #[test]
    fn evaluate_metadata_multiple_in_groups_are_anded() {
        let f = ChunkFilter {
            metadata_in: vec![
                ("skill_id".into(), vec!["research".into()]),
                ("lang".into(), vec!["en".into(), "de".into()]),
            ],
            ..Default::default()
        };
        assert!(f.evaluate_metadata(&meta(r#"{"skill_id":"research","lang":"en"}"#)));
        assert!(f.evaluate_metadata(&meta(r#"{"skill_id":"research","lang":"de"}"#)));
        assert!(!f.evaluate_metadata(&meta(r#"{"skill_id":"research","lang":"fr"}"#)));
        assert!(!f.evaluate_metadata(&meta(r#"{"skill_id":"other","lang":"en"}"#)));
    }

    #[test]
    fn evaluate_metadata_comparison_gt_matches_numeric() {
        let f = ChunkFilter {
            metadata_compare: vec![MetadataComparison {
                key: "confidence".into(),
                op: ComparisonOp::Gt,
                value: 0.7,
            }],
            ..Default::default()
        };
        assert!(f.evaluate_metadata(&meta(r#"{"confidence":0.9}"#)));
        assert!(!f.evaluate_metadata(&meta(r#"{"confidence":0.7}"#)));
        assert!(!f.evaluate_metadata(&meta(r#"{"confidence":0.5}"#)));
    }

    #[test]
    fn evaluate_metadata_comparison_accepts_numeric_strings() {
        // The SqliteAcquirer emits non-text values as JSON strings.
        // The evaluator must parse them as f64 to keep the
        // domain-level predicates portable across acquirer shapes.
        let f = ChunkFilter {
            metadata_compare: vec![MetadataComparison {
                key: "confidence".into(),
                op: ComparisonOp::Gt,
                value: 0.7,
            }],
            ..Default::default()
        };
        assert!(f.evaluate_metadata(&meta(r#"{"confidence":"0.85"}"#)));
        assert!(!f.evaluate_metadata(&meta(r#"{"confidence":"0.6"}"#)));
        assert!(
            !f.evaluate_metadata(&meta(r#"{"confidence":"not-a-number"}"#)),
            "non-numeric fails closed"
        );
    }

    #[test]
    fn evaluate_metadata_all_comparison_ops() {
        use ComparisonOp::*;
        let cases: &[(ComparisonOp, f64, f64, bool)] = &[
            (Gt, 1.0, 0.5, true),
            (Gt, 1.0, 1.0, false),
            (Ge, 1.0, 1.0, true),
            (Lt, 0.5, 1.0, true),
            (Le, 1.0, 1.0, true),
            (Eq, 1.0, 1.0, true),
            (Eq, 1.0, 1.01, false),
            (Ne, 1.0, 2.0, true),
            (Ne, 1.0, 1.0, false),
        ];
        for (op, actual, threshold, expected) in cases {
            let f = ChunkFilter {
                metadata_compare: vec![MetadataComparison {
                    key: "x".into(),
                    op: *op,
                    value: *threshold,
                }],
                ..Default::default()
            };
            let json = format!(r#"{{"x":{actual}}}"#);
            assert_eq!(
                f.evaluate_metadata(&meta(&json)),
                *expected,
                "op {op:?} actual={actual} threshold={threshold}"
            );
        }
    }

    #[test]
    fn evaluate_metadata_compound_predicates() {
        // Covers the realistic domain-level case: "skill_id IN [...]
        // AND confidence > 0.7". All declared predicates must pass.
        let f = ChunkFilter {
            metadata_in: vec![("skill_id".into(), vec!["research".into()])],
            metadata_compare: vec![MetadataComparison {
                key: "confidence".into(),
                op: ComparisonOp::Gt,
                value: 0.7,
            }],
            ..Default::default()
        };
        assert!(f.evaluate_metadata(&meta(r#"{"skill_id":"research","confidence":0.9}"#)));
        assert!(
            !f.evaluate_metadata(&meta(r#"{"skill_id":"research","confidence":0.5}"#)),
            "skill matches but confidence fails → overall false"
        );
        assert!(
            !f.evaluate_metadata(&meta(r#"{"skill_id":"chat","confidence":0.9}"#)),
            "confidence matches but skill fails → overall false"
        );
    }

    #[test]
    fn evaluate_metadata_empty_filter_accepts_everything() {
        // Regression guard: the default-constructed filter must not
        // accidentally reject chunks. Legacy domains rely on this
        // because they set only length-based predicates.
        let f = ChunkFilter::default();
        assert!(f.evaluate_metadata(&meta(r#"{}"#)));
        assert!(f.evaluate_metadata(&meta(r#"{"anything":"goes"}"#)));
    }

    #[test]
    fn chunk_role_as_str_round_trip() {
        let roles = [
            (ChunkRole::Argument, "argument"),
            (ChunkRole::Objection, "objection"),
            (ChunkRole::Evidence, "evidence"),
            (ChunkRole::Historical, "historical"),
            (ChunkRole::Illustrative, "illustrative"),
            (ChunkRole::Definition, "definition"),
            (ChunkRole::OpenQuestion, "open_question"),
            (ChunkRole::NonArgumentative, "non_argumentative"),
        ];
        for (role, expected) in &roles {
            assert_eq!(role.as_str(), *expected);
        }
    }

    #[test]
    fn classify_chunk_role_argumentative() {
        // Use PhilosophyDomain to test the default classify_chunk_role.
        let domain = crate::enrichment::domains::philosophy::PhilosophyDomain;
        let label = ClusterLabel {
            topic: "compatibilism".into(),
            position_name: Some("Compatibilism".into()),
            is_argumentative: true,
            is_objection: false,
            is_open_question: false,
            is_coherent: true,
            domain_id: None,
        };
        assert_eq!(domain.classify_chunk_role(&label), ChunkRole::Argument);
    }

    #[test]
    fn classify_chunk_role_objection() {
        let domain = crate::enrichment::domains::philosophy::PhilosophyDomain;
        let label = ClusterLabel {
            topic: "critique".into(),
            position_name: None,
            is_argumentative: true,
            is_objection: true,
            is_open_question: false,
            is_coherent: true,
            domain_id: None,
        };
        assert_eq!(domain.classify_chunk_role(&label), ChunkRole::Objection);
    }

    #[test]
    fn classify_chunk_role_open_question() {
        let domain = crate::enrichment::domains::philosophy::PhilosophyDomain;
        let label = ClusterLabel {
            topic: "unresolved".into(),
            position_name: None,
            is_argumentative: true,
            is_objection: false,
            is_open_question: true,
            is_coherent: true,
            domain_id: None,
        };
        assert_eq!(domain.classify_chunk_role(&label), ChunkRole::OpenQuestion);
    }

    #[test]
    fn classify_chunk_role_non_argumentative() {
        let domain = crate::enrichment::domains::philosophy::PhilosophyDomain;
        let label = ClusterLabel {
            topic: "definitions".into(),
            position_name: None,
            is_argumentative: false,
            is_objection: false,
            is_open_question: false,
            is_coherent: true,
            domain_id: None,
        };
        assert_eq!(
            domain.classify_chunk_role(&label),
            ChunkRole::NonArgumentative
        );
    }

    #[test]
    fn cluster_label_json_round_trip() {
        let label = ClusterLabel {
            topic: "free will".into(),
            position_name: Some("Compatibilism".into()),
            is_argumentative: true,
            is_objection: false,
            is_open_question: false,
            is_coherent: true,
            domain_id: Some("philosophy".into()),
        };
        let json = serde_json::to_string(&label).unwrap();
        let parsed: ClusterLabel = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.topic, "free will");
        assert_eq!(parsed.position_name.as_deref(), Some("Compatibilism"));
        assert!(parsed.is_argumentative);
        assert_eq!(parsed.domain_id.as_deref(), Some("philosophy"));
    }

    #[test]
    fn cluster_label_missing_domain_defaults_to_none() {
        let json = r#"{"topic":"test","position_name":null,"is_argumentative":true,"is_objection":false,"is_open_question":false,"is_coherent":true}"#;
        let label: ClusterLabel = serde_json::from_str(json).unwrap();
        assert!(label.domain_id.is_none());
    }

    #[test]
    fn chunk_role_serde_round_trip() {
        let role = ChunkRole::Argument;
        let json = serde_json::to_string(&role).unwrap();
        let parsed: ChunkRole = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, role);
    }
}
