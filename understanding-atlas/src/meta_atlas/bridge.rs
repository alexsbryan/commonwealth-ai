// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure half of the cross-corpus topic-to-topic ontological bridge.
//!
//! The bridge promotes the meta-atlas from name-equality `Entity` clustering
//! to a typed concept-alignment graph. The file-driven core — topic nodes,
//! graded alignment signals, and the forced-choice adjudication parse — is
//! arithmetic over the language and lives here. The persisted edge store, its
//! oplog and the lookup I/O stay in `corpus-engine`'s shell and are
//! re-exported at their historical paths.

pub mod adjudicate;
pub mod signals;
pub mod topic_node;

use serde::{Deserialize, Serialize};

/// Typed relation, read FROM the left topic TO the right topic
/// (`left` {relation} `right`).
///
/// Moved here from `corpus-engine`'s HOST `meta_atlas/bridge/edges.rs` by
/// domains `dm-understanding-pure-1` (ralph/DECISIONS.md): pure bridge files
/// (`signals`, `adjudicate`) name it and may not reach the host edge store.
/// `edges.rs` re-exports it at the historical path.
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

/// Which alignment signal contributed to an edge. Persisted on the edge so
/// `meta-atlas explain` can show *why* two topics were linked.
///
/// Moved here from the host edge store by `dm-understanding-pure-1`; see
/// [`BridgeRelation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeSignal {
    /// Normalised name / alias overlap.
    NameMatch,
    /// Concept-embedding cosine.
    Embedding,
    /// Jaccard of the two topics' `entity_keys` (the demoted name-cluster
    /// meta-atom, reused as a feature).
    SharedEntities,
    /// SEP's named entities appear as Wikipedia link-graph neighbours of the
    /// candidate.
    LinkGraphCoNeighbor,
    /// SEP Argument-dominant × Wikipedia Inventory-dominant — the "two
    /// registers" signature.
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
