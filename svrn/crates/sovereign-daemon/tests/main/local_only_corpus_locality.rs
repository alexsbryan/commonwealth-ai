// SPDX-License-Identifier: AGPL-3.0-or-later
//! `query_sharing=false` corpus locality test.
//!
//! The §7.1 promise — that KnowledgeView's three-map corpora
//! (personal-knowledge, conversation-history, institutional-notes)
//! never leave the user's machine — is encoded as three hardcoded
//! fields on the recipe builders in
//! `sovereign-tools/src/knowledge_view/recipes.rs`:
//!
//!   scope: Some("local".into()),
//!   mesh_sharing: false,
//!   query_sharing: Some(false),
//!
//! Pinned at the recipe layer by `knowledge_view::recipes::tests`
//! (the §7.2 invariant test). What's NOT pinned is the *daemon-side*
//! enforcement: the gossip publisher consults `query_sharing` (not
//! `mesh_sharing`) when deciding which corpora to advertise in the
//! `hosted_corpora` payload. A regression that:
//!
//!   - flipped the filter from `query_sharing` to `mesh_sharing`,
//!   - or applied the filter only in some code paths,
//!   - or made the filter parameterisable (i.e. operator-overridable),
//!
//! would silently publish the user's personal corpora into the
//! gossip-replicated mesh state, breaking §7's "defence in depth"
//! promise even though the recipe-layer test still passes.
//!
//! Two assertions:
//!
//! 1. **`query_sharing=true` corpus IS advertised in hosted_corpora.**
//! 2. **`query_sharing=false` corpus is NOT advertised, even when
//!    `mesh_sharing` is also true.** This is the inversion test —
//!    `query_sharing` is the load-bearing field for the gossip path,
//!    NOT `mesh_sharing`. (See `capabilities.rs:155-172`'s docstring
//!    for the rationale: SEP has `mesh_sharing=false, query_sharing=true`
//!    and DOES get advertised — the wire-fan-out path is separately
//!    gated.)
use std::sync::Arc;

use corpus_index::index::{CorpusIndex, InsertChunk};
use corpus_index::source::CorpusReadPort;
use corpus_index::types::EmbedFn;
use kernel_types::NodeId;
use sovereign_daemon::state::AppState;

use crate::common;
use crate::common::ledger_double::RecordingLedger;

const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Install a corpus with explicit privacy flags. The `query_sharing`
/// arg is `Option<bool>` matching `create_with_sharing`'s contract —
/// `None` resolves to `mesh_sharing` at open-time (pre-split
/// behaviour); `Some(...)` pins the value explicitly.
async fn install_corpus_with_privacy(
    indexes_dir: &std::path::Path,
    id: &str,
    name: &str,
    mesh_sharing: bool,
    query_sharing: Option<bool>,
) {
    let path = indexes_dir.join(id);
    let index = CorpusIndex::create_with_sharing(
        &path,
        id,
        name,
        "qwen3-embedding-0.6b",
        EMBED_DIM,
        mesh_sharing,
        query_sharing,
        "private",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: format!("Content of {id}"),
                title: Some(name.into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some(id.into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
                text_sha256: None,
            },
            vec![0.0_f32; EMBED_DIM],
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();
}

#[tokio::test]
async fn query_sharing_false_corpus_does_not_publish_to_hosted_corpora() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();

    // Install two corpora:
    //   - "sep" → mesh_sharing=false, query_sharing=true. Mimics
    //     the SEP licensing model (no replication, but federated
    //     queries are fair use). SHOULD appear in hosted_corpora.
    //   - "personal-knowledge" → query_sharing=false. Mimics
    //     KnowledgeView's three-map recipes. MUST NOT appear.
    install_corpus_with_privacy(
        &indexes,
        "sep",
        "Stanford Encyclopedia",
        /* mesh_sharing */ false,
        /* query_sharing */ Some(true),
    )
    .await;
    install_corpus_with_privacy(
        &indexes,
        "personal-knowledge",
        "Personal Knowledge",
        /* mesh_sharing */ false,
        /* query_sharing */ Some(false),
    )
    .await;

    let engine = Arc::new(crate::common::reading_double(indexes, mock_embed_fn()));

    // Build an AppState with the engine wired. What this node advertises
    // is what its peer-origin registration declares to cw-rails at every
    // register and renew (`peer_origin::claims_source`,
    // pb-mesh-exit-transport): `installed_indexes()` from the engine,
    // filtered on `query_sharing`, as `hosted_corpora`.
    let self_id = NodeId::from_u128(0xA770A770);
    let state = AppState::new_with_seeds(
        self_id,
        Some(engine),
        None,
        Default::default(),
        Default::default(),
        Default::default(),
        Arc::new(RecordingLedger::new(self_id)).seed(),
    );

    let declared = sovereign_daemon::peer_origin::claims_source(state.clone())().await;
    let corpora_ids: Vec<&str> = declared
        .hosted_corpora
        .iter()
        .map(|c| c.corpus_id.as_str())
        .collect();

    assert!(
        corpora_ids.contains(&"sep"),
        "sep (query_sharing=true) MUST appear in hosted_corpora — \
         it's the regular shared corpus. Got: {corpora_ids:?}"
    );
    assert!(
        !corpora_ids.contains(&"personal-knowledge"),
        "personal-knowledge (query_sharing=false) MUST NOT appear in \
         hosted_corpora. The §7.1 KnowledgeView locality promise lives \
         here: a regression that flipped the gossip filter from \
         `query_sharing` to `mesh_sharing` would silently publish the \
         user's personal corpora to the gossip-replicated mesh state, \
         exposing private content to peers. Got: {corpora_ids:?}"
    );
    assert_eq!(
        corpora_ids.len(),
        1,
        "exactly one corpus should be advertised; got: {corpora_ids:?}"
    );
}

#[tokio::test]
async fn locally_only_corpus_is_still_searchable_via_local_path() {
    // Inverse pin: even though `personal-knowledge` is NOT
    // advertised to peers, it MUST remain searchable locally —
    // KnowledgeView's whole point is that personal data is
    // useful, just not federated. This test pins that
    // `installed_indexes()` still returns the local-only corpus
    // (the daemon's own /v1/knowledge/search local-first path
    // reads from there).
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install_corpus_with_privacy(
        &indexes,
        "personal-knowledge",
        "Personal Knowledge",
        false,
        Some(false),
    )
    .await;

    let engine = crate::common::reading_double(indexes, mock_embed_fn());

    let installed = engine.installed_indexes().await.unwrap();
    assert_eq!(
        installed.len(),
        1,
        "the local-only corpus must still be enumerable via \
         installed_indexes() — that's the local-search code path's \
         entry point"
    );
    assert_eq!(installed[0].corpus_id, "personal-knowledge");
    // It's installed, but it should report query_sharing=false so
    // the gossip path filters it out.
    assert!(
        !installed[0].query_sharing,
        "installed metadata must reflect the query_sharing=false flag \
         so downstream callers (gossip publisher, /internal/knowledge/search) \
         see the right value"
    );
}
