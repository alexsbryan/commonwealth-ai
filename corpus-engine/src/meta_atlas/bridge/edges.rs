// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed cross-corpus alignment edges + a reversible append-only oplog.
//!
//! A [`BridgeEdge`] is one typed correspondence between an SEP topic
//! and a Wikipedia topic — the atom of the ontological map. Edges are
//! persisted as a snapshot ([`BridgeEdgesFile`] at
//! `~/.sovereign/meta-atlas/bridge_edges.json`, atomic tmp+rename) and
//! every add/remove is also journalled to an append-only oplog
//! ([`BridgeOplog`] at `bridge_oplog.jsonl`) so a bad alignment is
//! reversible and auditable.
//!
//! The oplog mirrors the *discipline* of
//! [`crate::enrichment::reconciliation::oplog`] (append-only JSONL, one
//! line per op) but uses bridge-native types: the reconciliation
//! `OpKind`/`OplogEntry` are atom-merge-shaped (`AtomId` + `MergeSignal`)
//! and cannot model an edge add/remove without abuse. We borrow the
//! pattern, not the schema.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atlas_canonical::lookup_key;

/// Typed relation, read FROM the left topic TO the right topic
/// (`left` {relation} `right`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeRelation {
    /// The same concept in two registers (e.g. SEP argues it, Wikipedia
    /// inventories it). The canonical "stereo view" edge.
    Same,
    /// The left topic is broader than the right topic — it subsumes a
    /// narrower right article (e.g. "Personal Identity" ⊃ "Ship of
    /// Theseus"). Drives subsumption zoom.
    Broader,
    /// The left topic is narrower than the right topic.
    Narrower,
    /// Related but neither equivalent nor subsuming.
    Related,
}

impl BridgeRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            BridgeRelation::Same => "same",
            BridgeRelation::Broader => "broader",
            BridgeRelation::Narrower => "narrower",
            BridgeRelation::Related => "related",
        }
    }

    /// The same relation seen from the right side (broader and narrower
    /// swap; same and related are symmetric).
    pub fn inverse(self) -> Self {
        match self {
            BridgeRelation::Same => BridgeRelation::Same,
            BridgeRelation::Broader => BridgeRelation::Narrower,
            BridgeRelation::Narrower => BridgeRelation::Broader,
            BridgeRelation::Related => BridgeRelation::Related,
        }
    }
}

/// Which alignment signal contributed to an edge. Persisted on the
/// edge so `meta-atlas explain` can show *why* two topics were linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeSignal {
    /// Normalised name / alias overlap.
    NameMatch,
    /// Concept-embedding cosine.
    Embedding,
    /// Jaccard of the two topics' `entity_keys` (the demoted name-
    /// cluster meta-atom, reused as a feature).
    SharedEntities,
    /// SEP's named entities appear as Wikipedia link-graph neighbours
    /// of the candidate.
    LinkGraphCoNeighbor,
    /// SEP Argument-dominant × Wikipedia Inventory-dominant — the
    /// "two registers" signature.
    ArticulationComplementarity,
    /// Shared Wikidata QID (near-exact; usually inert today).
    WikidataAnchor,
}

impl BridgeSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            BridgeSignal::NameMatch => "name_match",
            BridgeSignal::Embedding => "embedding",
            BridgeSignal::SharedEntities => "shared_entities",
            BridgeSignal::LinkGraphCoNeighbor => "link_graph_co_neighbor",
            BridgeSignal::ArticulationComplementarity => "articulation_complementarity",
            BridgeSignal::WikidataAnchor => "wikidata_anchor",
        }
    }
}

/// How an edge's relation was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeSource {
    /// Deterministic signals alone cleared the auto-`same` threshold.
    Deterministic,
    /// The relation was typed by the LLM adjudicator (uncertain band).
    Adjudicated,
}

/// A pointer to one topic on one side of an edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicRef {
    pub corpus_id: String,
    pub topic_id: String,
    pub title: String,
}

impl TopicRef {
    pub fn new(
        corpus_id: impl Into<String>,
        topic_id: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            corpus_id: corpus_id.into(),
            topic_id: topic_id.into(),
            title: title.into(),
        }
    }

    /// Stable identity handle — `<corpus_id>::<topic_id>`.
    pub fn key(&self) -> String {
        format!("{}::{}", self.corpus_id, self.topic_id)
    }
}

/// One typed cross-corpus correspondence, directional `left → right`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeEdge {
    /// Driver side — the corpus whose topics are enumerated and aligned
    /// from (e.g. SEP in the first instantiation).
    pub left: TopicRef,
    /// Candidate side — the corpus searched for matches (e.g. Wikipedia).
    pub right: TopicRef,
    pub relation: BridgeRelation,
    pub confidence: f32,
    pub signals_fired: Vec<BridgeSignal>,
    pub source: EdgeSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Normalised entity keys of the LEFT topic — the entities it names
    /// (philosophers, works, sub-concepts), not just the concept title.
    /// Retrieval surfaces *entities* (often people: "Kant", "Gödel"),
    /// rarely the concept title, so the index must be reachable by these
    /// for `bridge_boost` to fire. Populated at build; empty in older
    /// snapshots (`#[serde(default)]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_entity_keys: Vec<String>,
}

impl BridgeEdge {
    /// Identity of the edge — the ordered pair of topic keys. Two edges
    /// with the same key are the same correspondence (relation may have
    /// been revised).
    pub fn key(&self) -> (String, String) {
        (self.left.key(), self.right.key())
    }

    /// Given a surface form that matched one side of this edge, return
    /// the OTHER side — the topic to fetch for the cross-corpus "stereo"
    /// view. The surface can match the left side via its title OR via a
    /// constituent entity key (retrieval surfaces people/works, not the
    /// concept title), so both must count as "matched left" → fetch
    /// right. Otherwise it matched the right title → fetch left.
    pub fn other_side(&self, surface: &str) -> &TopicRef {
        let k = lookup_key(surface);
        let rk = lookup_key(&self.right.title);
        // If the surface IS (or contains, or is contained by) the right
        // title, it came in via the right side → fetch left. Otherwise it
        // matched the left side — its title, a constituent entity, or a
        // token fragment thereof — → fetch right (the candidate corpus).
        let contained = k.chars().count() >= 4 && (rk.contains(&k) || k.contains(&rk));
        if !k.is_empty() && (k == rk || contained) {
            &self.left
        } else {
            &self.right
        }
    }
}

/// The persisted snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeEdgesFile {
    pub schema_version: String,
    pub built_at: u64,
    /// Every topic that participated in a build (for `explain` lookups
    /// and coverage reporting), deduped by key.
    pub topics_seen: Vec<TopicRef>,
    pub edges: Vec<BridgeEdge>,
}

impl BridgeEdgesFile {
    pub const SCHEMA_VERSION: &'static str = "1.0";

    pub fn new(edges: Vec<BridgeEdge>, topics_seen: Vec<TopicRef>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            built_at: crate::stream_axes::timestamp_now(),
            topics_seen,
            edges,
        }
    }

    /// All edges whose left or right side matches `topic_key`.
    pub fn edges_for<'a>(&'a self, topic_key: &str) -> impl Iterator<Item = &'a BridgeEdge> + 'a {
        let key = topic_key.to_string();
        self.edges
            .iter()
            .filter(move |e| e.left.key() == key || e.right.key() == key)
    }
}

/// Default on-disk path for the persisted bridge edges.
pub fn default_bridge_edges_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".sovereign")
            .join("meta-atlas")
            .join("bridge_edges.json"),
    )
}

/// Atomically write the snapshot (tmp + rename), creating parent dirs.
/// Mirrors `meta_atlas::builder::write_meta_atlas`.
pub fn write_bridge_edges(file: &BridgeEdgesFile, out_path: &Path) -> io::Result<()> {
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = out_path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(file).map_err(io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, out_path)?;
    Ok(())
}

pub fn read_bridge_edges(path: &Path) -> io::Result<BridgeEdgesFile> {
    let s = fs::read_to_string(path)?;
    serde_json::from_str(&s).map_err(io::Error::other)
}

// ── reversible oplog ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeOpKind {
    AddEdge,
    RemoveEdge,
}

/// One line in `bridge_oplog.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeOp {
    pub op: BridgeOpKind,
    pub left_key: String,
    pub right_key: String,
    pub relation: BridgeRelation,
    pub signals: Vec<BridgeSignal>,
    pub source: EdgeSource,
    pub ts_unix: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rationale: String,
}

impl BridgeOp {
    pub fn add(edge: &BridgeEdge) -> Self {
        Self {
            op: BridgeOpKind::AddEdge,
            left_key: edge.left.key(),
            right_key: edge.right.key(),
            relation: edge.relation,
            signals: edge.signals_fired.clone(),
            source: edge.source,
            ts_unix: crate::stream_axes::timestamp_now() as i64,
            rationale: edge.rationale.clone().unwrap_or_default(),
        }
    }

    pub fn remove(edge: &BridgeEdge, rationale: impl Into<String>) -> Self {
        Self {
            op: BridgeOpKind::RemoveEdge,
            left_key: edge.left.key(),
            right_key: edge.right.key(),
            relation: edge.relation,
            signals: edge.signals_fired.clone(),
            source: edge.source,
            ts_unix: crate::stream_axes::timestamp_now() as i64,
            rationale: rationale.into(),
        }
    }
}

/// Append-only reader+writer over `bridge_oplog.jsonl`.
pub struct BridgeOplog {
    pub path: PathBuf,
}

impl BridgeOplog {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            path: dir.into().join("bridge_oplog.jsonl"),
        }
    }

    pub fn append(&self, op: &BridgeOp) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(op).map_err(io::Error::other)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }

    pub fn read_all(&self) -> io::Result<Vec<BridgeOp>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let reader = BufReader::new(fs::File::open(&self.path)?);
        let mut out = Vec::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<BridgeOp>(&line) {
                Ok(op) => out.push(op),
                Err(e) => tracing::warn!(
                    path = %self.path.display(),
                    line = lineno + 1,
                    "bridge_oplog: malformed line skipped ({e})"
                ),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge() -> BridgeEdge {
        BridgeEdge {
            left: TopicRef::new("sep-externalism-mind", "externalism-mind", "Externalism About the Mind"),
            right: TopicRef::new("wikipedia", "98765", "Semantic externalism"),
            relation: BridgeRelation::Same,
            confidence: 0.91,
            signals_fired: vec![BridgeSignal::Embedding, BridgeSignal::SharedEntities],
            source: EdgeSource::Adjudicated,
            rationale: Some("both treat content externalism via Twin Earth".into()),
            left_entity_keys: vec!["putnam".into()],
        }
    }

    #[test]
    fn other_side_resolves_via_title_or_entity_key() {
        let e = edge(); // left "Externalism About the Mind" (+entity "putnam") → right "Semantic externalism"
        // matched left via a constituent ENTITY → fetch RIGHT (the exact
        // case the live trace exposed: "Kant" matched left, but the old
        // title-only check returned left and added nothing).
        assert_eq!(e.other_side("Putnam").title, "Semantic externalism");
        // matched left via title → fetch right
        assert_eq!(e.other_side("Externalism About the Mind").title, "Semantic externalism");
        // matched right via title → fetch left
        assert_eq!(e.other_side("Semantic externalism").title, "Externalism About the Mind");
    }

    #[test]
    fn relation_inverse_swaps_broader_narrower() {
        assert_eq!(BridgeRelation::Broader.inverse(), BridgeRelation::Narrower);
        assert_eq!(BridgeRelation::Narrower.inverse(), BridgeRelation::Broader);
        assert_eq!(BridgeRelation::Same.inverse(), BridgeRelation::Same);
        assert_eq!(BridgeRelation::Related.inverse(), BridgeRelation::Related);
    }

    #[test]
    fn snapshot_round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bridge_edges.json");
        let file = BridgeEdgesFile::new(vec![edge()], vec![]);
        write_bridge_edges(&file, &path).unwrap();
        let back = read_bridge_edges(&path).unwrap();
        assert_eq!(back.edges.len(), 1);
        assert_eq!(back.edges[0].relation, BridgeRelation::Same);
        assert_eq!(back.schema_version, BridgeEdgesFile::SCHEMA_VERSION);
    }

    #[test]
    fn edges_for_matches_either_side() {
        let file = BridgeEdgesFile::new(vec![edge()], vec![]);
        assert_eq!(file.edges_for("sep-externalism-mind::externalism-mind").count(), 1);
        assert_eq!(file.edges_for("wikipedia::98765").count(), 1);
        assert_eq!(file.edges_for("wikipedia::00000").count(), 0);
    }

    #[test]
    fn oplog_add_then_remove_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let log = BridgeOplog::new(tmp.path());
        let e = edge();
        log.append(&BridgeOp::add(&e)).unwrap();
        log.append(&BridgeOp::remove(&e, "operator reversed a bad alignment"))
            .unwrap();
        let ops = log.read_all().unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].op, BridgeOpKind::AddEdge);
        assert_eq!(ops[1].op, BridgeOpKind::RemoveEdge);
        assert_eq!(ops[1].rationale, "operator reversed a bad alignment");
    }

    #[test]
    fn oplog_missing_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(BridgeOplog::new(tmp.path()).read_all().unwrap().is_empty());
    }
}
