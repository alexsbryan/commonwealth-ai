// SPDX-License-Identifier: AGPL-3.0-or-later
//! The append door's write contract (ROOT_CAUSE_FIXES C1 + C3b) and the
//! membership route, against a real cw-rails behind the rail port.
//!
//! These were unit tests over an in-process `RingRail`. The journals are
//! cw-rails' since pb-mesh-exit-transport, so they run here, through the
//! routes, where the daemon actually meets the rail.

use super::*;

/// The operator's view of the namespace's log — loopback, no bearer.
async fn operator_log(state: &AppState) -> serde_json::Value {
    let (status, log) = call(
        state.clone(),
        request(
            "GET",
            &format!("/v1/rail/log?namespace={NS}"),
            LOOPBACK,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{log}");
    log
}

/// An operator append of `body` on [`NS`].
async fn operator_append(state: &AppState, body: serde_json::Value) -> serde_json::Value {
    let (status, out) = call(
        state.clone(),
        request(
            "POST",
            &format!("/v1/rail/append?namespace={NS}"),
            LOOPBACK,
            None,
            Some(body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    out
}

/// **C1: a guest's words must say whose they are.** The name is stamped from
/// the session the door authenticated, never the wire — and a guest who
/// claimed none is REFUSED rather than published as the host's words.
/// Failing input: an append with no session handle, which before C1 landed
/// 200 and read as the host's.
#[tokio::test]
async fn a_guest_append_with_no_name_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key).await,
        vec![Scope::Rails(NS.into())],
    );

    let (status, body) = call(
        state.clone(),
        request(
            "POST",
            "/v1/rail/append",
            LAN_PEER,
            Some(GUEST_TOKEN),
            Some(groceries()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.to_string().contains("whose words"),
        "the sentence names the fix: {body}"
    );
    assert_eq!(
        operator_log(&state).await["held"],
        0,
        "a refused write leaves no trace"
    );
}

/// **C3b: a replayed append yields one act.** The key rides OUTSIDE the act —
/// door state, never the permanent journal — and the door replays the
/// recorded answer. Failing input: two POSTs with one key, which minted two
/// acts. Exactly-once per DOOR PROCESS is the honest scope.
#[tokio::test]
async fn a_replayed_append_yields_one_act() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = state_with_rail(dir.path(), &key).await;
    let mut body = groceries();
    body["idempotency_key"] = "k1".into();

    let first = operator_append(&state, body.clone()).await;
    let replay = operator_append(&state, body).await;
    assert_eq!(first, replay, "the recorded answer, not a second act's");
    assert_eq!(operator_log(&state).await["held"], 1, "one act, not two");
}

/// A key the seed roster does not name, admitted by Alex as "Sam".
async fn admit_sam(state: &AppState) -> (String, serde_json::Value) {
    let stranger = SigningKey::from_bytes(&[7u8; 32]).actor();
    let admitted = operator_append(
        state,
        serde_json::json!({ "op": "admit", "person": "Sam", "key": stranger }),
    )
    .await;
    (stranger, admitted)
}

async fn membership_of(state: &AppState) -> serde_json::Value {
    let (status, v) = call(
        state.clone(),
        request(
            "GET",
            &format!("/v1/rail/membership?namespace={NS}"),
            LOOPBACK,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v
}

fn stands(v: &serde_json::Value, key: &str) -> bool {
    v["membership"]["standing"]
        .as_array()
        .expect("standing is a list")
        .iter()
        .any(|s| s.as_str() == Some(key))
}

/// The membership route answers from ACTS, not the roster file: a stranger
/// holds standing through an `Admit` the seed never names. Failing input: a
/// route that renders the roster back — the stranger is absent there.
#[tokio::test]
async fn membership_reads_standing_from_acts_not_the_roster_file() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = state_with_rail(dir.path(), &key).await;
    let (stranger, _) = admit_sam(&state).await;

    let v = membership_of(&state).await;
    assert!(
        stands(&v, &stranger),
        "the stranger stands through the Admit: {v}"
    );
    assert_eq!(v["membership"]["bindings"][stranger.as_str()], "Sam");
    // The seed ships beside the walk, and it does not carry the stranger —
    // the difference is the question this route answers.
    assert!(
        v["roster"]["members"]["Sam"].is_null(),
        "the roster half must not carry the stranger: {v}"
    );
}

/// Voiding one `Admit` is the leak-undo: standing falls, and because a voided
/// op never enters the walk, no binding is created either (the
/// cumulative-binding rule belongs to the `Remove` cut, leg 4).
#[tokio::test]
async fn voiding_the_admit_drops_standing_and_leaves_no_binding() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = state_with_rail(dir.path(), &key).await;
    let (stranger, admitted) = admit_sam(&state).await;
    operator_append(
        &state,
        serde_json::json!({ "op": "correct", "corrects": admitted["id"], "replacement": null }),
    )
    .await;

    let v = membership_of(&state).await;
    assert!(!stands(&v, &stranger), "the void dropped the stranger: {v}");
    assert!(
        v["membership"]["bindings"][stranger.as_str()].is_null(),
        "a voided Admit leaves no binding: {v}"
    );
}
