// SPDX-License-Identifier: AGPL-3.0-or-later
//! Faithfulness-lane scoring primitives (T1 P0.3).
//!
//! Pure aggregation over judged claim tuples — the `(member_chunks,
//! claim, verdict)` rows the faithfulness lane appends to
//! `sovereign/bench/faithfulness/`. The LLM calls (claim decomposition
//! via `extract_claim_list`, per-claim support verdicts) live in the
//! bench orchestrator; nothing here talks to a model, so every number
//! is replayable from the JSONL alone.
//!
//! The lane's headline is the **per-corpus unsupported-claim rate**,
//! tracked against a `LaneBaseline` like every other bench lane —
//! regression-gated, not absolute-thresholded. Measured floor on the
//! SP3 seeds (2026-07-31): obsidian at 0.107 under the primary-tier
//! judge (197 claims), 0.147 under the fast tier (959 claims) — the
//! judge tier moves the absolute rate, which is why cross-run
//! comparisons pin `judge_model`.
//!
//! Also here, because they are pure and the orchestrator needs them:
//! the SP3 sampling policy (full-judge ≤ `full_threshold` nodes,
//! seeded stratified sampling above) and the parentless-top forest
//! grouping (P5.1: RAPTOR output is a FOREST — grouping by max-level
//! undercounts trees).

use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};

/// Judge verdict on one claim against its sealed evidence window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimVerdict {
    Supported,
    Unsupported,
}

/// One judged claim tuple, as appended to the lane's JSONL.
///
/// Deserializes BOTH row generations: the SP3 seed rows
/// (`corpus`, `member_chunks` as indexes, no texts) and the P0.3
/// appender's superset rows (`corpus_id`, sealed `evidence_chunks`
/// texts — the HarvestItem-compatible shape agreed with the
/// verifier-v0 Stream B side). Unknown fields are ignored by design:
/// the appender may carry fields only the trainer consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub claim: String,
    pub verdict: ClaimVerdict,
    /// Max per-chunk support probability the judge saw (diagnostic).
    #[serde(default)]
    pub max_support: f64,
    #[serde(alias = "corpus")]
    pub corpus_id: String,
    pub node_id: String,
    /// RAPTOR tree level of the node the claim came from (0 = leaf-cluster).
    pub level: u32,
    pub judge_model: String,
}

/// Per-level slice of the rate — level 0 is near-leaf summaries; rising
/// unsupported rates at higher levels are the summary-of-summary
/// compounding signal P1.4 exists to cut off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelRate {
    pub level: u32,
    pub n_claims: usize,
    pub n_unsupported: usize,
    pub unsupported_rate: f64,
}

/// Per-corpus faithfulness report — the lane's scoring unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaithfulnessReport {
    pub corpus_id: String,
    /// Judge models that produced the verdicts. More than one means the
    /// file mixes judge tiers — the headline rate is then NOT comparable
    /// across runs and the lane should treat the report as tainted.
    pub judge_models: Vec<String>,
    pub n_nodes: usize,
    pub n_claims: usize,
    pub n_unsupported: usize,
    /// The headline: unsupported claims / all claims. 0.0 when empty —
    /// but `n_claims == 0` must be treated as "nothing verified", never
    /// as a perfect score (same rule as the zero-test guard in the test
    /// runner).
    pub unsupported_rate: f64,
    pub per_level: Vec<LevelRate>,
}

/// Aggregate judged claim tuples into per-corpus reports.
///
/// Pure: same records in, same reports out, any order — records are
/// bucketed by `corpus_id` and levels sorted ascending.
pub fn score(records: &[ClaimRecord]) -> Vec<FaithfulnessReport> {
    let mut by_corpus: BTreeMap<&str, Vec<&ClaimRecord>> = BTreeMap::new();
    for r in records {
        by_corpus.entry(&r.corpus_id).or_default().push(r);
    }
    by_corpus
        .into_iter()
        .map(|(corpus_id, rows)| {
            let mut judge_models: Vec<String> = Vec::new();
            let mut nodes: BTreeMap<&str, ()> = BTreeMap::new();
            let mut levels: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
            let mut n_unsupported = 0usize;
            for r in &rows {
                if !judge_models.contains(&r.judge_model) {
                    judge_models.push(r.judge_model.clone());
                }
                nodes.insert(&r.node_id, ());
                let e = levels.entry(r.level).or_insert((0, 0));
                e.0 += 1;
                if r.verdict == ClaimVerdict::Unsupported {
                    e.1 += 1;
                    n_unsupported += 1;
                }
            }
            let n_claims = rows.len();
            FaithfulnessReport {
                corpus_id: corpus_id.to_string(),
                judge_models,
                n_nodes: nodes.len(),
                n_claims,
                n_unsupported,
                unsupported_rate: if n_claims == 0 {
                    0.0
                } else {
                    n_unsupported as f64 / n_claims as f64
                },
                per_level: levels
                    .into_iter()
                    .map(|(level, (n, u))| LevelRate {
                        level,
                        n_claims: n,
                        n_unsupported: u,
                        unsupported_rate: if n == 0 { 0.0 } else { u as f64 / n as f64 },
                    })
                    .collect(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Judge sampling policy (SP3 economics)
// ---------------------------------------------------------------------------

/// A node the orchestrator could judge — id plus the stratification key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeMeta {
    pub node_id: String,
    pub level: u32,
}

/// How the run selected nodes — recorded in the lane report so a
/// sampled 12% run is never mistaken for full coverage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SampleMode {
    Full,
    Stratified { rate: f64, seed: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplePlan {
    pub mode: SampleMode,
    /// Node ids to judge, in deterministic order.
    pub selected: Vec<String>,
    pub n_total: usize,
}

/// SP3 policy: judge every node up to `full_threshold` (~1.5k); above
/// it, a seeded stratified sample at `rate` (10–15%), stratified by
/// level so the thin upper levels — where compounding fabrication
/// lives — are never sampled away. Deterministic for a given
/// `(nodes, rate, seed)`; per-level counts are `ceil(n × rate)` so no
/// non-empty level rounds to zero.
pub fn plan_judge_sample(
    nodes: &[NodeMeta],
    full_threshold: usize,
    rate: f64,
    seed: u64,
) -> SamplePlan {
    if nodes.len() <= full_threshold {
        return SamplePlan {
            mode: SampleMode::Full,
            selected: nodes.iter().map(|n| n.node_id.clone()).collect(),
            n_total: nodes.len(),
        };
    }
    let mut by_level: BTreeMap<u32, Vec<&NodeMeta>> = BTreeMap::new();
    for n in nodes {
        by_level.entry(n.level).or_default().push(n);
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let mut selected = Vec::new();
    for (_, mut level_nodes) in by_level {
        // Sort before shuffling so the pick depends only on (ids, seed),
        // not on the caller's iteration order.
        level_nodes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        let take = ((level_nodes.len() as f64 * rate).ceil() as usize).max(1);
        level_nodes.shuffle(&mut rng);
        selected.extend(level_nodes.into_iter().take(take).map(|n| n.node_id.clone()));
    }
    SamplePlan {
        mode: SampleMode::Stratified { rate, seed },
        selected,
        n_total: nodes.len(),
    }
}

// ---------------------------------------------------------------------------
// Forest grouping (P5.1 caveat)
// ---------------------------------------------------------------------------

/// Group nodes into trees by walking each node up to its parentless
/// top. RAPTOR output is a FOREST (P5.1): grouping "the tree" by
/// max-level silently merges roots. Input is `(node_id, parent_id)`;
/// output maps each root id to its member node ids (root included).
/// Nodes whose parent chain dangles (parent id absent from the input)
/// are rooted at the last resolvable ancestor rather than dropped.
pub fn group_by_parentless_top(
    nodes: &[(String, Option<String>)],
) -> BTreeMap<String, Vec<String>> {
    let parent: BTreeMap<&str, Option<&str>> = nodes
        .iter()
        .map(|(id, p)| (id.as_str(), p.as_deref()))
        .collect();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, _) in nodes {
        let mut cur = id.as_str();
        // Bounded by node count; a parent cycle would otherwise hang us.
        for _ in 0..nodes.len() {
            match parent.get(cur) {
                Some(Some(p)) if parent.contains_key(p) => cur = p,
                _ => break,
            }
        }
        groups.entry(cur.to_string()).or_default().push(id.clone());
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(corpus: &str, node: &str, level: u32, verdict: ClaimVerdict) -> ClaimRecord {
        ClaimRecord {
            claim: "c".into(),
            verdict,
            max_support: 0.5,
            corpus_id: corpus.into(),
            node_id: node.into(),
            level,
            judge_model: "primary".into(),
        }
    }

    #[test]
    fn seed_row_shape_still_deserializes() {
        // Pinned to the SP3 seed generation (sp3_streamb.py): `corpus`
        // not `corpus_id`, `member_chunks` as indexes, no chunk texts.
        let seed = r#"{"member_chunks":[3,4],"claim":"x","verdict":"unsupported",
            "max_support":0.31,"chunks_checked":2,"corpus":"obsidian-vault",
            "node_id":"n1","level":0,"judge_model":"fast"}"#;
        let r: ClaimRecord = serde_json::from_str(seed).unwrap();
        assert_eq!(r.corpus_id, "obsidian-vault");
        assert_eq!(r.verdict, ClaimVerdict::Unsupported);
    }

    #[test]
    fn superset_row_shape_deserializes() {
        // The P0.3 appender shape — HarvestItem-compatible core plus
        // lane fields; the scorer only reads its own subset.
        let row = r#"{"id":"bk/n9/c0","corpus_id":"bk","question":"",
            "claim":"x","evidence_chunks":["t"],"evidence_chunk_ids":["c9"],
            "verdict":"supported","max_support":0.97,"judge_model":"primary",
            "node_id":"n9","level":2,"sampling":"full"}"#;
        let r: ClaimRecord = serde_json::from_str(row).unwrap();
        assert_eq!(r.level, 2);
        assert_eq!(r.verdict, ClaimVerdict::Supported);
    }

    #[test]
    fn committed_sp3_seed_files_score() {
        // The two SP3 seed files are the lane's oldest data — if this
        // breaks, the appender schema drifted from what's on disk.
        // Skips (loudly, via eprintln) on partial checkouts.
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/faithfulness");
        for (file, n_claims, n_unsupported) in [
            ("obsidian_fast_seed.jsonl", 959, 141),
            ("obsidian_primary_sample_seed.jsonl", 197, 21),
        ] {
            let path = format!("{root}/{file}");
            let Ok(raw) = std::fs::read_to_string(&path) else {
                eprintln!("seed file {path} absent — skipping");
                continue;
            };
            let records: Vec<ClaimRecord> = raw
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            let reports = score(&records);
            assert_eq!(reports.len(), 1, "{file}: one corpus expected");
            assert_eq!(reports[0].n_claims, n_claims, "{file}");
            assert_eq!(reports[0].n_unsupported, n_unsupported, "{file}");
        }
    }

    #[test]
    fn rates_are_per_corpus_and_per_level() {
        let records = vec![
            rec("a", "n1", 0, ClaimVerdict::Supported),
            rec("a", "n1", 0, ClaimVerdict::Unsupported),
            rec("a", "n2", 1, ClaimVerdict::Supported),
            rec("b", "n3", 0, ClaimVerdict::Unsupported),
        ];
        let reports = score(&records);
        assert_eq!(reports.len(), 2);
        let a = &reports[0];
        assert_eq!((a.corpus_id.as_str(), a.n_nodes, a.n_claims), ("a", 2, 3));
        assert!((a.unsupported_rate - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(a.per_level.len(), 2);
        assert_eq!(a.per_level[0].n_unsupported, 1);
        assert_eq!(reports[1].unsupported_rate, 1.0);
    }

    #[test]
    fn empty_input_scores_nothing() {
        assert!(score(&[]).is_empty());
    }

    #[test]
    fn mixed_judges_are_surfaced() {
        let mut r2 = rec("a", "n2", 0, ClaimVerdict::Supported);
        r2.judge_model = "fast".into();
        let reports = score(&[rec("a", "n1", 0, ClaimVerdict::Supported), r2]);
        assert_eq!(reports[0].judge_models, vec!["primary", "fast"]);
    }

    fn meta(id: &str, level: u32) -> NodeMeta {
        NodeMeta { node_id: id.into(), level }
    }

    #[test]
    fn small_forests_get_full_coverage() {
        let nodes: Vec<NodeMeta> = (0..10).map(|i| meta(&format!("n{i}"), 0)).collect();
        let plan = plan_judge_sample(&nodes, 1500, 0.12, 7);
        assert_eq!(plan.mode, SampleMode::Full);
        assert_eq!(plan.selected.len(), 10);
    }

    #[test]
    fn stratified_sample_is_deterministic_and_covers_thin_levels() {
        let mut nodes: Vec<NodeMeta> = (0..2000).map(|i| meta(&format!("n{i}"), 0)).collect();
        nodes.push(meta("top1", 3)); // a one-node level must survive sampling
        let a = plan_judge_sample(&nodes, 1500, 0.10, 42);
        let b = plan_judge_sample(&nodes, 1500, 0.10, 42);
        assert_eq!(a.selected, b.selected);
        assert!(a.selected.contains(&"top1".to_string()));
        assert_eq!(a.selected.len(), 201); // ceil(2000×0.10) + 1
        // Order-independence: same pick from a reversed input.
        let mut rev = nodes.clone();
        rev.reverse();
        let c = plan_judge_sample(&rev, 1500, 0.10, 42);
        assert_eq!(a.selected, c.selected);
        // A different seed picks a different subset.
        let d = plan_judge_sample(&nodes, 1500, 0.10, 43);
        assert_ne!(a.selected, d.selected);
    }

    #[test]
    fn forest_groups_by_parentless_top_not_level() {
        // Two roots — max-level grouping would see one tree.
        let nodes = vec![
            ("r1".to_string(), None),
            ("a".to_string(), Some("r1".to_string())),
            ("b".to_string(), Some("a".to_string())),
            ("r2".to_string(), None),
            ("c".to_string(), Some("r2".to_string())),
            ("dangling".to_string(), Some("ghost".to_string())),
        ];
        let groups = group_by_parentless_top(&nodes);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups["r1"], vec!["r1", "a", "b"]);
        assert_eq!(groups["r2"], vec!["r2", "c"]);
        // A dangling parent roots the node at itself, never drops it.
        assert_eq!(groups["dangling"], vec!["dangling"]);
    }
}
