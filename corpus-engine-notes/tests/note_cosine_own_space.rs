// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Order `mesh-scale-t1-notes`, bar `t1-notes-own-space` — the PAST half.
//
// `ingest_remote_notes` no longer stores a peer's vector, so no NEW
// foreign-space row can appear. That says nothing about the rows a
// store already holds: every note this node ingested before 2026-08-13
// carries whatever `model_id` the sender stamped on it, and on a
// heterogeneous mesh some of those are from another model's space. The
// read has to exclude them, and this file is the test for that.
//
// Its own binary, deliberately: it mutates `SOVEREIGN_EMBED_MODEL_ID`
// mid-test to manufacture a two-space store through the SHIPPED write
// path (rather than reaching into SQLite, which an integration test
// cannot do anyway — rusqlite is not a dev-dependency). Cargo runs each
// integration test file as its own process, so the mutation cannot race
// another test.
//
// NOT a duplicate of `red_baseline_cross_model_notes.rs` ARM 2, which
// covers the same read filter against a foreign row that arrived from a
// PEER. This one has nothing to do with the mesh: it is the operator who
// changed embed models on a single node, stranding that node's own older
// rows in a space its queries no longer live in. Same filter, different
// provenance, and the second one is reachable on a solo install.
//
// Spec: MESH_SCALE_100_USERS_1000_CORPORA.md §8.3.2.

use corpus_engine_notes::{EmbedFn, NoteStore, ScopeFilter};
use std::sync::Arc;

const SPACE_A: &str = "retired-embed-model-a";
const SPACE_B: &str = "qwen-embedding-0.6b";

#[tokio::test]
async fn a_row_written_in_a_retired_model_space_is_not_scored_against_local_queries() {
    // Weight 1.0 with no FTS query: the result set IS the cosine pool,
    // so nothing has to be inferred about which stage admitted a row.
    std::env::set_var("SOVEREIGN_NOTES_EMBED_WEIGHT", "1.0");

    let dir = tempfile::tempdir().unwrap();
    // Every text embeds to the same unit vector, so both notes below
    // are at cosine 1.0 and ranking cannot explain an absence — only
    // the model-space filter can.
    let embed: EmbedFn = Arc::new(|_text: &str| {
        let v = vec![1.0f32, 0.0, 0.0, 0.0];
        Box::pin(async move { Ok(v) })
    });
    let store = NoteStore::open(&dir.path().join("notes.db"))
        .unwrap()
        .with_embed_fn(embed);

    // ── The store's past: a note embedded under the old model ───────
    std::env::set_var("SOVEREIGN_EMBED_MODEL_ID", SPACE_A);
    let stale_id = store
        .write_note(
            "decision",
            "embedded back when this node ran a different model",
            vec![],
            vec![],
            "own-space",
        )
        .await
        .expect("write stale-space note");

    // ── The store's present ─────────────────────────────────────────
    std::env::set_var("SOVEREIGN_EMBED_MODEL_ID", SPACE_B);
    let fresh_id = store
        .write_note(
            "decision",
            "embedded under the model this node runs now",
            vec![],
            vec![],
            "own-space",
        )
        .await
        .expect("write local-space note");

    let ids: Vec<String> = store
        .read_notes_scoped_semantic(
            None,
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
        .collect();

    // ── Instrument validation, asserted before the finding ──────────
    // If the semantic path fell back to FTS, NOTHING comes back and the
    // exclusion below passes vacuously.
    assert!(
        ids.contains(&fresh_id),
        "instrument check FAILED: the local-space note did not come back, so \
         the cosine path never ran — this run measured the harness, not the \
         system. ids={ids:?}"
    );

    // ── The bar ─────────────────────────────────────────────────────
    assert!(
        !ids.contains(&stale_id),
        "a row in the {SPACE_A} space was scored against a {SPACE_B} query \
         vector and returned as a semantic hit. Cosine between two model \
         spaces is not a weak signal, it is not a signal. ids={ids:?}"
    );
}
