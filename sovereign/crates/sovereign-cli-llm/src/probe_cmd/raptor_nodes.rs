// SPDX-License-Identifier: AGPL-3.0-or-later
//! The probe's `raptor-nodes` mode (pb-bench-dials-vault): svrn reads a
//! corpus's RAPTOR tree from its own state store, every level, for
//! `svrn bench faithfulness` to judge. No route serves a corpus's tree
//! (`atlas_http` serves one conversation's), and svrn's store is svrn's.

use sovereign_contracts::probe::{
    ProbeEvidence, ProbeRequest, RaptorNodeEvidence, RaptorNodesEvidence,
};

/// The mode: every RAPTOR node of `request.corpus`, level 0 and up. An empty
/// tree is evidence, not an error; the judge decides what it means.
pub(super) async fn probe(request: &ProbeRequest) -> Result<ProbeEvidence, String> {
    let data_dir = sovereign_contracts::rebrand::data_dir();
    let db_path = sovereign_contracts::rebrand::state_db_path(&data_dir);
    let store = sovereign_store::sqlite::SqliteStateStore::open(&db_path)
        .map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let nodes = store
        .list_corpus_raptor_nodes(&request.corpus, 0)
        .await
        .map_err(|e| format!("list raptor nodes for {}: {e}", request.corpus))?;
    tracing::debug!(corpus = %request.corpus, nodes = nodes.len(), db = %db_path.display(), "raptor nodes read");
    Ok(ProbeEvidence::RaptorNodes(RaptorNodesEvidence {
        db_path: db_path.display().to_string(),
        nodes: nodes
            .into_iter()
            .map(|n| RaptorNodeEvidence {
                node_id: n.node_id,
                level: n.level,
                summary: n.summary,
                primary_entities_json: n.primary_entities_json,
                cluster_coherence: n.cluster_coherence,
                direct_member_chunk_ids_json: n.direct_member_chunk_ids_json,
                evidence_chunk_ids_json: n.evidence_chunk_ids_json,
            })
            .collect(),
    }))
}
