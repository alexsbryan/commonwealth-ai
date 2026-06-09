// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runtime-side meta-atlas index — wraps a loaded [`MetaAtlasFile`]
//! for fast canonical-name lookups at retrieval time.
//!
//! Move 5 Stage 4. The sovereign-core `Runtime` holds an
//! `Option<Arc<MetaAtlasIndex>>` (replaces the Move 4 v1
//! `CanonicalRegistry`); the chat-path boost pass consults it on
//! every knowledge-query turn to surface stream-tagged anchors per
//! question entity.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::atlas_canonical::lookup_key;

use super::builder::{Anchor, MetaAtlasFile, MetaAtom};
use super::{default_meta_atlas_path, read_meta_atlas};

/// Read-only lookup wrapper. Keyed by normalised canonical key with
/// alias fallthrough and Move 5.1 token-index disambiguation.
#[derive(Debug, Clone)]
pub struct MetaAtlasIndex {
    by_key: HashMap<String, Arc<MetaAtom>>,
    /// Reverse alias map: alias_key -> canonical_key it resolves to.
    /// Built once at construction.
    alias_to_key: HashMap<String, String>,
    /// Move 5.1 token index: `token` → all canonical_keys whose
    /// space-split contains that token. Used by [`lookup`] for
    /// single-word surface forms to fan out across the meta-atlas
    /// and pick the best-scoring candidate ("Einstein" →
    /// "albert einstein" instead of the disambig stub
    /// canonical_key="einstein").
    token_to_keys: HashMap<String, Vec<String>>,
    /// Total number of distinct meta-atoms in the index.
    len: usize,
    /// Distinct corpus_ids that contributed at least one anchor.
    corpora: BTreeSet<String>,
}

impl MetaAtlasIndex {
    pub fn empty() -> Self {
        Self {
            by_key: HashMap::new(),
            alias_to_key: HashMap::new(),
            token_to_keys: HashMap::new(),
            len: 0,
            corpora: BTreeSet::new(),
        }
    }

    /// Load a previously-persisted `canonical_atoms.json`. Tolerates
    /// missing file (returns empty index) so a fresh deploy with no
    /// `sovereign meta-atlas build` yet doesn't break the chat path.
    pub fn load(path: Option<&Path>) -> std::io::Result<Self> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => match default_meta_atlas_path() {
                Some(p) => p,
                None => return Ok(Self::empty()),
            },
        };
        if !path.exists() {
            return Ok(Self::empty());
        }
        let file = read_meta_atlas(&path)?;
        Ok(Self::from_file(file))
    }

    /// Build an index from an in-memory [`MetaAtlasFile`]. Used by
    /// tests + by the bootstrap that already has the file in hand.
    pub fn from_file(file: MetaAtlasFile) -> Self {
        let mut by_key: HashMap<String, Arc<MetaAtom>> = HashMap::new();
        let mut alias_to_key: HashMap<String, String> = HashMap::new();
        let mut token_to_keys: HashMap<String, Vec<String>> = HashMap::new();
        let mut corpora: BTreeSet<String> = BTreeSet::new();
        for atom in file.atoms {
            for anchor in &atom.anchors {
                corpora.insert(anchor.corpus_id.clone());
            }
            let key = atom.canonical_key.clone();
            for alias in &atom.aliases {
                alias_to_key
                    .entry(alias.clone())
                    .or_insert_with(|| key.clone());
            }
            // Move 5.1 token index: every space-split token of the
            // canonical_key becomes a fanout entry. Lookup of a
            // single-word surface form fans across all candidates
            // that share that token.
            for token in key.split_whitespace() {
                if token.is_empty() {
                    continue;
                }
                token_to_keys
                    .entry(token.to_string())
                    .or_default()
                    .push(key.clone());
            }
            by_key.insert(key, Arc::new(atom));
        }
        let len = by_key.len();
        Self {
            by_key,
            alias_to_key,
            token_to_keys,
            len,
            corpora,
        }
    }

    /// Look up by a raw surface form. Normalises via
    /// [`crate::atlas_canonical::lookup_key`].
    ///
    /// Move 5.1: for single-word surface forms (e.g. "Einstein") the
    /// lookup fans out across all canonical_keys whose token-split
    /// contains the surface form, scores each candidate, and returns
    /// the highest-scoring one. The score function captures the
    /// observation that public-noun queries typically want the
    /// canonical biographical/topical article — which tends to have
    /// the surface form as a non-disambig token in a short clean
    /// canonical_key, not a disambiguation page with parenthetical
    /// suffix.
    ///
    /// Scoring components (per candidate):
    ///   +2.0 if surface form is the LAST token of the canonical_key
    ///        (matches "Einstein" → "albert einstein": surname is
    ///        typically last in `[FirstName] [LastName]` shape).
    ///   +1.0 if surface form is the FIRST token (matches "Hardin"
    ///        → "hardin's tragedy thesis").
    ///   −3.0 if display has parenthetical disambiguation
    ///        (`"Hurricane Newton (2016)"` penalty).
    ///   −0.1 per token (prefers shorter canonical_keys).
    ///   +max-anchor-salience across all of the meta-atom's anchors
    ///        (a strongly-attested meta-atom outranks a stub).
    ///
    /// Multi-word surface forms (e.g. "Albert Einstein", "Marie
    /// Curie") skip the fanout — they hit the exact canonical_key
    /// directly. Fanout is the disambig path for the hard case.
    pub fn lookup(&self, surface: &str) -> Option<Arc<MetaAtom>> {
        let key = lookup_key(surface);
        if key.is_empty() {
            return None;
        }
        // Multi-word: exact canonical_key match → alias map → None.
        if key.contains(' ') {
            if let Some(atom) = self.by_key.get(&key) {
                return Some(Arc::clone(atom));
            }
            if let Some(canon) = self.alias_to_key.get(&key) {
                return self.by_key.get(canon).cloned();
            }
            return None;
        }
        // Single-word: collect the exact + token-fanout candidates,
        // score, return best.
        let mut best: Option<(f32, &Arc<MetaAtom>)> = None;
        let consider = |atom: &Arc<MetaAtom>, key_str: &str| -> f32 {
            let tokens: Vec<&str> = key_str.split_whitespace().collect();
            let mut score = 0.0_f32;
            let last_match = tokens.last() == Some(&key.as_str());
            let first_match = tokens.first() == Some(&key.as_str());
            if last_match {
                score += 2.0;
            }
            if first_match && !last_match {
                score += 1.0;
            }
            if atom.display.contains('(') && atom.display.contains(')') {
                score -= 3.0;
            }
            score -= (tokens.len() as f32) * 0.1;
            let max_sal = atom
                .anchors
                .iter()
                .map(|a| a.salience)
                .fold(0.0_f32, f32::max);
            score += max_sal;
            score
        };
        let mut best_score = f32::MIN;
        // Exact canonical_key match (only single-token canon).
        if let Some(atom) = self.by_key.get(&key) {
            let s = consider(atom, &key);
            if s > best_score {
                best_score = s;
                best = Some((s, atom));
            }
        }
        // Alias map.
        if let Some(canon) = self.alias_to_key.get(&key) {
            if let Some(atom) = self.by_key.get(canon) {
                let s = consider(atom, canon);
                if s > best_score {
                    best_score = s;
                    best = Some((s, atom));
                }
            }
        }
        // Token fanout. Move 5.1: iterate in sorted order so ties
        // resolve deterministically (alphabetically-first canonical_key
        // wins) — without this, hashmap iteration order produces
        // unstable picks ("Newton" → "Helmut Newton" or "Isaac
        // Newton" depending on hasher seed).
        if let Some(keys) = self.token_to_keys.get(&key) {
            let mut sorted = keys.clone();
            sorted.sort();
            for canon in &sorted {
                if canon == &key {
                    continue; // already considered above
                }
                if let Some(atom) = self.by_key.get(canon) {
                    let s = consider(atom, canon);
                    if s > best_score {
                        best_score = s;
                        best = Some((s, atom));
                    }
                }
            }
        }
        best.map(|(_, atom)| Arc::clone(atom))
    }

    /// Bulk lookup. Preserves first-occurrence order; dedupes by
    /// canonical key.
    pub fn lookup_any(&self, surfaces: &[String]) -> Vec<Arc<MetaAtom>> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out: Vec<Arc<MetaAtom>> = Vec::new();
        for s in surfaces {
            if let Some(atom) = self.lookup(s) {
                if seen.insert(atom.canonical_key.clone()) {
                    out.push(atom);
                }
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn corpus_count(&self) -> usize {
        self.corpora.len()
    }

    /// Pick the highest-articulation-on-axis anchor for a meta-atom.
    /// Returns the anchor when its dominant axis equals `axis` AND
    /// its weight on `axis` clears `min_weight`. Ties resolved by
    /// `salience` descending.
    ///
    /// Used at retrieval time: meta-atlas-boost asks the index for
    /// "best Inventory anchor for Einstein", "best Argument anchor",
    /// "best Trace anchor", and injects up to three chunks.
    pub fn top_anchor_for_axis(
        atom: &MetaAtom,
        axis: crate::stream_axes::Articulation,
        min_weight: f32,
    ) -> Option<&Anchor> {
        atom.anchors
            .iter()
            .filter(|a| {
                a.articulation.dominant() == axis && a.articulation.weight(axis) >= min_weight
            })
            .max_by(|a, b| {
                a.salience
                    .partial_cmp(&b.salience)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::{AtomId, ChunkRef};
    use crate::meta_atlas::builder::{Anchor, MetaAtom};
    use crate::stream_axes::{Articulation, ArticulationVector, Stability};
    use std::collections::BTreeSet;

    fn anchor(corpus: &str, art: ArticulationVector, salience: f32) -> Anchor {
        Anchor {
            corpus_id: corpus.into(),
            atom_id: AtomId::entity(1),
            primary_chunk: ChunkRef::new("sec_0001", None),
            articulation: art,
            stability: Some(Stability::Frozen),
            salience,
            atlas_content_hash: String::new(),
        }
    }

    #[test]
    fn lookup_finds_by_canonical_key() {
        let file = MetaAtlasFile {
            schema_version: "1.0".into(),
            built_at: 0,
            atlases_seen: Vec::new(),
            atoms: vec![MetaAtom {
                canonical_key: "albert einstein".into(),
                display: "Albert Einstein".into(),
                aliases: BTreeSet::from(["einstein".to_string()]),
                anchors: vec![anchor(
                    "wikipedia",
                    ArticulationVector::new(0.75, 0.20, 0.05),
                    0.5,
                )],
            }],
        };
        let idx = MetaAtlasIndex::from_file(file);
        assert!(idx.lookup("Albert Einstein").is_some());
        assert!(idx.lookup("Einstein").is_some());
        assert!(idx.lookup("Nonexistent").is_none());
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.corpus_count(), 1);
    }

    #[test]
    fn top_anchor_picks_dominant_axis_match() {
        let atom = MetaAtom {
            canonical_key: "x".into(),
            display: "X".into(),
            aliases: BTreeSet::new(),
            anchors: vec![
                anchor("wiki", ArticulationVector::new(0.8, 0.15, 0.05), 0.5),
                anchor("sep", ArticulationVector::new(0.1, 0.85, 0.05), 0.9),
                anchor("conv", ArticulationVector::new(0.1, 0.1, 0.8), 0.7),
            ],
        };
        let inv = MetaAtlasIndex::top_anchor_for_axis(&atom, Articulation::Inventory, 0.4).unwrap();
        assert_eq!(inv.corpus_id, "wiki");
        let arg = MetaAtlasIndex::top_anchor_for_axis(&atom, Articulation::Argument, 0.4).unwrap();
        assert_eq!(arg.corpus_id, "sep");
        let trc = MetaAtlasIndex::top_anchor_for_axis(&atom, Articulation::Trace, 0.4).unwrap();
        assert_eq!(trc.corpus_id, "conv");
    }

    #[test]
    fn top_anchor_returns_none_when_no_dominant_match_clears_threshold() {
        let atom = MetaAtom {
            canonical_key: "x".into(),
            display: "X".into(),
            aliases: BTreeSet::new(),
            anchors: vec![anchor(
                "wiki",
                ArticulationVector::balanced(), // 0.33/0.33/0.33
                0.5,
            )],
        };
        // dominant resolves to Inventory by tie-break (since
        // ArticulationVector::dominant has Inventory→Argument→Trace
        // priority), but weight is 0.33 — below 0.40 threshold.
        let inv = MetaAtlasIndex::top_anchor_for_axis(&atom, Articulation::Inventory, 0.40);
        assert!(inv.is_none());
    }

    #[test]
    fn empty_index_lookup_returns_none() {
        let idx = MetaAtlasIndex::empty();
        assert!(idx.lookup("anything").is_none());
        assert!(idx.is_empty());
    }

    // ── Move 5.1: disambig fanout ──────────────────────────

    /// Surface form "Einstein" should resolve to the
    /// `canonical_key="albert einstein"` meta-atom (last-token match,
    /// non-disambig display) over the `canonical_key="einstein"`
    /// disambig stub (lower salience, no parenthetical but tied on
    /// last-token).
    #[test]
    fn surface_einstein_prefers_albert_einstein_over_disambig_stub() {
        let einstein_stub = MetaAtom {
            canonical_key: "einstein".into(),
            display: "Einstein".into(),
            aliases: BTreeSet::new(),
            anchors: vec![anchor(
                "wikipedia",
                ArticulationVector::new(0.65, 0.25, 0.10),
                0.00, // disambig stubs carry low salience
            )],
        };
        let albert = MetaAtom {
            canonical_key: "albert einstein".into(),
            display: "Albert Einstein".into(),
            aliases: BTreeSet::new(),
            anchors: vec![anchor(
                "wikipedia",
                ArticulationVector::new(0.65, 0.25, 0.10),
                0.50, // real article
            )],
        };
        let file = MetaAtlasFile {
            schema_version: "1.0".into(),
            built_at: 0,
            atlases_seen: Vec::new(),
            atoms: vec![einstein_stub, albert],
        };
        let idx = MetaAtlasIndex::from_file(file);
        let hit = idx.lookup("Einstein").expect("should match");
        assert_eq!(hit.canonical_key, "albert einstein");
        assert_eq!(hit.display, "Albert Einstein");
    }

    /// Surface form "Newton" should prefer
    /// `canonical_key="isaac newton"` over
    /// `canonical_key="hurricane newton 2016"` because the latter
    /// has a parenthetical disambig in its display string.
    #[test]
    fn surface_newton_prefers_isaac_newton_over_parenthetical_disambig() {
        let isaac = MetaAtom {
            canonical_key: "isaac newton".into(),
            display: "Isaac Newton".into(),
            aliases: BTreeSet::new(),
            anchors: vec![anchor(
                "wikipedia",
                ArticulationVector::new(0.65, 0.25, 0.10),
                0.50,
            )],
        };
        let hurricane = MetaAtom {
            canonical_key: "hurricane newton 2016".into(),
            display: "Hurricane Newton (2016)".into(),
            aliases: BTreeSet::new(),
            anchors: vec![anchor(
                "wikipedia",
                ArticulationVector::new(0.65, 0.25, 0.10),
                0.50,
            )],
        };
        let file = MetaAtlasFile {
            schema_version: "1.0".into(),
            built_at: 0,
            atlases_seen: Vec::new(),
            atoms: vec![isaac, hurricane],
        };
        let idx = MetaAtlasIndex::from_file(file);
        let hit = idx.lookup("Newton").expect("should match");
        assert_eq!(hit.canonical_key, "isaac newton");
    }

    /// Multi-word surface forms skip the fanout — they hit
    /// canonical_key exactly. This preserves the simple case for
    /// already-canonical questions ("Marie Curie", "Albert Einstein").
    #[test]
    fn multiword_surface_uses_exact_match_only() {
        let marie = MetaAtom {
            canonical_key: "marie curie".into(),
            display: "Marie Curie".into(),
            aliases: BTreeSet::new(),
            anchors: vec![anchor(
                "wikipedia",
                ArticulationVector::new(0.65, 0.25, 0.10),
                0.50,
            )],
        };
        let file = MetaAtlasFile {
            schema_version: "1.0".into(),
            built_at: 0,
            atlases_seen: Vec::new(),
            atoms: vec![marie],
        };
        let idx = MetaAtlasIndex::from_file(file);
        let hit = idx.lookup("Marie Curie").expect("should match");
        assert_eq!(hit.canonical_key, "marie curie");
    }

    /// Token fanout doesn't introduce false matches on single-token
    /// surface forms with no candidate canonical_keys.
    #[test]
    fn unknown_single_word_surface_returns_none() {
        let idx = MetaAtlasIndex::empty();
        assert!(idx.lookup("Zogfraz").is_none());
    }

    /// Explicit aliases still win over token fanout when they point
    /// at a higher-scoring meta-atom. Tests that the alias map path
    /// is still consulted.
    #[test]
    fn explicit_alias_path_still_consulted() {
        let alpha = MetaAtom {
            canonical_key: "alpha".into(),
            display: "Alpha".into(),
            aliases: BTreeSet::from(["xyz".to_string()]),
            anchors: vec![anchor(
                "wikipedia",
                ArticulationVector::new(0.65, 0.25, 0.10),
                0.80,
            )],
        };
        let file = MetaAtlasFile {
            schema_version: "1.0".into(),
            built_at: 0,
            atlases_seen: Vec::new(),
            atoms: vec![alpha],
        };
        let idx = MetaAtlasIndex::from_file(file);
        let hit = idx.lookup("XYZ").expect("should match via alias");
        assert_eq!(hit.canonical_key, "alpha");
    }
}
