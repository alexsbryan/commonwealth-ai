// SPDX-License-Identifier: AGPL-3.0-or-later
//! What svrn declares about this node at every register and renew: only
//! what svrn owns (phase-b-83 (1)). Hosted corpora, embed model,
//! availability, in-flight and the storage budget left. The node's
//! hardware and live load are cw-rails' to measure (commonwealth-rails
//! `self_measure`), which clamps its free storage to the budget declared
//! here (phase-b-91). Moved from sovereign-mesh `capabilities.rs`, whose
//! hardware half went to cw-rails.

use std::sync::Arc;

use corpus_engine_atlas_reader::ports::AtlasPort;
use corpus_index::source::CorpusReadPort;
use corpus_index::types::IndexInfo;
use oicp_types::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use oicp_types::knowledge::{ChunkRange, CorpusShardInfo};
use sovereign_contracts::self_claims::SelfClaims;
use tracing::{debug, warn};

use super::TRACE_TARGET;

/// svrn's declaration now. The corpus walk's usage is handed back through
/// `claims_source` before it is asked, so the budget left reflects this
/// round's sum (the order `SelfClaims` documents).
pub(super) async fn svrn_claims<E: CorpusReadPort + ?Sized>(
    engine: Option<&Arc<E>>,
    atlas: Option<&Arc<dyn AtlasPort>>,
    now_secs: u64,
    claims_source: &dyn SelfClaims,
) -> NodeCapabilities {
    // One walk serves both the corpus list and the usage sum, so the two
    // never see different sets of corpora.
    let installed = match engine {
        Some(e) => match e.installed_indexes().await {
            Ok(idxs) => Some(idxs),
            Err(err) => {
                // No corpora this round and usage 0, so the budget is not
                // read as spent while the indexes are unreadable.
                warn!(target: TRACE_TARGET, error = %err, "peer origin: installed_indexes failed");
                None
            }
        },
        None => None,
    };
    let storage_used_bytes: u64 = installed
        .as_deref()
        .map(|idxs| idxs.iter().map(|i| i.index_size_bytes).sum())
        .unwrap_or(0);
    let hosted_corpora = match (engine, installed.as_deref()) {
        (Some(e), Some(idxs)) => hosted_corpora(&**e, atlas, idxs),
        _ => Vec::new(),
    };
    claims_source.record_storage_used(storage_used_bytes);
    let claims = claims_source.claims().await;

    NodeCapabilities {
        hardware: HardwareProfile {
            gpus: Vec::new(),
            system_ram_gb: 0,
            cpu_cores: 0,
            total_storage_gb: 0,
            free_storage_gb: 0,
            network_bandwidth_mbps: None,
        },
        available: AvailableResources::default(),
        active_processes: Vec::new(),
        hosted_corpora,
        reported_at: now_secs,
        inference_availability: claims.availability,
        inference_capable: false,
        loaded_models: Vec::new(),
        origins: Vec::new(),
        media_allow: Vec::new(),
        media_available: None,
        // The collaborative-ingestion planner matches candidates on this
        // exactly; `None` keeps a node with no embed slot out of it.
        embed_model: claims.embed_model,
        // Never set: an advertised benchmark arms the size-ratio
        // extrapolation in oicp-types scoring.rs `throughput_factor`, which
        // SCHEDULER_QUALITY.md §4.5 measured at −56% and filed DO-NOT-BUILD.
        // A real measurement belongs in sovereign_serve::mesh_measurements.
        benchmark: None,
        current_in_flight: claims.in_flight,
        // serve's to declare (its rpc registration), never svrn's.
        anchor: None,
        storage_remaining_bytes: claims.storage_remaining,
    }
}

/// Each installed index peers may query, as the shard record they route on.
///
/// Filtered on `query_sharing`, not `mesh_sharing`: the first gates
/// federated search against this copy (this advertisement), the second
/// gates replicating its bytes. SEP is `mesh_sharing=false,
/// query_sharing=true`; a private codebase corpus is false on both. An index
/// whose meta predates the split resolves `query_sharing` from
/// `mesh_sharing` at open.
fn hosted_corpora<E: CorpusReadPort + ?Sized>(
    engine: &E,
    atlas: Option<&Arc<dyn AtlasPort>>,
    indexes: &[IndexInfo],
) -> Vec<CorpusShardInfo> {
    let indexes_dir = engine.index_dir().to_path_buf();
    indexes
        .iter()
        .filter(|idx| idx.query_sharing)
        .cloned()
        .map(|idx| {
            // An unreadable summary advertises zero atoms this round, named.
            let summary = atlas.and_then(|a| {
                match a.atlas_summary(&indexes_dir.join(&idx.corpus_id).join("atlas")) {
                    Ok(s) => s,
                    Err(e) => {
                        debug!(target: TRACE_TARGET, corpus = %idx.corpus_id, error = %e,
                               "peer origin: atlas summary unread; advertising no atlas");
                        None
                    }
                }
            });
            let (atom_count, tier2_count, fingerprint) = match summary {
                Some(s) => (s.atom_count, s.tier2_count, Some(s.fingerprint)),
                None => (0, 0, None),
            };
            CorpusShardInfo {
                corpus_id: idx.corpus_id,
                // The engine's storage range and the gossip wire's are
                // distinct types of one shape: copy.
                chunk_range: idx.chunk_range.map(|r| ChunkRange {
                    start_id: r.start_id,
                    end_id: r.end_id,
                }),
                is_replica: idx.is_shard && idx.chunk_range.is_some(),
                last_updated: idx.last_updated,
                // Peers compute coverage_ratio() on these to pick which
                // canonical to pull when several mirror one corpus.
                chunk_count: idx.chunk_count,
                canonical_fingerprint: idx.canonical_fingerprint,
                total_shards: idx.total_shards,
                processed_shards: idx.processed_shards,
                atlas_atom_count: atom_count,
                atlas_tier2_count: tier2_count,
                atlas_fingerprint: fingerprint,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "claims/tests.rs"]
mod tests;
