// SPDX-License-Identifier: AGPL-3.0-or-later
//! The raptor-nodes probe's evidence ([`super::ProbeMode::RaptorNodes`];
//! pb-bench-dials-vault): a corpus's stored RAPTOR tree as svrn's state store
//! holds it, every level, with the fields a faithfulness judge reads. The
//! corpus is [`super::ProbeRequest::corpus`].

use serde::{Deserialize, Serialize};

/// A corpus's RAPTOR nodes, in the store's order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaptorNodesEvidence {
    /// The state store the nodes were read from, so an empty tree can name
    /// where it looked.
    pub db_path: String,
    /// Every node at or above level 0.
    pub nodes: Vec<RaptorNodeEvidence>,
}

/// One RAPTOR node: its summary and the chunks it claims to summarise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaptorNodeEvidence {
    /// The node's id.
    pub node_id: String,
    /// Its tree level (0 = leaves' parents).
    pub level: i64,
    /// The summary text.
    pub summary: String,
    /// JSON array of the node's primary entities.
    pub primary_entities_json: String,
    /// The cluster's coherence; 1.0 with no entities marks a single-node
    /// sentinel tree.
    pub cluster_coherence: f64,
    /// JSON array of the node's direct member chunk ids, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_member_chunk_ids_json: Option<String>,
    /// JSON array of the node's evidence chunk ids.
    pub evidence_chunk_ids_json: String,
}
