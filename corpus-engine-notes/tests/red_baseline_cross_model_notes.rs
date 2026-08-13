// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Bar `t1-notes-own-space`. Red baseline: order `mesh-scale-t1-red`.
// Fix: order `mesh-scale-t1-notes`.
// Spec: research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md §7.2
// ("Shipped note embeddings are a live correctness bug, not just waste"), §8.3.
//
// ── What this file was, and why it changed shape ────────────────────
//
// Committed 2026-08-13 `#[ignore]`d and watched failing, as the red-first
// test for the contamination defect:
//
//   * `ingest_remote_notes` wrote the remote `model_id` and vector straight
//     into `note_embeddings` with no comparison against the local embed
//     model (`notes.rs:2263-2277` at fde73931).
//   * `fetch_cosine_pool` selected `e.embedding` only — `e.model_id` was
//     never projected or compared, so every stored vector was scored
//     against the local query vector regardless of which model space
//     produced it (`notes.rs:454-515` at fde73931).
//
//   RED: a foreign-space embedding (model_id=foreign-embed-model-b, local
//   model is qwen-embedding-0.6b) was blended into the cosine pool and
//   returned as a semantic hit. ids=["note-local-space", "note-foreign-space"]
//
// The original single test asserted the defect through a PROXY: that the
// note id `note-foreign-space` was absent from the cosine pool. The fix
// makes that proxy invalid, and not by weakening it — the fix re-embeds
// the remote note's content in the LOCAL space, which legitimately puts
// that note in the pool carrying a vector that is now comparable. The
// note belongs there; its *foreign vector* does not.
//
// So the proxy is replaced by the property it stood for, split across the
// two cases that actually differ, and the original assertion is kept
// VERBATIM on the arm where it is still the right assertion:
//
//   ARM 1 (the future) — a peer ships a foreign vector. Assert on what
//   landed in `note_embeddings`: both rows in the LOCAL model space, and
//   the stored bytes are the locally-computed ones, NOT the shipped ones.
//   This is a stronger claim than the id proxy ever made, because it
//   reads storage instead of inferring from ranking.
//
//   ARM 2 (the past) — a row already on disk in the pre-2026-08-13 shape,
//   written directly into `note_embeddings` with a foreign `model_id`.
//   Ingest cannot fix these; only the read filter can. Here the original
//   `!ids.contains(&"note-foreign-space")` assertion is exactly right and
//   is preserved word for word.
//
// Both arms assert the instrument BEFORE the finding: the same-space note
// must come back first, or a dark cosine path would let the finding pass
// vacuously. DO NOT "fix" either arm by weakening its assertion — the
// assertions ARE the bar.
//
// Watched failing against pre-fix `notes.rs` (fde73931) before being
// un-`#[ignore]`d; see the order's landing docket.
//
// Run it:
//   cargo test -p corpus-engine-notes --test red_baseline_cross_model_notes

use corpus_engine_notes::{
    EmbedFn, ExportedNoteEmbedding, ExportedNoteRow, NotePropagationEvent, NoteStore, ScopeFilter,
};
use std::path::Path;
use std::sync::Arc;

/// The id the local store stamps on its own embeddings (`notes.rs`
/// reads `SOVEREIGN_EMBED_MODEL_ID`, defaulting to this).
const LOCAL_MODEL: &str = "qwen-embedding-0.6b";
/// A peer on a different embed model — the heterogeneous-mesh case.
const FOREIGN_MODEL: &str = "foreign-embed-model-b";

/// What the local `embed_fn` below produces for any text. Every stored
/// vector is compared against this, so "was it re-embedded here?" is a
/// byte comparison rather than an inference.
const LOCAL_VECTOR: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
/// What the peers ship. Deliberately NOT `LOCAL_VECTOR`: if the shipped
/// bytes equalled the local ones, "the stored blob is the local one"
/// would be true whether or not the fix worked.
const SHIPPED_BY_FOREIGN_PEER: [f32; 4] = [0.0, 1.0, 0.0, 0.0];
const SHIPPED_BY_LOCAL_PEER: [f32; 4] = [0.0, 0.0, 1.0, 0.0];

fn le_bytes(v: &[f32]) -> Vec<u8> {
    // Same encoding as the crate-private `embedding_to_le_bytes`
    // (`notes.rs`), reproduced here because it is pub(crate).
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn wire_note(
    id: &str,
    content: &str,
    embedding: Option<ExportedNoteEmbedding>,
) -> NotePropagationEvent {
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
            origin_node_id: Some(format!("peer-{id}")),
        },
        embedding,
        entities: vec![],
        tombstone: false,
        updated_at: 1_700_000_000,
        sent_at: Some(1_700_000_000),
    }
}

fn shipped(model_id: &str, vec: &[f32]) -> Option<ExportedNoteEmbedding> {
    Some(ExportedNoteEmbedding {
        model_id: model_id.to_string(),
        dim: vec.len() as i64,
        embedding: le_bytes(vec),
    })
}

/// The local model space: every query and every note embeds to the same
/// unit vector, so a stored vector's cosine is decided entirely by the
/// vector, not by the query text. Deterministic and dependency-free.
fn local_embed() -> EmbedFn {
    Arc::new(|_text: &str| {
        let v = LOCAL_VECTOR.to_vec();
        Box::pin(async move { Ok(v) })
    })
}

fn pure_cosine_env() {
    std::env::set_var("SOVEREIGN_EMBED_MODEL_ID", LOCAL_MODEL);
    // Pure-cosine blend: with weight 1.0 and no FTS query, everything the
    // read returns came out of `fetch_cosine_pool`, so the result set IS
    // the pool. No inference about which stage admitted a row is needed.
    std::env::set_var("SOVEREIGN_NOTES_EMBED_WEIGHT", "1.0");
}

async fn cosine_pool_ids(store: &NoteStore) -> Vec<String> {
    store
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
        .expect("read_notes_scoped_semantic")
        .into_iter()
        .map(|n| n.id)
        .collect()
}

/// Read back what actually landed in `note_embeddings`. Reading storage
/// is the point: the original test inferred the model space from ranking,
/// and ranking is exactly what the fix changes.
fn stored_embedding(db: &Path, note_id: &str) -> Option<(String, Vec<u8>)> {
    let conn = rusqlite::Connection::open(db).expect("open notes.db for verification");
    conn.query_row(
        "SELECT model_id, embedding FROM note_embeddings WHERE note_id = ?",
        rusqlite::params![note_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

// ── ARM 1 — the future: a shipped vector is never adopted ────────────

#[tokio::test]
async fn a_shipped_foreign_space_vector_is_discarded_and_re_embedded_locally() {
    pure_cosine_env();

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("notes.db");
    let store = NoteStore::open(&db).unwrap().with_embed_fn(local_embed());

    // Two remote notes arrive over gossip in the pre-strip wire shape.
    // Identical but for which model space produced the vector they carry.
    let report = store
        .ingest_remote_notes(vec![
            wire_note(
                "note-local-space",
                "peer on the same embed model",
                shipped(LOCAL_MODEL, &SHIPPED_BY_LOCAL_PEER),
            ),
            wire_note(
                "note-foreign-space",
                "peer on a different embed model",
                shipped(FOREIGN_MODEL, &SHIPPED_BY_FOREIGN_PEER),
            ),
        ])
        .await
        .expect("ingest_remote_notes");
    assert_eq!(
        report.inserted, 2,
        "instrument check: both wire notes must land before anything is \
         claimed about their embeddings (report: {report:?})"
    );

    // ── Instrument validation, asserted before the finding ──────────────
    // If the semantic path silently fell back to FTS (weight unset,
    // embed_fn missing, scope rejected), NOTHING would come back and the
    // assertions below would be describing an inert system.
    let ids = cosine_pool_ids(&store).await;
    assert!(
        ids.contains(&"note-local-space".to_string()),
        "instrument check FAILED: the same-space note did not come back, so \
         the cosine path never ran — this run measured the harness, not the \
         system. ids={ids:?}"
    );

    // ── The bar ─────────────────────────────────────────────────────────
    // A vector from another model's space is not comparable to the local
    // query vector. The fix does not merely exclude it — it discards it
    // and embeds the note's content HERE, so the note keeps its recall
    // and the pool keeps one metric space. Both halves are asserted
    // against storage, not against ranking.
    let local_bytes = le_bytes(&LOCAL_VECTOR);
    for id in ["note-local-space", "note-foreign-space"] {
        let (model_id, blob) = stored_embedding(&db, id).unwrap_or_else(|| {
            panic!("no note_embeddings row for {id} — the note was stored but never embedded")
        });
        assert_eq!(
            model_id, LOCAL_MODEL,
            "RED: {id} was stored in the {model_id} space. Every row in \
             note_embeddings must be in the local space ({LOCAL_MODEL}) or \
             `fetch_cosine_pool` is comparing vectors that have no metric \
             relationship. `ingest_remote_notes` must re-embed, not adopt."
        );
        assert_eq!(
            blob, local_bytes,
            "RED: {id}'s stored vector is not the one this node computed. \
             `ingest_remote_notes` stored the SENDER's bytes — a model_id \
             label is a field the sender supplies, so a matching label is \
             not evidence the vector is in our space (ARCH §18.1)."
        );
    }

    // And the note itself is not lost in the process: re-embedding is what
    // keeps a peer's note reachable by semantic recall at all.
    assert!(
        ids.contains(&"note-foreign-space".to_string()),
        "the re-embedded note vanished from the cosine pool — discarding a \
         foreign vector must not cost the note its recall. ids={ids:?}"
    );
}

// ── ARM 2 — the past: rows already on disk in the old shape ──────────

#[tokio::test]
async fn red_baseline_foreign_space_embedding_must_not_enter_the_cosine_pool() {
    pure_cosine_env();

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("notes.db");

    // Manufacture the pre-2026-08-13 on-disk state. Ingest with no embed
    // hook so the store writes note rows and no embedding rows, then write
    // the embedding rows directly — which is exactly what the old
    // `ingest_remote_notes` did with a peer's shipped vector, foreign
    // `model_id` and all. `ingest_remote_notes` can no longer produce this
    // state; every store that ran the old build still contains it.
    {
        let store = NoteStore::open(&db).unwrap();
        let report = store
            .ingest_remote_notes(vec![
                wire_note("note-local-space", "peer on the same embed model", None),
                wire_note(
                    "note-foreign-space",
                    "peer on a different embed model",
                    None,
                ),
            ])
            .await
            .expect("ingest_remote_notes");
        assert_eq!(
            report.inserted, 2,
            "instrument check: both notes must land before their embedding \
             rows are written (report: {report:?})"
        );
    }
    {
        let conn = rusqlite::Connection::open(&db).expect("open notes.db to seed legacy rows");
        // Same vector for both; they differ ONLY in which model space is
        // recorded as having produced it. Ranking therefore cannot explain
        // an absence — only the model-space filter can.
        for (note_id, model_id) in [
            ("note-local-space", LOCAL_MODEL),
            ("note-foreign-space", FOREIGN_MODEL),
        ] {
            conn.execute(
                "INSERT INTO note_embeddings (note_id, embedding, model_id, dim, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    note_id,
                    le_bytes(&LOCAL_VECTOR),
                    model_id,
                    LOCAL_VECTOR.len() as i64,
                    1_700_000_000i64
                ],
            )
            .expect("seed legacy note_embeddings row");
        }
    }

    let store = NoteStore::open(&db).unwrap().with_embed_fn(local_embed());
    let ids = cosine_pool_ids(&store).await;
    let ids: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();

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
