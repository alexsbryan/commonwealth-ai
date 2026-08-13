// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Order `mesh-scale-t1-notes`, bars `t1-notes-clean-wire` +
// `t1-notes-own-space`. Green counterpart to the two red baselines in
// `red_baseline_note_wire_size.rs` / `red_baseline_cross_model_notes.rs`.
//
// Three things are asserted here, and each has a named failing input:
//
//   1. SIZE — a serialized `NotePropagationEvent` stays under 2 KB even
//      when the event in hand is carrying a full 1024-dim embedding.
//      The failing input is the pre-strip serializer: the same event,
//      serialized through a byte-for-byte mirror of the old struct, is
//      ~16 KB, and this test measures BOTH and prints the ratio, so a
//      regression that re-attaches the vector cannot pass.
//
//   2. MIXED MESH — a peer on the pre-strip build must still be able to
//      decode our events, and we must still be able to decode theirs.
//      The failing input for the first direction is omitting the field
//      instead of writing `null`: the old struct has no
//      `#[serde(default)]` on `embedding`, so a missing key is a decode
//      ERROR on that peer. `LegacyNotePropagationEvent` below is that
//      peer, reproduced field-for-field.
//
//   3. INGEST — a shipped vector is discarded and the content is
//      re-embedded through the LOCAL `embed_fn`; a note that arrives
//      when no `embed_fn` is wired is stored but stays out of the
//      cosine pool until the backfill embeds it.
//
// Spec: research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md
// §8.3.1 (16.1 KB/note measured, cliff ~520) and §8.3.2.

use corpus_engine_notes::{
    EmbedFn, ExportedNoteEmbedding, ExportedNoteRow, NotePropagationEvent, NoteStore, ScopeFilter,
};
use std::sync::Arc;

/// Matches the default in `notes.rs::local_embed_model_id`. Set
/// explicitly so the assertions do not depend on the ambient env.
const LOCAL_MODEL: &str = "qwen-embedding-0.6b";
const FOREIGN_MODEL: &str = "foreign-embed-model-b";

/// The bar from the order: ≤ ~2 KB per gossiped note, which puts the
/// 8 MiB `MAX_REQUEST_BODY_BYTES` push limit (`server.rs:30`) at
/// ≥ ~4,000 notes instead of the measured ~520.
const WIRE_BUDGET_BYTES: usize = 2048;
const BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// Production embedding width on this mesh — every embedding in the
/// live store is 1024-dim `qwen-embedding-0.6b` (§8.3.1).
const PROD_DIM: usize = 1024;

/// A note body at the measured p50 of the real store: §8.3.1 puts the
/// serialized note body (embedding stripped) at a 1,274-byte p50 and a
/// 1,591-byte mean. Using a body at the p90 (2,417 B) would exceed the
/// budget on its own; using an empty one would measure the harness. This
/// sits at the mean.
fn realistic_note_content() -> String {
    "decision: the gossip wire carries the note and not the vector. ".repeat(24)
}

fn wire_event(
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
            symbols: vec!["NoteStore::ingest_remote_notes".to_string()],
            files: vec!["corpus-engine-notes/src/notes.rs".to_string()],
            session_id: "wire-shapes".to_string(),
            created_at: 1_700_000_000,
            scope: "global".to_string(),
            feature_id: None,
            related_entity: None,
            source: "agent".to_string(),
            supersedes: None,
            payload_json: None,
            origin_node_id: Some("peer-1".to_string()),
        },
        embedding,
        entities: vec![],
        tombstone: false,
        updated_at: 1_700_000_000,
        sent_at: Some(1_700_000_000),
    }
}

/// A production-width embedding: 1024 dims → a 4,096-byte LE blob.
///
/// The bytes come from a seeded LCG rather than a smooth ramp because
/// what costs bytes on the wire is the DECIMAL WIDTH of each element of
/// the JSON array, and a ramp of small f32s produces many one- and
/// two-digit bytes. Uniformly distributed bytes average 2.57 decimal
/// digits, which reproduces the 14.4-14.6 KB payload measured over 500
/// real notes (§8.3.1); a ramp lands ~4% under it and would make this
/// fixture quietly weaker than the thing it stands in for.
fn prod_sized_embedding(model_id: &str) -> ExportedNoteEmbedding {
    let mut state: u32 = 0x5EED_1234;
    let bytes: Vec<u8> = (0..PROD_DIM * 4)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect();
    ExportedNoteEmbedding {
        model_id: model_id.to_string(),
        dim: PROD_DIM as i64,
        embedding: bytes,
    }
}

// ── The pre-strip peer, reproduced ──────────────────────────────────
//
// Field-for-field mirror of `NotePropagationEvent` as it stood at
// commit fde73931 — in particular `embedding` carries NO
// `#[serde(default)]`, which is exactly why the new serializer writes
// `null` rather than omitting the key.

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LegacyNotePropagationEvent {
    content_hash: String,
    note: LegacyExportedNoteRow,
    embedding: Option<LegacyExportedNoteEmbedding>,
    #[serde(default)]
    entities: Vec<LegacyExportedNoteEntity>,
    tombstone: bool,
    updated_at: i64,
    #[serde(default)]
    sent_at: Option<i64>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LegacyExportedNoteRow {
    id: String,
    kind: String,
    content: String,
    symbols: Vec<String>,
    files: Vec<String>,
    session_id: String,
    created_at: i64,
    scope: String,
    feature_id: Option<String>,
    related_entity: Option<String>,
    source: String,
    supersedes: Option<String>,
    payload_json: Option<String>,
    origin_node_id: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LegacyExportedNoteEmbedding {
    model_id: String,
    dim: i64,
    embedding: Vec<u8>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LegacyExportedNoteEntity {
    entity: String,
    kind: String,
}

impl LegacyNotePropagationEvent {
    /// The pre-strip wire copy of a live event — what the daemon's sink
    /// used to hand `serde_json::to_vec` (`bootstrap.rs:1340-1347`).
    fn from_current(ev: &NotePropagationEvent) -> Self {
        LegacyNotePropagationEvent {
            content_hash: ev.content_hash.clone(),
            note: LegacyExportedNoteRow {
                id: ev.note.id.clone(),
                kind: ev.note.kind.clone(),
                content: ev.note.content.clone(),
                symbols: ev.note.symbols.clone(),
                files: ev.note.files.clone(),
                session_id: ev.note.session_id.clone(),
                created_at: ev.note.created_at,
                scope: ev.note.scope.clone(),
                feature_id: ev.note.feature_id.clone(),
                related_entity: ev.note.related_entity.clone(),
                source: ev.note.source.clone(),
                supersedes: ev.note.supersedes.clone(),
                payload_json: ev.note.payload_json.clone(),
                origin_node_id: ev.note.origin_node_id.clone(),
            },
            embedding: ev.embedding.as_ref().map(|e| LegacyExportedNoteEmbedding {
                model_id: e.model_id.clone(),
                dim: e.dim,
                embedding: e.embedding.clone(),
            }),
            entities: Vec::new(),
            tombstone: ev.tombstone,
            updated_at: ev.updated_at,
            sent_at: ev.sent_at,
        }
    }
}

// ── 1. SIZE ─────────────────────────────────────────────────────────

#[test]
fn gossiped_note_event_stays_under_the_wire_budget() {
    // The adversarial input: an event that IS holding a full
    // production-width embedding. If the budget were met only because
    // the constructors happen to pass `None` today, this event would
    // blow it — the guarantee has to come from the serializer.
    let ev = wire_event(
        "note-sized",
        &realistic_note_content(),
        Some(prod_sized_embedding(LOCAL_MODEL)),
    );

    let now = serde_json::to_vec(&ev).expect("serialize current event");
    let before = serde_json::to_vec(&LegacyNotePropagationEvent::from_current(&ev))
        .expect("serialize legacy");

    println!(
        "T1_WIRE green={} red={} ratio={:.1}x notes_to_8MiB green={:.0} red={:.0}",
        now.len(),
        before.len(),
        before.len() as f64 / now.len() as f64,
        BODY_LIMIT_BYTES as f64 / now.len() as f64,
        BODY_LIMIT_BYTES as f64 / before.len() as f64,
    );

    // ── Instrument validation, asserted before the finding ──────────
    // A serializer that dropped the note body would also come in under
    // budget. Prove we measured a real note first.
    let json = String::from_utf8(now.clone()).expect("utf8");
    assert!(
        json.contains("note-sized") && json.contains("the gossip wire carries the note"),
        "instrument check FAILED: the serialized event does not contain the \
         note body, so this run measured an empty struct, not a note"
    );
    // §8.3.1 measured the embedding payload alone at 14,443-14,643
    // bytes (p50 14,545) across 500 real notes. If this fixture's
    // payload is smaller than that, it is not production-width and the
    // budget below is unearned.
    let payload = before.len() - now.len();
    assert!(
        payload >= 14_000,
        "instrument check FAILED: the embedding payload in this fixture is \
         {payload} bytes, under the 14.4-14.6 KB measured on the real store \
         (§8.3.1) — the fixture is not production-width and the budget below \
         is unearned"
    );

    // ── The bar ─────────────────────────────────────────────────────
    assert!(
        !json.contains("\"embedding\":["),
        "REGRESSION: the embedding vector is back on the wire. This is the \
         14.5 KB that put the 8 MiB push limit at ~520 notes \
         (MESH_SCALE_100_USERS_1000_CORPORA.md §8.3.1)."
    );
    assert!(
        now.len() <= WIRE_BUDGET_BYTES,
        "REGRESSION: a gossiped note serializes to {} bytes, over the {} byte \
         bar — the 8 MiB push limit lands at {:.0} notes.",
        now.len(),
        WIRE_BUDGET_BYTES,
        BODY_LIMIT_BYTES as f64 / now.len() as f64,
    );
}

// ── 2. MIXED MESH ───────────────────────────────────────────────────

#[test]
fn a_pre_strip_peer_can_still_decode_our_events() {
    let ev = wire_event(
        "note-compat",
        "mixed mesh",
        Some(prod_sized_embedding(LOCAL_MODEL)),
    );
    let bytes = serde_json::to_vec(&ev).expect("serialize");

    let decoded: LegacyNotePropagationEvent = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "a peer on the pre-strip build could not decode our event ({e}) — \
             its `embedding` field has no #[serde(default)], so the key must be \
             written as null rather than omitted. Wire: {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    assert_eq!(decoded.note.id, "note-compat");
    assert!(
        decoded.embedding.is_none(),
        "the old peer must see an absent vector, not a stale one"
    );
}

#[test]
fn we_can_still_decode_a_pre_strip_peer_s_events() {
    let ev = wire_event(
        "note-legacy",
        "old build",
        Some(prod_sized_embedding(FOREIGN_MODEL)),
    );
    let legacy_bytes = serde_json::to_vec(&LegacyNotePropagationEvent::from_current(&ev))
        .expect("serialize legacy");

    let decoded: NotePropagationEvent =
        serde_json::from_slice(&legacy_bytes).expect("decode a pre-strip peer's event");
    assert_eq!(decoded.note.id, "note-legacy");
    let emb = decoded.embedding.as_ref().expect(
        "the legacy shape's vector must still deserialize — ingest is \
                where it gets discarded, not the decoder",
    );
    assert_eq!(emb.model_id, FOREIGN_MODEL);
}

// ── 3. INGEST ───────────────────────────────────────────────────────

/// Every query and every note embeds to the same unit vector, so a
/// stored vector's cosine is decided by the vector, not the text.
fn unit_embed() -> EmbedFn {
    Arc::new(|_text: &str| {
        let v = vec![1.0f32, 0.0, 0.0, 0.0];
        Box::pin(async move { Ok(v) })
    })
}

fn pure_cosine_env() {
    std::env::set_var("SOVEREIGN_EMBED_MODEL_ID", LOCAL_MODEL);
    // Weight 1.0 with no FTS query: everything the read returns came
    // out of `fetch_cosine_pool`, so the result set IS the pool.
    std::env::set_var("SOVEREIGN_NOTES_EMBED_WEIGHT", "1.0");
}

async fn cosine_ids(store: &NoteStore) -> Vec<String> {
    store
        .read_notes_scoped_semantic(
            None,
            &[],
            &[],
            &[],
            10,
            false,
            &ScopeFilter::default(),
            Some("which notes are in the cosine pool?"),
        )
        .await
        .expect("read_notes_scoped_semantic")
        .into_iter()
        .map(|n| n.id)
        .collect()
}

#[tokio::test]
async fn a_shipped_foreign_vector_is_discarded_and_the_note_re_embedded_locally() {
    pure_cosine_env();
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db"))
        .unwrap()
        .with_embed_fn(unit_embed());

    // The pre-strip wire shape, from a peer on a different embed model.
    let report = store
        .ingest_remote_notes(vec![wire_event(
            "note-from-old-peer",
            "a peer on a different embed model",
            Some(prod_sized_embedding(FOREIGN_MODEL)),
        )])
        .await
        .expect("ingest_remote_notes");

    assert_eq!(
        report.inserted, 1,
        "instrument check: the note must land ({report:?})"
    );
    assert_eq!(
        report.foreign_embeddings_discarded, 1,
        "the shipped vector must be counted as discarded, not silently \
         tolerated ({report:?})"
    );
    assert_eq!(
        report.embeddings_recomputed, 1,
        "the note must be re-embedded HERE, in the local space ({report:?})"
    );
    assert_eq!(report.embeddings_deferred, 0, "{report:?}");

    // Behavioural proof that the row is in the LOCAL space: the cosine
    // pool only admits rows stamped with the local model id, so being
    // returned at all is the assertion. Had the shipped vector been
    // stored verbatim (the pre-fix behaviour) the row would carry
    // `foreign-embed-model-b` and the pool would drop it.
    let ids = cosine_ids(&store).await;
    assert!(
        ids.contains(&"note-from-old-peer".to_string()),
        "the re-embedded note is missing from the cosine pool, so either the \
         re-embed did not happen or it wrote a foreign model id. ids={ids:?}"
    );
}

#[tokio::test]
async fn a_stripped_wire_note_is_embedded_locally_at_ingest() {
    pure_cosine_env();
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db"))
        .unwrap()
        .with_embed_fn(unit_embed());

    // The post-strip wire shape: no vector at all.
    let report = store
        .ingest_remote_notes(vec![wire_event(
            "note-from-new-peer",
            "a peer on the stripped wire",
            None,
        )])
        .await
        .expect("ingest_remote_notes");

    assert_eq!(report.inserted, 1, "{report:?}");
    assert_eq!(report.foreign_embeddings_discarded, 0, "{report:?}");
    assert_eq!(
        report.embeddings_recomputed, 1,
        "a note that arrives without a vector must be embedded here, not left \
         semantically invisible ({report:?})"
    );

    let ids = cosine_ids(&store).await;
    assert!(
        ids.contains(&"note-from-new-peer".to_string()),
        "ids={ids:?}"
    );
}

#[tokio::test]
async fn re_ingesting_the_same_batch_does_not_re_embed() {
    pure_cosine_env();
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db"))
        .unwrap()
        .with_embed_fn(unit_embed());

    let batch = || vec![wire_event("note-repeat", "the poller re-scans", None)];
    let first = store.ingest_remote_notes(batch()).await.expect("first");
    assert_eq!(first.embeddings_recomputed, 1, "{first:?}");

    // The daemon's ingest poller re-scans the same MeshStore entries
    // every 10 s (`bootstrap.rs:1510`). If dedup did not gate the
    // embed, every tick would re-embed the entire mesh corpus.
    let second = store.ingest_remote_notes(batch()).await.expect("second");
    assert_eq!(second.deduplicated, 1, "{second:?}");
    assert_eq!(
        second.embeddings_recomputed, 0,
        "a deduplicated event must not spend an embed call ({second:?})"
    );
}

#[tokio::test]
async fn with_no_embed_hook_the_note_is_stored_and_kept_out_of_the_cosine_pool() {
    pure_cosine_env();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("notes.db");

    // No `with_embed_fn`: the local embed slot is unavailable, which is
    // the daemon's state before the model loads.
    let store = NoteStore::open(&db).unwrap();
    let report = store
        .ingest_remote_notes(vec![wire_event(
            "note-unembedded",
            "arrived before the embed slot was up",
            Some(prod_sized_embedding(FOREIGN_MODEL)),
        )])
        .await
        .expect("ingest_remote_notes");

    assert_eq!(
        report.inserted, 1,
        "never dropped: the note is stored even when it cannot be embedded \
         ({report:?})"
    );
    assert_eq!(
        report.embeddings_deferred, 1,
        "absence is reported, not defaulted ({report:?})"
    );
    assert_eq!(report.embeddings_recomputed, 0, "{report:?}");
    assert_eq!(report.foreign_embeddings_discarded, 1, "{report:?}");

    // Never blended unembedded: the note is readable by the keyword
    // path but absent from the cosine pool.
    let by_text = store
        .read_notes(Some("embed slot"), &[], &[], &[], 10, false)
        .await
        .expect("read_notes");
    assert!(
        by_text.iter().any(|n| n.id == "note-unembedded"),
        "the note must still be READABLE — it was stored, just not embedded"
    );

    // Backfill closes the loop: wire the hook and the deferred note
    // joins the pool, in the local space.
    drop(store);
    let store = NoteStore::open(&db).unwrap().with_embed_fn(unit_embed());
    let backfill = store.backfill_tier_artifacts(0).await;
    assert_eq!(
        backfill.embeddings_backfilled, 1,
        "the deferred note must be picked up by the backfill ({backfill:?})"
    );
    let ids = cosine_ids(&store).await;
    assert!(
        ids.contains(&"note-unembedded".to_string()),
        "after backfill the note must be in the cosine pool. ids={ids:?}"
    );
}
