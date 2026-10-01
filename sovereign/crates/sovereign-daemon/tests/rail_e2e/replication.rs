// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sealing through the append door.
//!
//! Split out of `main.rs`, which the ring-rail work pushed into the
//! 800-1200 approach band (ARCH §3.1). The fixtures stay in `main.rs`.
//! Replication through svrn's `/internal/ring/sync` route, and the
//! convergence ceiling beside it, went with that route: the ring round is
//! cw-rails' since pb-mesh-exit-transport, and its tests are cw-rails'.

use super::*;

/// **An app seals through the door it already writes with, and the journal
/// gets shorter.** No second verb, no capability an app has to be granted
/// separately, no local setting — `{"op":"seal"}` is an act like any other, it
/// takes the author's next `seq`, and the prune it authorises happens in the
/// same request.
///
/// The `retired` block is the point. Before it, an app could seal and had no
/// way to learn whether anything was actually removed: the seal's own 200
/// looks identical whether the journal shrank by a thousand lines or was
/// refused outright.
#[tokio::test]
async fn an_app_seals_through_the_append_door_and_the_journal_shrinks() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let post = |body: serde_json::Value| {
        let state = state.clone();
        async move {
            call(
                state,
                request(
                    "POST",
                    "/v1/rail/append",
                    LAN_PEER,
                    Some(GUEST_TOKEN),
                    Some(body),
                ),
            )
            .await
        }
    };

    for _ in 0..3 {
        let (status, body) = post(groceries()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    // An ordinary append says nothing about retention, because it retires
    // nothing — the field is absent rather than a zero that reads like a prune
    // that found nothing.
    let (_, plain) = post(groceries()).await;
    assert!(plain.get("retired").is_none(), "{plain}");

    let (status, sealed) = post(serde_json::json!({ "op": "seal" })).await;
    assert_eq!(status, StatusCode::OK, "{sealed}");
    assert_eq!(sealed["seq"], 4, "a seal is the author's next act");
    assert_eq!(sealed["retired"]["removed"], 4, "{sealed}");
    assert_eq!(sealed["retired"]["kept"], 1, "the seal itself: {sealed}");

    let (status, log) = call(
        state,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{log}");
    assert_eq!(log["held"], 1, "the journal is four lines shorter: {log}");
    assert_eq!(
        log["complete"], true,
        "a compacted node is not a broken one: {}",
        log["gaps"]
    );
    // The seal is delivery, not meaning: it is on the log and no reducer sees
    // a payload for it.
    let ops = log["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1);
    assert!(
        ops[0].get("payload").map(|p| p.is_null()).unwrap_or(true),
        "{log}"
    );
}
