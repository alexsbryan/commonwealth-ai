// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the checkpoint route — see `ring_checkpoint.rs`.
//!
//! The digest test is the row's own bar: the document's stated digest must
//! equal `commonwealth_rail::digest` recomputed over the ops the document
//! carries, parsed back out of the carried lines. That round trip is what
//! makes the completeness claim checkable rather than asserted.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use commonwealth_rail::{Person, RailAct, RingSigner, Roster};
use tower::ServiceExt;

use super::*;
use crate::server::internal_router;

/// A fixed-key signer whose actor is a stable hex string, so the roster file
/// can name it. The route never admits — signatures are checked by `admit`,
/// which this route does not run — so a deterministic body is enough.
struct Fixed;
impl RingSigner for Fixed {
    fn actor(&self) -> String {
        "ab".repeat(32)
    }
    fn sign(&self, _ns: &str, _ts: i64, _seq: u64, _body: &str) -> String {
        "cd".repeat(64)
    }
}

/// A rail on a temp dir with `ns` rostered to Fixed's key.
fn rail_with_roster(root: &std::path::Path, ns: &str) -> Arc<commonwealth_rail::RingRail> {
    let rail = Arc::new(commonwealth_rail::RingRail::new(root, Arc::new(Fixed)));
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("Ada"), vec![Fixed.actor()]);
    rail.journal(ns)
        .unwrap()
        .set_roster(&Roster::new(members))
        .unwrap();
    rail
}

/// A state whose rail is `rail`.
fn state_with(rail: Arc<commonwealth_rail::RingRail>) -> crate::state::AppState {
    crate::state::test_app_state_with_seed(crate::state::fabric::FabricSeed {
        ring_rail: Some(rail),
        ..Default::default()
    })
}

async fn get(ns: &str, state: crate::state::AppState) -> axum::http::Response<Body> {
    internal_router(state)
        .oneshot(
            Request::get(format!("/internal/ring/checkpoint/{ns}"))
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    54321,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("the route must answer, not hang")
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).expect("a JSON body")
}

/// Append one record act so the namespace under test holds a real op.
async fn append_record(rail: &commonwealth_rail::RingRail, ns: &str, amount: u64) {
    let journal = rail.journal(ns).unwrap();
    let roster = rail.roster(&journal).await.unwrap();
    journal
        .append(
            RailAct::Record {
                payload: commonwealth_rail::Payload::new(serde_json::json!({
                    "kind": "expense", "amount": amount,
                }))
                .unwrap(),
            },
            rail.signer(),
            &roster,
            None,
        )
        .unwrap_or_else(|e| panic!("append to {ns}: {e}"));
}

/// The v1 document's shape: every field TL names, present and honest.
#[tokio::test]
async fn a_checkpoint_of_a_live_namespace_has_the_v1_shape() {
    let dir = tempfile::tempdir().unwrap();
    let rail = rail_with_roster(dir.path(), "house-expenses");
    append_record(&rail, "house-expenses", 12).await;
    append_record(&rail, "house-expenses", 7).await;

    let response = get("house-expenses", state_with(rail)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let doc = body_json(response).await;

    assert_eq!(doc["v"], 1);
    assert_eq!(doc["ns"], "house-expenses");
    let created = doc["created_unix"]
        .as_u64()
        .expect("created_unix is a number");
    assert!(
        created > 1_600_000_000,
        "created_unix is a unix timestamp in seconds, got {created}"
    );
    let ops = doc["ops"].as_array().expect("ops is an array");
    assert_eq!(ops.len(), 2, "the journal's two lines, carried");
    for line in ops {
        let line = line.as_str().expect("each op is the journal line verbatim");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("a line parses");
        assert!(
            parsed["seq"].is_u64(),
            "a journal line, not a summary: {parsed}"
        );
        assert!(
            parsed["sig"].is_string(),
            "signed, as it was written: {parsed}"
        );
    }
}

/// The row's round-trip bar: the stated digest is what the rail computes
/// over the ops the document carries — parsed back out of those very lines.
#[tokio::test]
async fn the_stated_digest_is_what_the_carried_ops_compute_to() {
    let dir = tempfile::tempdir().unwrap();
    let rail = rail_with_roster(dir.path(), "house-expenses");
    append_record(&rail, "house-expenses", 12).await;
    append_record(&rail, "house-expenses", 7).await;
    append_record(&rail, "house-expenses", 3).await;

    let response = get("house-expenses", state_with(rail)).await;
    let doc = body_json(response).await;

    // TL step 1: parse the carried lines as ordinary journal lines — the
    // digest is then recomputed over exactly what a verifier would admit.
    let ops: Vec<commonwealth_rail::Op<commonwealth_rail::SignedOp>> = doc["ops"]
        .as_array()
        .expect("ops is an array of lines")
        .iter()
        .map(|line| {
            serde_json::from_str(line.as_str().expect("each op is a line"))
                .expect("a line parses as an ordinary journal line")
        })
        .collect();
    let stated: commonwealth_rail::Digest =
        serde_json::from_value(doc["digest"].clone()).expect("digest is the rail's Digest shape");
    assert_eq!(
        commonwealth_rail::digest(&ops),
        stated,
        "the completeness claim: the document holds every act through every mark it names"
    );
}

/// The roster the append path admitted under is IN the document — a verifier
/// with no roster of its own can still weigh every signature.
#[tokio::test]
async fn the_roster_the_append_path_uses_is_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let rail = rail_with_roster(dir.path(), "house-expenses");
    append_record(&rail, "house-expenses", 12).await;

    let response = get("house-expenses", state_with(rail.clone())).await;
    let doc = body_json(response).await;

    let embedded: Roster = serde_json::from_value(doc["roster"].clone())
        .expect("the embedded roster is the rail's Roster shape");
    let journal = rail.journal("house-expenses").unwrap();
    let live = rail
        .roster(&journal)
        .await
        .expect("the roster the append path used");
    assert_eq!(
        embedded, live,
        "the document carries the append path's roster"
    );
    assert_eq!(
        embedded
            .members
            .get(&Person::from("Ada"))
            .map(Vec::as_slice),
        Some(&[Fixed.actor()][..]),
        "the signer that wrote the act is named in it"
    );
}

/// A namespace this node does not hold is refused by name, and no empty
/// ring is materialised for a typo.
#[tokio::test]
async fn a_namespace_this_node_does_not_hold_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let rail = rail_with_roster(dir.path(), "house-expenses");

    let response = get("no-such-ring", state_with(rail)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let doc = body_json(response).await;
    let refusal = doc["error"].as_str().expect("a sentence");
    assert!(refusal.contains("no-such-ring"), "{refusal}");
    assert!(
        !dir.path().join("rings").join("no-such-ring").exists(),
        "a refused namespace must not be created on touch"
    );
}

/// A journal that cannot be read is a 500 with the read's own sentence —
/// never an empty document dressed as a quiet ring.
#[tokio::test]
async fn a_journal_read_error_is_refused_and_not_silently_empty() {
    let dir = tempfile::tempdir().unwrap();
    let rail = rail_with_roster(dir.path(), "house-expenses");
    // The journal's file, made unreadable as a path: a directory where the
    // JSONL belongs makes every read an io error on every platform.
    let journal = rail.journal("house-expenses").unwrap();
    let file = <commonwealth_rail::SignedOp as commonwealth_rail::Journaled>::FILE;
    std::fs::create_dir(journal.dir().join(file)).unwrap();

    let response = get("house-expenses", state_with(rail)).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let doc = body_json(response).await;
    let refusal = doc["error"].as_str().expect("a sentence");
    assert!(
        !refusal.is_empty(),
        "the read's own sentence, not a bare status"
    );
}
