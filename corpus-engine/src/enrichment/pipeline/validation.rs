// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas traversal + validation battery.
//!
//! Landing 4 piece: `Atlas::from_cache_dir` reads phase 3/5/6/7 caches
//! off disk into the consolidated `Atlas`, and `run_battery` walks a
//! `QueryBattery` against the atlas to produce per-question scores the
//! `sovereign enrich validate` CLI renders.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::phase_cache::PhaseCache;
use super::types::{
    Atlas, CanonicalConcern, Phase3Output, Phase5Output, Phase6Output, Phase7Output, PipelinePhase,
    Position, Tension,
};
use crate::error::{Error, Result};
use crate::types::EmbedFn;

impl Atlas {
    /// Load the consolidated atlas from a phase-cache directory.
    /// Missing caches degrade gracefully: missing phase 7 is fine
    /// (no gaps), missing phase 5 means an empty positions set, etc.
    /// Phase 3 is required — without canonical concerns the atlas
    /// has nothing to traverse.
    pub fn from_cache_dir(cache_dir: &Path) -> Result<Self> {
        let cache = PhaseCache::new(cache_dir);
        let concerns: Phase3Output = cache.read(PipelinePhase::Concerns)?.ok_or_else(|| {
            Error::InvalidInput("atlas cannot be built: phase 3 (concerns) cache is missing".into())
        })?;
        let positions: Phase5Output =
            cache
                .read(PipelinePhase::Positions)?
                .unwrap_or_else(|| Phase5Output {
                    schema_version: Phase5Output::SCHEMA_VERSION,
                    pipeline_id: concerns.pipeline_id.clone(),
                    positions: Vec::new(),
                    failures: Vec::new(),
                    written_at: String::new(),
                });
        let tensions: Phase6Output =
            cache
                .read(PipelinePhase::Tensions)?
                .unwrap_or_else(|| Phase6Output {
                    schema_version: Phase6Output::SCHEMA_VERSION,
                    pipeline_id: concerns.pipeline_id.clone(),
                    tensions: Vec::new(),
                    failures: Vec::new(),
                    written_at: String::new(),
                });
        let gaps: Phase7Output = cache
            .read(PipelinePhase::Gaps)?
            .unwrap_or_else(|| Phase7Output {
                schema_version: Phase7Output::SCHEMA_VERSION,
                pipeline_id: concerns.pipeline_id.clone(),
                gaps: Vec::new(),
                failures: Vec::new(),
                written_at: String::new(),
            });
        Ok(Self {
            concerns: concerns.concerns,
            positions: positions.positions,
            tensions: tensions.tensions,
            gaps: gaps.gaps,
        })
    }
}

// ── Query traversal ──────────────────────────────────────────

/// One query resolved against the atlas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryTraversal {
    pub query: String,
    pub locate: Vec<ConcernMatch>,
    pub positions: Vec<PositionRef>,
    pub tensions: Vec<TensionRef>,
    pub grounding_chunk_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcernMatch {
    pub concern_id: String,
    pub concern_text: String,
    pub similarity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionRef {
    pub position_id: String,
    pub concern_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TensionRef {
    pub tension_id: String,
    pub position_a_id: String,
    pub position_b_id: String,
    pub description: String,
}

/// Walk the atlas for a single query.
///
/// - Embeds the query and scores every concern by cosine similarity.
/// - Keeps concerns above `min_similarity` OR the single best match if
///   none clear that bar (so we never return empty LOCATE blocks).
/// - Pulls positions + tensions + grounding chunk ids from the
///   matched concerns.
pub async fn traverse_atlas(
    atlas: &Atlas,
    query: &str,
    embed: &EmbedFn,
    min_similarity: f32,
) -> Result<QueryTraversal> {
    if atlas.concerns.is_empty() {
        return Ok(QueryTraversal {
            query: query.to_string(),
            locate: Vec::new(),
            positions: Vec::new(),
            tensions: Vec::new(),
            grounding_chunk_ids: Vec::new(),
        });
    }

    let q_emb = embed(query).await?;
    let concern_embs: Vec<Vec<f32>> = {
        let mut out = Vec::with_capacity(atlas.concerns.len());
        for c in &atlas.concerns {
            out.push(embed(&c.concern_text).await?);
        }
        out
    };

    let mut scored: Vec<(f32, &CanonicalConcern)> = atlas
        .concerns
        .iter()
        .zip(concern_embs.iter())
        .map(|(c, e)| (cosine(&q_emb, e), c))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let above: Vec<&(f32, &CanonicalConcern)> = scored
        .iter()
        .filter(|(s, _)| *s >= min_similarity)
        .collect();
    let locate: Vec<ConcernMatch> = if !above.is_empty() {
        above
            .iter()
            .map(|(s, c)| ConcernMatch {
                concern_id: c.id.clone(),
                concern_text: c.concern_text.clone(),
                similarity: *s,
            })
            .collect()
    } else if let Some((s, c)) = scored.first() {
        vec![ConcernMatch {
            concern_id: c.id.clone(),
            concern_text: c.concern_text.clone(),
            similarity: *s,
        }]
    } else {
        Vec::new()
    };

    // Positions + tensions + grounding from the LOCATE set.
    let locate_ids: std::collections::HashSet<&str> =
        locate.iter().map(|m| m.concern_id.as_str()).collect();
    let positions: Vec<&Position> = atlas
        .positions
        .iter()
        .filter(|p| locate_ids.contains(p.concern_id.as_str()))
        .collect();
    let tensions: Vec<&Tension> = atlas
        .tensions
        .iter()
        .filter(|t| {
            let in_set = |id: &str| positions.iter().any(|p| p.id == id);
            in_set(&t.position_a_id) && in_set(&t.position_b_id)
        })
        .collect();
    let mut grounding_chunk_ids: Vec<u64> = positions
        .iter()
        .flat_map(|p| p.grounding.iter().map(|g| g.chunk_id))
        .collect();
    grounding_chunk_ids.sort_unstable();
    grounding_chunk_ids.dedup();

    Ok(QueryTraversal {
        query: query.to_string(),
        locate,
        positions: positions
            .iter()
            .map(|p| PositionRef {
                position_id: p.id.clone(),
                concern_id: p.concern_id.clone(),
                text: p.position_text.clone(),
            })
            .collect(),
        tensions: tensions
            .iter()
            .map(|t| TensionRef {
                tension_id: t.id.clone(),
                position_a_id: t.position_a_id.clone(),
                position_b_id: t.position_b_id.clone(),
                description: t.description.clone(),
            })
            .collect(),
        grounding_chunk_ids,
    })
}

// ── Validation battery ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryBattery {
    #[serde(default = "default_battery_version")]
    pub schema_version: u32,
    pub questions: Vec<ValidationQuestion>,
}

fn default_battery_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationQuestion {
    /// 1-based ordinal or user-chosen short id.
    pub id: String,
    pub question: String,
}

impl QueryBattery {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        // Accept either `{"questions":[…]}` or a bare array of strings
        // (convenient for quick dev files).
        if let Ok(qs) = serde_json::from_str::<Vec<String>>(&raw) {
            return Ok(QueryBattery {
                schema_version: 1,
                questions: qs
                    .into_iter()
                    .enumerate()
                    .map(|(i, q)| ValidationQuestion {
                        id: format!("q_{:02}", i + 1),
                        question: q,
                    })
                    .collect(),
            });
        }
        serde_json::from_str(&raw).map_err(|e| {
            Error::Serialization(format!(
                "validation battery {} parse error: {}",
                path.display(),
                e
            ))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatteryRow {
    pub question_id: String,
    pub question: String,
    pub top_match_similarity: f32,
    pub concern_matches: usize,
    pub positions: usize,
    pub tensions: usize,
    pub grounding_passages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatteryResult {
    pub rows: Vec<BatteryRow>,
}

impl BatteryResult {
    /// Fraction of rows where the top concern match's similarity is
    /// ≥ threshold. Used by `validate` to produce a headline score.
    pub fn pass_rate(&self, threshold: f32) -> f32 {
        if self.rows.is_empty() {
            return 0.0;
        }
        let pass = self
            .rows
            .iter()
            .filter(|r| r.top_match_similarity >= threshold)
            .count();
        pass as f32 / self.rows.len() as f32
    }
}

pub async fn run_battery(
    battery: &QueryBattery,
    atlas: &Atlas,
    embed: &EmbedFn,
    min_similarity: f32,
) -> Result<BatteryResult> {
    let mut rows = Vec::with_capacity(battery.questions.len());
    for q in &battery.questions {
        let t = traverse_atlas(atlas, &q.question, embed, min_similarity).await?;
        let top = t.locate.first().map(|m| m.similarity).unwrap_or(0.0);
        rows.push(BatteryRow {
            question_id: q.id.clone(),
            question: q.question.clone(),
            top_match_similarity: top,
            concern_matches: t.locate.len(),
            positions: t.positions.len(),
            tensions: t.tensions.len(),
            grounding_passages: t.grounding_chunk_ids.len(),
        });
    }
    Ok(BatteryResult { rows })
}

// ── Helpers ──────────────────────────────────────────────────

fn cosine(a: &[f32], b: &[f32]) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::super::types::{Grounding, Position, Tension};
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn sample_atlas() -> Atlas {
        Atlas {
            concerns: vec![
                CanonicalConcern {
                    id: "cc_01".into(),
                    cluster_id: "qc_01".into(),
                    concern_text: "Can authentic feeling survive social reality?".into(),
                    scope: Some("novel-wide".into()),
                    primary_arcs: vec!["Anna-Vronsky".into()],
                },
                CanonicalConcern {
                    id: "cc_02".into(),
                    cluster_id: "qc_02".into(),
                    concern_text: "Can meaning be found through physical labor?".into(),
                    scope: Some("novel-wide".into()),
                    primary_arcs: vec!["Levin".into()],
                },
            ],
            positions: vec![
                Position {
                    id: "pos_01".into(),
                    concern_id: "cc_01".into(),
                    chunk_cluster_id: "kc_01".into(),
                    position_text: "Anna's trajectory…".into(),
                    grounding: vec![Grounding {
                        chunk_id: 100,
                        section_id: "sec_0008".into(),
                        summary: "s".into(),
                    }],
                    extensions: HashMap::new(),
                },
                Position {
                    id: "pos_02".into(),
                    concern_id: "cc_01".into(),
                    chunk_cluster_id: "kc_02".into(),
                    position_text: "Karenin's duty…".into(),
                    grounding: vec![Grounding {
                        chunk_id: 200,
                        section_id: "sec_0017".into(),
                        summary: "s".into(),
                    }],
                    extensions: HashMap::new(),
                },
                Position {
                    id: "pos_03".into(),
                    concern_id: "cc_02".into(),
                    chunk_cluster_id: "kc_03".into(),
                    position_text: "Levin's mowing…".into(),
                    grounding: vec![Grounding {
                        chunk_id: 300,
                        section_id: "sec_0012".into(),
                        summary: "s".into(),
                    }],
                    extensions: HashMap::new(),
                },
            ],
            tensions: vec![Tension {
                id: "t_01".into(),
                position_a_id: "pos_01".into(),
                position_b_id: "pos_02".into(),
                description: "parallel contrast".into(),
                specific_disagreement: None,
                structural_type: Some("parallel_contrast".into()),
            }],
            gaps: Vec::new(),
        }
    }

    /// Deterministic embed that scans the entire text for topical
    /// keywords. We pick the FIRST keyword found so "Can authentic…"
    /// and "Can meaning…" fall into different buckets (both have
    /// "Can" but differ on the next topical word).
    fn keyword_embed() -> EmbedFn {
        Arc::new(move |text: &str| {
            let lower = text.to_ascii_lowercase();
            let authentic_keys = ["authentic", "social", "love", "anna", "karenin", "feeling"];
            let meaning_keys = ["meaning", "labor", "levin", "mowing", "physical"];
            let mut auth_idx = usize::MAX;
            let mut mean_idx = usize::MAX;
            for k in authentic_keys {
                if let Some(i) = lower.find(k) {
                    if i < auth_idx {
                        auth_idx = i;
                    }
                }
            }
            for k in meaning_keys {
                if let Some(i) = lower.find(k) {
                    if i < mean_idx {
                        mean_idx = i;
                    }
                }
            }
            let v: Vec<f32> = if auth_idx < mean_idx {
                vec![1.0, 0.0, 0.0]
            } else if mean_idx < auth_idx {
                vec![0.0, 1.0, 0.0]
            } else {
                vec![0.0, 0.0, 1.0]
            };
            Box::pin(async move { Ok(v) })
        })
    }

    #[tokio::test]
    async fn traverse_atlas_finds_best_concern() {
        let atlas = sample_atlas();
        let t = traverse_atlas(&atlas, "authentic love vs society", &keyword_embed(), 0.5)
            .await
            .unwrap();
        // Expect cc_01 (authentic…) to be first.
        assert!(!t.locate.is_empty());
        assert_eq!(t.locate[0].concern_id, "cc_01");
        // Two positions under cc_01 + tension between them.
        assert_eq!(t.positions.len(), 2);
        assert_eq!(t.tensions.len(), 1);
        assert_eq!(t.grounding_chunk_ids, vec![100, 200]);
    }

    #[tokio::test]
    async fn traverse_atlas_falls_back_to_best_match_when_below_threshold() {
        let atlas = sample_atlas();
        // Orthogonal query embedding will score near 0 on every concern.
        let t = traverse_atlas(&atlas, "zzz orthogonal nothing", &keyword_embed(), 0.5)
            .await
            .unwrap();
        // We still return the single best match so LOCATE never empties.
        assert_eq!(t.locate.len(), 1);
    }

    #[tokio::test]
    async fn traverse_atlas_empty_atlas_returns_empty_result() {
        let atlas = Atlas::default();
        let t = traverse_atlas(&atlas, "anything", &keyword_embed(), 0.5)
            .await
            .unwrap();
        assert!(t.locate.is_empty());
        assert!(t.positions.is_empty());
    }

    #[tokio::test]
    async fn battery_loads_bare_string_array() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("battery.json");
        std::fs::write(&path, r#"["q one","q two","q three"]"#).unwrap();
        let b = QueryBattery::load(&path).unwrap();
        assert_eq!(b.questions.len(), 3);
        assert_eq!(b.questions[0].id, "q_01");
    }

    #[tokio::test]
    async fn battery_loads_structured_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("battery.json");
        std::fs::write(
            &path,
            r#"{"questions":[{"id":"a","question":"x"},{"id":"b","question":"y"}]}"#,
        )
        .unwrap();
        let b = QueryBattery::load(&path).unwrap();
        assert_eq!(b.questions.len(), 2);
        assert_eq!(b.questions[0].id, "a");
    }

    #[tokio::test]
    async fn run_battery_produces_per_question_rows() {
        let atlas = sample_atlas();
        let battery = QueryBattery {
            schema_version: 1,
            questions: vec![
                ValidationQuestion {
                    id: "q1".into(),
                    question: "authentic love in Anna".into(),
                },
                ValidationQuestion {
                    id: "q2".into(),
                    question: "meaning through Levin's labor".into(),
                },
            ],
        };
        let res = run_battery(&battery, &atlas, &keyword_embed(), 0.5)
            .await
            .unwrap();
        assert_eq!(res.rows.len(), 2);
        // Top match for q1 should be cc_01; its similarity should be 1.0
        // under our keyword embed.
        assert!(res.rows[0].top_match_similarity > 0.9);
        // pass_rate @ 0.5 is 100%.
        assert!(res.pass_rate(0.5) > 0.99);
    }

    #[test]
    fn from_cache_dir_requires_phase_3() {
        let dir = tempdir().unwrap();
        // Empty cache dir — phase 3 is missing.
        let err = Atlas::from_cache_dir(dir.path()).unwrap_err();
        assert!(format!("{err}").contains("phase 3"));
    }

    #[test]
    fn from_cache_dir_tolerates_missing_optional_phases() {
        let dir = tempdir().unwrap();
        let cache = PhaseCache::new(dir.path());
        let phase3 = Phase3Output {
            schema_version: Phase3Output::SCHEMA_VERSION,
            pipeline_id: "literary".into(),
            concerns: sample_atlas().concerns,
            failures: Vec::new(),
            written_at: "t".into(),
        };
        cache.write(PipelinePhase::Concerns, &phase3).unwrap();
        // Positions / tensions / gaps missing — should load empty.
        let atlas = Atlas::from_cache_dir(dir.path()).unwrap();
        assert_eq!(atlas.concerns.len(), 2);
        assert!(atlas.positions.is_empty());
        assert!(atlas.tensions.is_empty());
    }
}
