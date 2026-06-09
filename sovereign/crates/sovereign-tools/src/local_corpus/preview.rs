// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build the `VaultPreview` the UI renders on the Organize screen.
//!
//! Pure translation layer: takes a `LabeledClusterResult` plus the
//! corpus index (for chunk titles / bodies) and returns a structure
//! the Tauri command can serialise straight to TS. No LLM calls, no
//! writes — the user sees exactly this preview and can approve or
//! cancel.
//!
//! Outlier classification follows spec §6.4:
//!   - `LowConfidence`: best-cluster confidence < `min_confidence`.
//!   - `AmbiguousCluster`: top two clusters within `0.1` of each
//!     other (heuristic in the spec). Treated as low-priority for
//!     v1; lands alongside LowConfidence in the outlier panel.
//!   - `TooShort`: body under 80 characters; too little signal to
//!     cluster usefully.

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine::CorpusEngine;
use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};

use super::clusterer::{ClusterConfig, LabeledCluster, LabeledClusterResult, OpenQuestion};

// ─── Public output types ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPreview {
    pub clusters: Vec<ClusterSummary>,
    pub outliers: Vec<OutlierNote>,
    /// Notes that would get flagged under the `Flag` multi-cluster
    /// strategy. Empty for v1 (`Dominant` is always active).
    pub flagged: Vec<FlaggedNote>,
    pub total_notes: usize,
    pub tagged_notes: usize,
    pub outlier_count: usize,
    pub open_questions: Vec<OpenQuestion>,
    /// The namespace every `primary_tag` / `additional_tag` is
    /// prefixed with. Always `"sovereign"` for v1; here so the UI
    /// can render it without hardcoding.
    pub namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSummary {
    pub cluster: LabeledCluster,
    pub assignments: Vec<FileAssignment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAssignment {
    pub chunk_id: u64,
    pub relative_path: String,
    pub note_title: String,
    pub primary_tag: String,
    pub additional_tags: Vec<String>,
    pub confidence: f32,
    pub existing_tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlierNote {
    pub chunk_id: u64,
    pub relative_path: String,
    pub note_title: String,
    pub best_cluster_id: i32,
    pub best_cluster_confidence: f32,
    pub reason: OutlierReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutlierReason {
    LowConfidence {
        threshold: f32,
    },
    AmbiguousCluster {
        top_clusters: Vec<ClusterConfidence>,
    },
    TooShort {
        char_count: usize,
    },
    /// The note's best-matching cluster ended up with fewer than
    /// `min_notes_per_cluster` notes after the chunk-to-note rollup,
    /// so we don't tag it on its own. `cluster_size` is how many
    /// notes were in that collapsed cluster.
    SingletonCluster {
        cluster_size: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfidence {
    pub cluster_id: i32,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaggedNote {
    pub chunk_id: u64,
    pub note_title: String,
    pub candidate_clusters: Vec<ClusterConfidence>,
}

// ─── Builder ─────────────────────────────────────────────────────────

const NAMESPACE: &str = "sovereign";
const MIN_CHARS_TO_CLUSTER: usize = 80;

/// Shape a `LabeledClusterResult` + live corpus index into a
/// `VaultPreview`. Fetches chunk bodies in a single `get_chunks` call
/// to minimise LanceDB round-trips.
///
/// **Note-level rollup.** The engine clusters chunks, but the user
/// thinks in notes. A single Obsidian note typically produces many
/// chunks (one per H2/H3 section); each chunk is independently
/// assigned to a cluster with its own confidence. Before we hand the
/// preview to the UI we roll up chunks → notes:
///
///   - Each note's cluster is the cluster of its highest-confidence
///     chunk. (Majority-vote would be more robust but also more
///     surprising — "why is the note with only one weak chunk in this
///     cluster" — so we take the peak passage as the representative.)
///   - Each note's confidence is that peak chunk's confidence.
///   - Each note appears at most once across the whole preview: either
///     as an assignment under one cluster, or in the outlier panel.
///
/// Without this rollup, the cluster-detail panel AND the
/// `_sovereign-index/<tag>.md` Map-of-Content note render duplicate
/// rows for every multi-chunk note — confusing and noisy.
pub async fn build_preview(
    engine: Arc<CorpusEngine>,
    corpus_id: &str,
    config: &ClusterConfig,
    result: &LabeledClusterResult,
) -> Result<VaultPreview> {
    let index = engine
        .open_index_for_corpus(corpus_id)
        .await
        .map_err(|e| Error::Execution(format!("open index '{corpus_id}': {e}")))?;

    let all_chunk_ids: Vec<u64> = result.chunk_assignments.keys().copied().collect();
    let chunks = index
        .get_chunks(&all_chunk_ids)
        .await
        .map_err(|e| Error::Execution(format!("get_chunks: {e}")))?;

    // Index by id for O(1) lookup during assembly.
    let chunk_by_id: HashMap<u64, &corpus_engine::StoredChunk> =
        chunks.iter().map(|c| (c.id, c)).collect();

    let cluster_by_id: HashMap<i32, &LabeledCluster> =
        result.clusters.iter().map(|c| (c.id, c)).collect();

    // ── Pass 1: collect per-note best chunk ──────────────────────
    //
    // For each distinct note_title we remember the chunk with the
    // highest confidence (across all its assignments — the "peak
    // passage"). If that peak chunk was HDBSCAN noise, we resolve
    // its *nearest* cluster from `noise_best_cluster` and use that
    // as the note's effective cluster. This is the fix for the
    // demo bug where noise notes stayed in the outlier panel
    // regardless of how low the user dragged `min_confidence`.
    #[derive(Clone)]
    struct NoteBest {
        chunk_id: u64,
        effective_cluster: i32, // `-1` only when there are zero clusters at all
        confidence: f32,
        note_title: String,
        char_count: usize,
    }
    let effective_cluster_of = |chunk_id: u64, assigned: i32| -> i32 {
        if assigned >= 0 {
            assigned
        } else {
            result
                .noise_best_cluster
                .get(&chunk_id)
                .copied()
                .unwrap_or(-1)
        }
    };

    let mut best_by_note: HashMap<String, NoteBest> = HashMap::new();
    for (chunk_id, cluster_id) in &result.chunk_assignments {
        let Some(chunk) = chunk_by_id.get(chunk_id) else {
            continue;
        };
        let note_title = chunk
            .title
            .clone()
            .unwrap_or_else(|| format!("chunk-{chunk_id}"));
        let confidence = result
            .chunk_confidences
            .get(chunk_id)
            .copied()
            .unwrap_or(0.0);
        let char_count = chunk.content.chars().count();
        let effective = effective_cluster_of(*chunk_id, *cluster_id);

        let entry = best_by_note
            .entry(note_title.clone())
            .or_insert_with(|| NoteBest {
                chunk_id: *chunk_id,
                effective_cluster: effective,
                confidence,
                note_title: note_title.clone(),
                char_count,
            });
        if confidence > entry.confidence {
            entry.chunk_id = *chunk_id;
            entry.effective_cluster = effective;
            entry.confidence = confidence;
            entry.char_count = entry.char_count.max(char_count);
        } else {
            entry.char_count = entry.char_count.max(char_count);
        }
    }

    // ── Pass 2: provisional classification (before singleton filter) ──
    let total_notes = best_by_note.len();
    let mut cluster_assignments: HashMap<i32, Vec<FileAssignment>> = HashMap::new();
    let mut outliers: Vec<OutlierNote> = Vec::new();

    for note in best_by_note.values() {
        let relative_path = note.note_title.clone(); // M4b: real source_path
        if note.char_count < MIN_CHARS_TO_CLUSTER {
            outliers.push(OutlierNote {
                chunk_id: note.chunk_id,
                relative_path,
                note_title: note.note_title.clone(),
                best_cluster_id: note.effective_cluster,
                best_cluster_confidence: note.confidence,
                reason: OutlierReason::TooShort {
                    char_count: note.char_count,
                },
            });
            continue;
        }
        if note.effective_cluster < 0 || note.confidence < config.min_confidence {
            outliers.push(OutlierNote {
                chunk_id: note.chunk_id,
                relative_path,
                note_title: note.note_title.clone(),
                best_cluster_id: note.effective_cluster,
                best_cluster_confidence: note.confidence,
                reason: OutlierReason::LowConfidence {
                    threshold: config.min_confidence,
                },
            });
            continue;
        }

        let primary_tag = cluster_by_id
            .get(&note.effective_cluster)
            .map(|c| format!("{NAMESPACE}/{}", c.tag_path))
            .unwrap_or_else(|| {
                format!(
                    "{NAMESPACE}/uncategorized/cluster-{}",
                    note.effective_cluster
                )
            });

        cluster_assignments
            .entry(note.effective_cluster)
            .or_default()
            .push(FileAssignment {
                chunk_id: note.chunk_id,
                relative_path,
                note_title: note.note_title.clone(),
                primary_tag,
                additional_tags: Vec::new(),
                confidence: note.confidence,
                existing_tags: Vec::new(),
            });
    }

    // ── Pass 3: singleton-cluster filter ───────────────────────────
    //
    // A cluster with fewer than `min_notes_per_cluster` distinct
    // notes after rollup is not worth a tag of its own — the user
    // described these as "premature tagging". Collapse those
    // clusters: their notes become outliers with reason
    // `SingletonCluster`, and the cluster itself disappears from
    // the preview entirely (no tag_path, no MoC note).
    let min_notes = config.min_notes_per_cluster.max(1);
    let doomed: Vec<i32> = cluster_assignments
        .iter()
        .filter(|(_, notes)| notes.len() < min_notes)
        .map(|(cid, _)| *cid)
        .collect();
    for cid in doomed {
        if let Some(notes) = cluster_assignments.remove(&cid) {
            let cluster_size = notes.len();
            for a in notes {
                outliers.push(OutlierNote {
                    chunk_id: a.chunk_id,
                    relative_path: a.relative_path,
                    note_title: a.note_title,
                    best_cluster_id: cid,
                    best_cluster_confidence: a.confidence,
                    reason: OutlierReason::SingletonCluster { cluster_size },
                });
            }
        }
    }

    // Sort assignments within each cluster by descending confidence
    // so the UI's top rows are the clearest exemplars.
    for list in cluster_assignments.values_mut() {
        list.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    outliers.sort_by(|a, b| {
        b.best_cluster_confidence
            .partial_cmp(&a.best_cluster_confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Assemble cluster summaries in note-count-descending order —
    // big clusters first, a natural reading flow. Clusters that
    // survived the singleton filter (i.e. still have entries in
    // `cluster_assignments`) are the ones we render; the rest are
    // dropped so the UI doesn't show empty-looking cluster cards.
    let mut summaries: Vec<ClusterSummary> = result
        .clusters
        .iter()
        .filter_map(|c| {
            cluster_assignments
                .remove(&c.id)
                .filter(|notes| !notes.is_empty())
                .map(|notes| ClusterSummary {
                    cluster: c.clone(),
                    assignments: notes,
                })
        })
        .collect();
    summaries.sort_by(|a, b| b.assignments.len().cmp(&a.assignments.len()));

    let tagged_notes: usize = summaries.iter().map(|s| s.assignments.len()).sum();
    let outlier_count = outliers.len();

    Ok(VaultPreview {
        clusters: summaries,
        outliers,
        flagged: Vec::new(),
        total_notes,
        tagged_notes,
        outlier_count,
        open_questions: result.open_questions.clone(),
        namespace: NAMESPACE.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_corpus::clusterer::{LabeledClusterResult, MultiClusterStrategy};

    #[test]
    fn outlier_reason_serialises_with_tag() {
        // The TS type union depends on `#[serde(tag = "type")]` so
        // freezing the serialised shape prevents silent schema drift.
        let r = OutlierReason::TooShort { char_count: 42 };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"too_short""#), "got: {s}");
        assert!(s.contains(r#""char_count":42"#), "got: {s}");
    }

    #[test]
    fn outlier_reason_low_confidence_shape() {
        let r = OutlierReason::LowConfidence { threshold: 0.4 };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"low_confidence""#));
        assert!(s.contains(r#""threshold":0.4"#));
    }

    // The note-level rollup logic is the core of the preview shape
    // but lives inside `build_preview` which needs a live
    // `CorpusIndex`. To keep these tests hermetic we factor the
    // classification through a synthetic helper that mirrors the
    // live path's decisions, asserting the behaviour we rely on.
    //
    // If these drift from `build_preview`, the end-to-end test
    // `vault_write_rollback_clean_round_trip` catches the wire-up
    // mismatch.

    fn cluster_config(min_conf: f32, min_notes_per_cluster: usize) -> ClusterConfig {
        ClusterConfig {
            min_cluster_size: 5,
            min_confidence: min_conf,
            multi_tag_threshold: 0.6,
            multi_cluster_strategy: MultiClusterStrategy::Dominant,
            min_notes_per_cluster,
        }
    }

    /// One chunk row in the test helper. `cluster_id = -1` means
    /// "HDBSCAN noise"; the optional `noise_best` is the nearest
    /// cluster the preview should promote it to when the threshold
    /// is low enough.
    struct Chunk {
        title: &'static str,
        cluster_id: i32,
        noise_best: Option<i32>,
        confidence: f32,
        char_count: usize,
    }

    /// Pure test helper: replicates the rollup + singleton math with
    /// in-memory notes so we can assert invariants without a LanceDB.
    /// Keep in sync with `build_preview` — the end-to-end test
    /// `vault_write_rollback_clean_round_trip` catches wire-up drift.
    fn rollup_preview(
        chunks: &[Chunk],
        min_conf: f32,
        min_notes_per_cluster: usize,
    ) -> (
        Vec<(i32, Vec<(String, f32)>)>,
        Vec<(String, f32, &'static str)>,
    ) {
        use std::collections::BTreeMap;
        let config = cluster_config(min_conf, min_notes_per_cluster);
        // Type-level smoke — ensures the helper keeps pace with the
        // shape of the real result struct.
        let _ = LabeledClusterResult {
            clusters: vec![],
            chunk_assignments: Default::default(),
            chunk_confidences: Default::default(),
            noise_best_cluster: Default::default(),
            noise_chunks: vec![],
            open_questions: vec![],
        };

        let effective = |c: &Chunk| -> i32 {
            if c.cluster_id >= 0 {
                c.cluster_id
            } else {
                c.noise_best.unwrap_or(-1)
            }
        };

        // Pass 1: per-note best.
        let mut best: BTreeMap<String, (i32, f32, usize)> = BTreeMap::new();
        for c in chunks {
            let e = best.entry(c.title.to_string()).or_insert((
                effective(c),
                c.confidence,
                c.char_count,
            ));
            if c.confidence > e.1 {
                *e = (effective(c), c.confidence, e.2.max(c.char_count));
            } else {
                e.2 = e.2.max(c.char_count);
            }
        }

        // Pass 2: provisional classify.
        let mut assigned: BTreeMap<i32, Vec<(String, f32)>> = BTreeMap::new();
        let mut outliers: Vec<(String, f32, &'static str)> = Vec::new();
        for (title, (cid, conf, cc)) in best {
            if cc < MIN_CHARS_TO_CLUSTER {
                outliers.push((title, conf, "too_short"));
            } else if cid < 0 || conf < config.min_confidence {
                outliers.push((title, conf, "low_confidence"));
            } else {
                assigned.entry(cid).or_default().push((title, conf));
            }
        }

        // Pass 3: singleton filter.
        let min_n = config.min_notes_per_cluster.max(1);
        let doomed: Vec<i32> = assigned
            .iter()
            .filter(|(_, v)| v.len() < min_n)
            .map(|(k, _)| *k)
            .collect();
        for cid in doomed {
            if let Some(rows) = assigned.remove(&cid) {
                for (t, conf) in rows {
                    outliers.push((t, conf, "singleton_cluster"));
                }
            }
        }

        (assigned.into_iter().collect(), outliers)
    }

    // Back-compat shim for the original tests that predate the new
    // helper signature. They don't exercise the new features, so a
    // shim is cleaner than rewriting each call site.
    fn legacy_rollup(
        notes: &[(&str, i32, f32, usize)],
        min_conf: f32,
    ) -> (
        Vec<(i32, Vec<(String, f32)>)>,
        Vec<(String, f32, &'static str)>,
    ) {
        let chunks: Vec<Chunk> = notes
            .iter()
            .map(|(t, cid, conf, cc)| Chunk {
                title: Box::leak((*t).to_string().into_boxed_str()),
                cluster_id: *cid,
                noise_best: None,
                confidence: *conf,
                char_count: *cc,
            })
            .collect();
        rollup_preview(&chunks, min_conf, 1)
    }

    #[test]
    fn rollup_collapses_multiple_chunks_into_one_note() {
        // Five chunks from the same note `Stock Buybacks`: all land
        // in cluster 7, confidences 0.91, 0.87, 0.87, 0.83, 0.79.
        // Expected: ONE assignment, confidence 0.91.
        let notes = vec![
            ("Stock Buybacks", 7, 0.91, 800),
            ("Stock Buybacks", 7, 0.87, 800),
            ("Stock Buybacks", 7, 0.87, 800),
            ("Stock Buybacks", 7, 0.83, 800),
            ("Stock Buybacks", 7, 0.79, 800),
        ];
        let (assigned, outliers) = legacy_rollup(&notes, 0.4);
        assert_eq!(assigned.len(), 1);
        let (cid, rows) = &assigned[0];
        assert_eq!(*cid, 7);
        assert_eq!(rows.len(), 1, "single note should collapse to one row");
        assert_eq!(rows[0].0, "Stock Buybacks");
        assert!((rows[0].1 - 0.91).abs() < 1e-6, "peak confidence = 0.91");
        assert!(outliers.is_empty());
    }

    #[test]
    fn rollup_note_wins_cluster_of_its_peak_chunk() {
        // Same note has chunks in two different clusters — whichever
        // cluster contains the peak-confidence chunk owns the note.
        let notes = vec![
            ("Dual Topic Note", 1, 0.55, 400),
            ("Dual Topic Note", 2, 0.81, 400),
            ("Dual Topic Note", 1, 0.62, 400),
        ];
        let (assigned, _) = legacy_rollup(&notes, 0.4);
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].0, 2, "cluster of the 0.81 chunk wins");
        assert_eq!(assigned[0].1.len(), 1);
    }

    #[test]
    fn rollup_outlier_is_listed_once() {
        // A note whose best chunk is below threshold → one outlier
        // entry with its peak confidence, not one outlier per chunk.
        let notes = vec![
            ("Loose Draft", 3, 0.32, 400),
            ("Loose Draft", 3, 0.25, 400),
            ("Loose Draft", -1, 0.10, 400),
        ];
        let (assigned, outliers) = legacy_rollup(&notes, 0.4);
        assert!(assigned.is_empty());
        assert_eq!(outliers.len(), 1);
        assert_eq!(outliers[0].0, "Loose Draft");
        assert!((outliers[0].1 - 0.32).abs() < 1e-6);
        assert_eq!(outliers[0].2, "low_confidence");
    }

    #[test]
    fn threshold_tweak_moves_note_between_assigned_and_outlier() {
        // A note at peak confidence 0.38 — at threshold 0.4 it's an
        // outlier; drop the threshold to 0.35 and it flips into the
        // cluster. This is the behaviour the UI slider relies on.
        let notes = vec![("Stock Buybacks", 5, 0.38, 600)];

        let (assigned_strict, outliers_strict) = legacy_rollup(&notes, 0.40);
        assert!(assigned_strict.is_empty());
        assert_eq!(outliers_strict.len(), 1);

        let (assigned_loose, outliers_loose) = legacy_rollup(&notes, 0.35);
        assert!(outliers_loose.is_empty());
        assert_eq!(assigned_loose.len(), 1);
        assert_eq!(assigned_loose[0].1.len(), 1);
    }

    #[test]
    fn too_short_uses_longest_chunk_not_shortest() {
        // Note with a tiny chunk (10 chars) and a real chunk (500
        // chars) must not be classified as TooShort — the rollup
        // takes the max char_count.
        let notes = vec![
            ("Mixed Length", 4, 0.70, 10),
            ("Mixed Length", 4, 0.70, 500),
        ];
        let (assigned, outliers) = legacy_rollup(&notes, 0.4);
        assert_eq!(assigned.len(), 1);
        assert!(outliers.is_empty());
    }

    // ─── Regression tests for the two M5.1 demo bugs ────────────

    #[test]
    fn noise_chunk_with_strong_match_promotes_when_threshold_drops() {
        // This is the "Time on Screens" case: HDBSCAN marked the
        // chunk as noise but it's 77% close to cluster 9's centroid.
        // At the default 0.4 threshold it should already be
        // promoted to cluster 9 (not a permanent outlier).
        let chunks = vec![Chunk {
            title: "Time on Screens",
            cluster_id: -1,
            noise_best: Some(9),
            confidence: 0.77,
            char_count: 800,
        }];

        // Threshold 0.4 → promoted to cluster 9. This is the bug:
        // previously `cluster_id < 0` short-circuited before the
        // threshold check and the note stayed an outlier forever.
        let (assigned_040, outliers_040) = rollup_preview(&chunks, 0.40, 1);
        assert_eq!(
            assigned_040.len(),
            1,
            "noise chunk above threshold should be promoted"
        );
        assert_eq!(assigned_040[0].0, 9);
        assert!(outliers_040.is_empty());

        // Drag threshold up to 0.85 — now it legitimately outlies.
        let (assigned_085, outliers_085) = rollup_preview(&chunks, 0.85, 1);
        assert!(assigned_085.is_empty());
        assert_eq!(outliers_085.len(), 1);
        assert_eq!(outliers_085[0].2, "low_confidence");
    }

    #[test]
    fn noise_chunk_without_any_clusters_stays_outlier() {
        // Sanity: if HDBSCAN produced zero clusters (`noise_best`
        // is None because there's nowhere to promote to), noise
        // chunks remain outliers regardless of threshold.
        let chunks = vec![Chunk {
            title: "Orphan",
            cluster_id: -1,
            noise_best: None,
            confidence: 0.99,
            char_count: 400,
        }];
        let (assigned, outliers) = rollup_preview(&chunks, 0.0, 1);
        assert!(assigned.is_empty());
        assert_eq!(outliers.len(), 1);
    }

    #[test]
    fn singleton_cluster_filter_bumps_solo_note_to_outlier() {
        // A cluster with only one distinct note after rollup is
        // collapsed: that note becomes a SingletonCluster outlier.
        // Captures the demo complaint that a single note
        // shouldn't get its own tag.
        let chunks = vec![
            Chunk {
                title: "Unique Note",
                cluster_id: 42,
                noise_best: None,
                confidence: 0.9,
                char_count: 600,
            },
            Chunk {
                title: "Big Cluster A",
                cluster_id: 1,
                noise_best: None,
                confidence: 0.8,
                char_count: 600,
            },
            Chunk {
                title: "Big Cluster B",
                cluster_id: 1,
                noise_best: None,
                confidence: 0.75,
                char_count: 600,
            },
        ];

        let (assigned, outliers) = rollup_preview(&chunks, 0.4, 2);
        // Cluster 42 collapses; cluster 1 survives.
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].0, 1);
        assert_eq!(assigned[0].1.len(), 2);
        // `Unique Note` lands in outliers with the singleton reason.
        let singleton: Vec<_> = outliers
            .iter()
            .filter(|o| o.2 == "singleton_cluster")
            .collect();
        assert_eq!(singleton.len(), 1);
        assert_eq!(singleton[0].0, "Unique Note");
    }

    #[test]
    fn singleton_filter_respects_min_of_one() {
        // min_notes_per_cluster = 1 leaves everything as-is (the
        // M4-era behaviour). Used by the back-compat legacy shim.
        let chunks = vec![Chunk {
            title: "Lone Ranger",
            cluster_id: 3,
            noise_best: None,
            confidence: 0.9,
            char_count: 400,
        }];
        let (assigned, outliers) = rollup_preview(&chunks, 0.4, 1);
        assert_eq!(assigned.len(), 1);
        assert!(outliers.is_empty());
    }
}
