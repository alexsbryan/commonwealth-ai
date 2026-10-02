// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicU64, Ordering};

use corpus_index::ingest_port::daemon::IngestPort;
use corpus_index::ingest_port::double::IngestPortDouble;
use sovereign_contracts::self_claims::LocalClaims;

use super::*;

/// A node that answers every claim and records the usage it is handed;
/// `storage_remaining` is a fixed budget less that usage.
#[derive(Default)]
struct Budgeted {
    budget: Option<u64>,
    used: AtomicU64,
}

#[async_trait::async_trait]
impl SelfClaims for Budgeted {
    async fn claims(&self) -> LocalClaims {
        LocalClaims {
            availability: 0.5,
            in_flight: Some(3),
            storage_remaining: self
                .budget
                .map(|b| b.saturating_sub(self.used.load(Ordering::SeqCst))),
            embed_model: Some(oicp_types::manifest::EmbedModelInfo {
                model_id: "embed".to_string(),
                dimensions: 8,
                pooling: oicp_types::manifest::PoolingStrategy::Mean,
                normalization: oicp_types::manifest::NormalizationStrategy::Application,
                query_instruction_prefix: String::new(),
            }),
            media_available: None,
        }
    }

    fn record_storage_used(&self, used: u64) {
        self.used.store(used, Ordering::SeqCst);
    }
}

async fn index(indexes: &std::path::Path, id: &str, query_sharing: bool) {
    let idx = corpus_index::index::CorpusIndex::create_with_sharing(
        &indexes.join(id),
        id,
        id,
        "test-embed",
        8,
        false,
        Some(query_sharing),
        "private",
    )
    .await
    .unwrap();
    idx.mark_ingestion_complete().unwrap();
}

/// svrn declares its own answers and no hardware, anchor or benchmark; the
/// budget it declares is what is left after this round's corpus usage; only
/// a query-shared corpus is advertised. Failing inputs: declare the
/// builder's hardware, ask for claims before handing back the usage, or
/// filter on `mesh_sharing`.
#[tokio::test]
async fn svrn_declares_only_its_own_answers() {
    let dir = tempfile::tempdir().unwrap();
    index(dir.path(), "shared", true).await;
    index(dir.path(), "private", false).await;
    let engine: Arc<dyn IngestPort> = Arc::new(
        IngestPortDouble::new()
            .with_index_dir(dir.path().to_path_buf())
            .listing_indexes_under_index_dir(),
    );
    let used: u64 = engine
        .installed_indexes()
        .await
        .unwrap()
        .iter()
        .map(|i| i.index_size_bytes)
        .sum();
    let node = Budgeted {
        budget: Some(used + 10),
        ..Default::default()
    };

    let caps = svrn_claims(Some(&engine), None, 7, &node).await;
    let hosted: Vec<&str> = caps
        .hosted_corpora
        .iter()
        .map(|c| c.corpus_id.as_str())
        .collect();
    assert_eq!(hosted, vec!["shared"]);
    assert_eq!(caps.storage_remaining_bytes, Some(10));
    assert_eq!(
        (
            caps.inference_availability,
            caps.current_in_flight,
            caps.embed_model.as_ref().map(|m| m.model_id.as_str())
        ),
        (0.5, Some(3), Some("embed"))
    );
    assert!(caps.hardware.gpus.is_empty());
    assert_eq!(caps.hardware.system_ram_gb, 0);
    assert!(caps.anchor.is_none());
    assert!(caps.benchmark.is_none());
    assert_eq!(caps.reported_at, 7);

    let unbudgeted = svrn_claims(None::<&Arc<dyn IngestPort>>, None, 0, &Budgeted::default()).await;
    assert!(unbudgeted.hosted_corpora.is_empty());
    assert_eq!(unbudgeted.storage_remaining_bytes, None);
}
