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
    /// normalised title (`lookup_key`) → indices into `edges`.
    by_key: HashMap<String, Vec<usize>>,
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
        for edge in file.edges {
            let idx = edges.len();
            // Key by both titles (normalised) AND the left topic's entity
            // keys (already normalised). Retrieval reaches an edge via the
            // entities a query surfaces — usually constituent entities
            // (people / works), not the concept title.
            let mut keys: Vec<String> =
                vec![lookup_key(&edge.left.title), lookup_key(&edge.right.title)];
            keys.extend(edge.left_entity_keys.iter().cloned());
            for key in keys {
                if !key.is_empty() {
                    by_key.entry(key).or_default().push(idx);
                }
            }
            edges.push(Arc::new(edge));
        }
        Self { edges, by_key }
    }

    /// Edges whose left or right title matches `surface` (normalised).
    /// Deduped, preserving first-seen order.
    pub fn lookup(&self, surface: &str) -> Vec<Arc<BridgeEdge>> {
        let key = lookup_key(surface);
        if key.is_empty() {
            return Vec::new();
        }
        let Some(idxs) = self.by_key.get(&key) else {
            return Vec::new();
        };
        idxs.iter().map(|&i| Arc::clone(&self.edges[i])).collect()
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
    use super::*;
    use super::super::edges::{BridgeRelation, BridgeSignal, EdgeSource, TopicRef};

    fn file() -> BridgeEdgesFile {
        BridgeEdgesFile::new(
            vec![
                BridgeEdge {
                    left: TopicRef::new("sep-practical-wisdom", "practical-wisdom", "practical wisdom"),
                    right: TopicRef::new("wikipedia", "1", "Nicomachean Ethics"),
                    relation: BridgeRelation::Related,
                    confidence: 0.85,
                    signals_fired: vec![BridgeSignal::Embedding],
                    source: EdgeSource::Adjudicated,
                    rationale: Some("phronesis is within the treatise".into()),
                    left_entity_keys: vec!["aristotle".into()],
                },
                BridgeEdge {
                    left: TopicRef::new("sep-externalism", "externalism", "Externalism About the Mind"),
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
        assert_eq!(idx.lookup("Semantic Externalism")[0].relation, BridgeRelation::Same);
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
