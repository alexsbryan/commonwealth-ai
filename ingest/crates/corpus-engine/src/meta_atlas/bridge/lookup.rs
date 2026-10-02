// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runtime-side bridge index — a read-only lookup over the persisted
//! [`BridgeEdgesFile`] for fast title→edges resolution at retrieval
//! time. The runtime holds an `Option<Arc<BridgeIndex>>` (the bridge
//! counterpart to [`super::super::index::MetaAtlasIndex`]); the
//! retrieval-time bridge boost consults it per question entity to pull
//! the *other* corpus's framing through a typed edge.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::atlas_canonical::lookup_key;

use super::edges::BridgeEdge;
use super::{default_bridge_edges_path, read_bridge_edges, BridgeEdgesFile};

/// Read-only lookup wrapper keyed by normalised topic title. Each edge
/// is indexed under BOTH its left and right title, so a query entity
/// matching either side surfaces the edge.
#[derive(Debug, Clone, Default)]
pub struct BridgeIndex {
    edges: Vec<Arc<BridgeEdge>>,
    /// normalised full key (title / entity key) → edge indices. Exact,
    /// precise path.
    by_key: HashMap<String, Vec<usize>>,
    /// significant token → edge indices. The robustness fallback: the
    /// entity extractor shreds "Computer Fraud and Abuse Act" into
    /// fragments, so an exact-key index alone misses them. A query token
    /// that overlaps any of an edge's key tokens resolves the edge.
    by_token: HashMap<String, Vec<usize>>,
}

/// Tokens of `s` worth indexing/matching on: ≥4 chars after normalisation
/// (`lookup_key` lowercases + folds punctuation to spaces, so
/// `"18 U.S.C. § 1030"` → `u s c 1030` → only `1030`). Deliberately NO
/// domain stopword list — commonness is discovered from the edge set
/// itself, by pruning high-document-frequency tokens in
/// [`BridgeIndex::from_file`]. The 4-char floor is a generic low-signal
/// cut, not corpus knowledge.
fn significant_tokens(s: &str) -> Vec<String> {
    crate::atlas_canonical::lookup_key(s)
        .split_whitespace()
        .filter(|t| t.chars().count() >= 4)
        .map(String::from)
        .collect()
}

impl BridgeIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load a persisted `bridge_edges.json`. Tolerates a missing file
    /// (returns empty) so a deploy with no bridge built yet doesn't
    /// break the chat path.
    pub fn load(path: Option<&Path>) -> std::io::Result<Self> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => match default_bridge_edges_path() {
                Some(p) => p,
                None => return Ok(Self::empty()),
            },
        };
        if !path.exists() {
            return Ok(Self::empty());
        }
        Ok(Self::from_file(read_bridge_edges(&path)?))
    }

    pub fn from_file(file: BridgeEdgesFile) -> Self {
        let mut edges: Vec<Arc<BridgeEdge>> = Vec::with_capacity(file.edges.len());
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_token: HashMap<String, Vec<usize>> = HashMap::new();
        for edge in file.edges {
            let idx = edges.len();
            // Key by both titles (normalised) AND the left topic's entity
            // keys (already normalised). Retrieval reaches an edge via the
            // entities a query surfaces — usually constituent entities
            // (people / works), not the concept title.
            let mut keys: Vec<String> =
                vec![lookup_key(&edge.left.title), lookup_key(&edge.right.title)];
            keys.extend(edge.left_entity_keys.iter().cloned());
            for key in &keys {
                if !key.is_empty() {
                    by_key.entry(key.clone()).or_default().push(idx);
                }
                for tok in significant_tokens(key) {
                    by_token.entry(tok).or_default().push(idx);
                }
            }
            edges.push(Arc::new(edge));
        }
        // Prune corpus-derived stopwords: a token appearing in too many
        // edges carries no discriminative signal (e.g. "section" across a
        // US-Code edge set). Threshold is data-driven (≤40% of edges, floor
        // 2) — nothing domain-specific is baked in.
        let max_df = ((edges.len() as f32 * 0.4).ceil() as usize).max(2);
        by_token.retain(|_, idxs| {
            idxs.sort_unstable();
            idxs.dedup();
            idxs.len() <= max_df
        });
        Self {
            edges,
            by_key,
            by_token,
        }
    }

    /// Edges whose left or right title matches `surface` (normalised).
    /// Deduped, preserving first-seen order.
    pub fn lookup(&self, surface: &str) -> Vec<Arc<BridgeEdge>> {
        let key = lookup_key(surface);
        if key.is_empty() {
            return Vec::new();
        }
        // Exact key match first (precise path).
        if let Some(idxs) = self.by_key.get(&key) {
            return idxs.iter().map(|&i| Arc::clone(&self.edges[i])).collect();
        }
        // Token fallback: any significant token the surface shares with an
        // edge's keys resolves it. Dedup by edge, preserve first-seen order.
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for tok in significant_tokens(surface) {
            if let Some(idxs) = self.by_token.get(&tok) {
                for &i in idxs {
                    if seen.insert(i) {
                        out.push(Arc::clone(&self.edges[i]));
                    }
                }
            }
        }
        out
    }

    /// Bulk lookup across surfaces; dedupes edges by their topic-key pair.
    pub fn lookup_any(&self, surfaces: &[String]) -> Vec<Arc<BridgeEdge>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for s in surfaces {
            for e in self.lookup(s) {
                if seen.insert(e.key()) {
                    out.push(e);
                }
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.edges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::super::edges::{BridgeRelation, BridgeSignal, EdgeSource, TopicRef};
    use super::*;

    fn file() -> BridgeEdgesFile {
        BridgeEdgesFile::new(
            vec![
                BridgeEdge {
                    left: TopicRef::new(
                        "sep-practical-wisdom",
                        "practical-wisdom",
                        "practical wisdom",
                    ),
                    right: TopicRef::new("wikipedia", "1", "Nicomachean Ethics"),
                    relation: BridgeRelation::Related,
                    confidence: 0.85,
                    signals_fired: vec![BridgeSignal::Embedding],
                    source: EdgeSource::Adjudicated,
                    rationale: Some("phronesis is within the treatise".into()),
                    left_entity_keys: vec!["aristotle".into()],
                },
                BridgeEdge {
                    left: TopicRef::new(
                        "sep-externalism",
                        "externalism",
                        "Externalism About the Mind",
                    ),
                    right: TopicRef::new("wikipedia", "2", "Semantic externalism"),
                    relation: BridgeRelation::Same,
                    confidence: 0.92,
                    signals_fired: vec![BridgeSignal::Embedding, BridgeSignal::SharedEntities],
                    source: EdgeSource::Deterministic,
                    rationale: None,
                    left_entity_keys: vec!["putnam".into(), "twin earth".into()],
                },
            ],
            vec![],
        )
    }

    #[test]
    fn lookup_matches_either_side() {
        let idx = BridgeIndex::from_file(file());
        assert_eq!(idx.len(), 2);
        // left-side title
        assert_eq!(idx.lookup("practical wisdom").len(), 1);
        // right-side title, case-insensitive
        assert_eq!(idx.lookup("nicomachean ethics").len(), 1);
        assert_eq!(
            idx.lookup("Semantic Externalism")[0].relation,
            BridgeRelation::Same
        );
        assert!(idx.lookup("nonexistent").is_empty());
    }

    #[test]
    fn lookup_resolves_via_constituent_entity_not_just_title() {
        let idx = BridgeIndex::from_file(file());
        // "Putnam" is a constituent ENTITY of the externalism topic, not
        // its title — retrieval surfaces people, so this MUST resolve the
        // edge even though the title is "Externalism About the Mind".
        // (This is the exact failure the live bench exposed: entities were
        // people, the index was keyed only on concept titles → 0 matches.)
        let hits = idx.lookup("Putnam");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].right.title, "Semantic externalism");
        // The title path still works.
        assert_eq!(idx.lookup("Externalism About the Mind").len(), 1);
        // Entity key on the other edge resolves too.
        assert_eq!(idx.lookup("aristotle").len(), 1);
    }

    #[test]
    fn token_fallback_resolves_fragments_and_self_prunes_common_tokens() {
        let mk = |t: &str, c: &str, keys: Vec<&str>| BridgeEdge {
            left: TopicRef::new("us-code", "x", t),
            right: TopicRef::new("scotus", "y", c),
            relation: BridgeRelation::Related,
            confidence: 0.9,
            signals_fired: vec![BridgeSignal::NameMatch],
            source: EdgeSource::Adjudicated,
            rationale: None,
            left_entity_keys: keys.into_iter().map(String::from).collect(),
        };
        // Three edges that all share the token "section" but each carry a
        // distinctive multi-word entity (the exact shape the legal seed
        // had, where the extractor shredded the phrases).
        let file = BridgeEdgesFile::new(
            vec![
                mk(
                    "18 U.S.C. § 1030",
                    "Van Buren v. United States",
                    vec![
                        "section 1030",
                        "computer fraud and abuse act",
                        "exceeds authorized access",
                    ],
                ),
                mk(
                    "47 U.S.C. § 230",
                    "Zeran v. America Online, Inc.",
                    vec!["section 230", "communications decency act"],
                ),
                mk(
                    "17 U.S.C. § 107",
                    "Campbell v. Acuff-Rose Music, Inc.",
                    vec!["section 107", "fair use"],
                ),
            ],
            vec![],
        );
        let idx = BridgeIndex::from_file(file);
        // A fragment of a multi-word entity still resolves via tokens.
        let cf = idx.lookup("Computer Fraud");
        assert_eq!(cf.len(), 1);
        assert_eq!(cf[0].right.title, "Van Buren v. United States");
        // "section" is in all 3 edges → pruned as a corpus-derived stopword
        // → no spurious over-match (was the engine-hardcoded-stoplist case).
        assert!(idx.lookup("Section").is_empty());
        // other_side picks the candidate (right) for a left-side fragment.
        let dec = idx.lookup("decency");
        assert_eq!(dec.len(), 1);
        assert_eq!(
            dec[0].other_side("decency").title,
            "Zeran v. America Online, Inc."
        );
    }

    #[test]
    fn lookup_any_dedupes() {
        let idx = BridgeIndex::from_file(file());
        // both surfaces hit the same edge → one result
        let hits = idx.lookup_any(&["practical wisdom".into(), "Nicomachean Ethics".into()]);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn empty_index_is_safe() {
        let idx = BridgeIndex::empty();
        assert!(idx.is_empty());
        assert!(idx.lookup("anything").is_empty());
    }

    #[test]
    fn missing_file_loads_empty() {
        let idx = BridgeIndex::load(Some(Path::new("/nonexistent/bridge_edges.json"))).unwrap();
        assert!(idx.is_empty());
    }
}
