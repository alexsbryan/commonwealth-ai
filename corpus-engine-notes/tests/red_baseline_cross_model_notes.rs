// SPDX-License-Identifier: AGPL-3.0-or-later
//
// RED BASELINE — order `mesh-scale-t1-red`, bar `t1-notes-own-space`.
// Spec: research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md §7.2
// ("Shipped note embeddings are a live correctness bug, not just waste"), §8.3.
//
// THIS TEST IS EXPECTED TO FAIL ON TODAY'S CODE. It is committed `#[ignore]`d
// so the build gate stays green while the failure stays recorded; it is the
// red-first test the Tier-1 build order (`t1-notes-own-space`) turns green.
// DO NOT "fix" it by weakening the assertion — the assertion IS the bar.
//
// The defect, verified 2026-08-13:
//   * `ingest_remote_notes` writes the remote `model_id` and vector straight
//     into `note_embeddings` with no comparison against the local embed model
//     (`notes.rs:2263-2277`).
//   * `fetch_cosine_pool` selects `e.embedding` only — `e.model_id` is never
//     projected or compared, so every stored vector is scored against the
//     local query vector regardless of which model space produced it
//     (`notes.rs:454-515`).
// Net: on a heterogeneous mesh, a peer running a different embed model
// injects foreign-space vectors that compete for T1 recall on equal terms.
//
// Run it:
//   cargo test -p corpus-engine-notes --test red_baseline_cross_model_notes \
//     -- --ignored --nocapture

use corpus_engine_notes::{
    EmbedFn, ExportedNoteEmbedding, ExportedNoteRow, NotePropagationEvent, NoteStore, ScopeFilter,
};
use std::sync::Arc;

/// The id the local store stamps on its own embeddings (`notes.rs:1809`
/// reads `SOVEREIGN_EMBED_MODEL_ID`, defaulting to this).
const LOCAL_MODEL: &str = "qwen-embedding-0.6b";
/// A peer on a different embed model — the heterogeneous-mesh case.
const FOREIGN_MODEL: &str = "foreign-embed-model-b";

fn le_bytes(v: &[f32]) -> Vec<u8> {
    // Same encoding as the crate-private `embedding_to_le_bytes`
    // (`notes.rs:573-579`), reproduced here because it is pub(crate).
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn wire_note(id: &str, content: &str, model_id: &str, vec: &[f32]) -> NotePropagationEvent {
    NotePropagationEvent {
        content_hash: format!("hash-{id}"),
        note: ExportedNoteRow {
            id: id.to_string(),
            kind: "decision".to_string(),
            content: content.to_string(),
            symbols: vec![],
            files: vec![],
            session_id: "red-baseline".to_string(),
            created_at: 1_700_000_000,
            scope: "global".to_string(),
            feature_id: None,
            related_entity: None,
            source: "agent".to_string(),
            supersedes: None,
            payload_json: None,
            origin_node_id: Some(format!("peer-{model_id}")),
        },
        embedding: Some(ExportedNoteEmbedding {
            model_id: model_id.to_string(),
            dim: vec.len() as i64,
            embedding: le_bytes(vec),
        }),
        entities: vec![],
        tombstone: false,
        updated_at: 1_700_000_000,
        sent_at: Some(1_700_000_000),
    }
}

#[tokio::test]
#[ignore = "RED BASELINE — fails on today's code by design (bar t1-notes-own-space)"]
async fn red_baseline_foreign_space_embedding_must_not_enter_the_cosine_pool() {
    std::env::set_var("SOVEREIGN_EMBED_MODEL_ID", LOCAL_MODEL);
    // Pure-cosine blend: with weight 1.0 and no FTS query, everything the
    // read returns came out of `fetch_cosine_pool`, so the result set IS the
    // pool. No inference about which stage admitted a row is needed.
    std::env::set_var("SOVEREIGN_NOTES_EMBED_WEIGHT", "1.0");

    let dir = tempfile::tempdir().unwrap();
    // The local space: every query embeds to the same unit vector, so a
    // stored vector's cosine is decided entirely by the vector, not by the
    // query text. Deterministic and dependency-free.
    let embed: EmbedFn = Arc::new(|_text: &str| {
        let v = vec![1.0f32, 0.0, 0.0, 0.0];
        Box::pin(async move { Ok(v) })
    });
    let store = NoteStore::open(&dir.path().join("notes.db"))
        .unwrap()
        .with_embed_fn(embed);

    // Two remote notes arrive over gossip. Identical shape, identical vector,
    // differing ONLY in which model space produced it.
    let report = store
        .ingest_remote_notes(vec![
            wire_note(
                "note-local-space",
                "peer on the same embed model",
                LOCAL_MODEL,
                &[1.0, 0.0, 0.0, 0.0],
            ),
            wire_note(
                "note-foreign-space",
                "peer on a different embed model",
                FOREIGN_MODEL,
                &[1.0, 0.0, 0.0, 0.0],
            ),
        ])
        .await
        .expect("ingest_remote_notes");
    assert_eq!(
        report.inserted, 2,
        "instrument check: both wire notes must land before anything is \
         claimed about ranking (report: {report:?})"
    );

    let hits = store
        .read_notes_scoped_semantic(
            None, // no FTS query — the result set is the cosine pool
            &[],
            &[],
            &[],
            10,
            false,
            &ScopeFilter::default(),
            Some("which model space am I searching in?"),
        )
        .await
        .expect("read_notes_scoped_semantic");
    let ids: Vec<&str> = hits.iter().map(|n| n.id.as_str()).collect();

    // ── Instrument validation, asserted before the finding ──────────────
    // If the semantic path silently fell back to FTS (weight unset, embed_fn
    // missing, scope rejected), NOTHING would be returned and the
    // contamination assertion below would pass vacuously. A red baseline
    // that passes because the instrument was dark is worse than no baseline.
    assert!(
        ids.contains(&"note-local-space"),
        "instrument check FAILED: the same-space note did not come back, so \
         the cosine path never ran — this run measured the harness, not the \
         system. ids={ids:?}"
    );

    // ── The bar ─────────────────────────────────────────────────────────
    // A vector from another model's space is not comparable to the local
    // query vector; it must be excluded (or re-embedded locally at ingest)
    // rather than scored as a peer of local vectors.
    assert!(
        !ids.contains(&"note-foreign-space"),
        "RED: a foreign-space embedding (model_id={FOREIGN_MODEL}, local model \
         is {LOCAL_MODEL}) was blended into the cosine pool and returned as a \
         semantic hit. `fetch_cosine_pool` (notes.rs:454-515) never projects \
         `e.model_id`, and `ingest_remote_notes` (notes.rs:2263-2277) stores \
         the remote vector verbatim. ids={ids:?}"
    );
}
