use super::*;
use std::sync::Mutex;

/// Exactly what `sovereign_daemon::routes_internal::mesh_admin::
/// contribution_view` serialises: `Vec<NodeContributionsView>`,
/// plain field names, no serde renames, already sorted by node id
/// (the handler's own `out.sort_by` is the last thing it does).
///
/// Every one of the nine scalars and five nested fields carries a
/// DISTINCT non-zero value, so a field wired to the wrong
/// neighbour fails instead of matching by coincidence. Two nodes,
/// because a one-row fixture cannot show order surviving the hop.
const DAEMON_VIEW_JSON: &str = r#"[
      {
        "node_id": "0a0b0c0d0e0f101112131415161718aa",
        "window_days": 30,
        "inference_served_requests": 11,
        "inference_served_tokens": 22,
        "inference_served_wall_seconds": 33.5,
        "inference_consumed_requests": 44,
        "inference_consumed_tokens": 55,
        "corpora_hosted": [
          {"corpus_id":"sep","corpus_name":"Stanford Encyclopedia",
           "size_gb":6.25,"queries_served":77,"is_sole_host":true},
          {"corpus_id":"gutenberg","corpus_name":"Project Gutenberg",
           "size_gb":8.5,"queries_served":88,"is_sole_host":false}
        ],
        "bytes_served": 99,
        "bytes_received": 100
      },
      {
        "node_id": "ff0b0c0d0e0f101112131415161718bb",
        "window_days": 30,
        "inference_served_requests": 1,
        "inference_served_tokens": 2,
        "inference_served_wall_seconds": 3.0,
        "inference_consumed_requests": 4,
        "inference_consumed_tokens": 5,
        "corpora_hosted": [],
        "bytes_served": 6,
        "bytes_received": 7
      }
    ]"#;

/// NO REGRESSION, field for field.
///
/// Before svt-3 the Local arm built these DTOs in-process from
/// `commonwealth_state::current_contributions` and the Attach arm
/// parsed them from this route. Both arms now parse this route, so
/// what used to be a mapping bug becomes a PARSE bug — and this is
/// where it lands. `NodeContributionsView` has no serde renames
/// (`mesh_admin.rs:1444-1465`), so a field renamed on either side
/// blanks the Members ledger; the comment above that struct says
/// exactly that, and until now nothing enforced it.
#[test]
fn the_daemon_view_parses_into_the_dto_field_for_field() {
    let got: Vec<NodeContributionsDto> =
        serde_json::from_str(DAEMON_VIEW_JSON).expect("the daemon's own shape parses");
    assert_eq!(got.len(), 2);

    let a = &got[0];
    assert_eq!(a.node_id, "0a0b0c0d0e0f101112131415161718aa");
    assert_eq!(a.window_days, 30, "the 30-day default window survives");
    assert_eq!(a.inference_served_requests, 11);
    assert_eq!(a.inference_served_tokens, 22);
    assert_eq!(a.inference_served_wall_seconds, 33.5, "an f64, not rounded");
    assert_eq!(a.inference_consumed_requests, 44);
    assert_eq!(a.inference_consumed_tokens, 55);
    assert_eq!(a.bytes_served, 99);
    assert_eq!(a.bytes_received, 100);

    assert_eq!(a.corpora_hosted.len(), 2, "nested rows are not flattened");
    let sep = &a.corpora_hosted[0];
    assert_eq!(sep.corpus_id, "sep");
    assert_eq!(sep.corpus_name, "Stanford Encyclopedia");
    assert_eq!(sep.size_gb, 6.25);
    assert_eq!(sep.queries_served, 77);
    assert!(sep.is_sole_host, "the sole-host flag is not defaulted");
    assert!(!a.corpora_hosted[1].is_sole_host);

    let b = &got[1];
    assert_eq!(b.node_id, "ff0b0c0d0e0f101112131415161718bb");
    assert!(
        b.corpora_hosted.is_empty(),
        "a peer hosting nothing is an empty list, not a missing key"
    );
    assert_eq!(b.inference_served_wall_seconds, 3.0);
}

/// THE WIRE HOP — the whole of what svt-3 changed.
///
/// Drives the exact expression `mesh_get_contributions` now runs
/// in BOTH modes: `TurnClient::new(internal_base_url)
/// .contribution_view::<Vec<NodeContributionsDto>>()`. Asserts the
/// host saw `/internal/contribution/view` — the same path the
/// deleted hand-rolled `reqwest` call built by hand — and that the
/// answer arrives with its values and its ORDER intact.
///
/// Order is the host's answer, not the client's: the handler sorts
/// by node id and the desktop's `out.sort_by` went with the local
/// arm (ARCH principle 8). So the fixture is served in the host's
/// order and must come back in it.
#[tokio::test]
async fn the_wire_hop_reaches_the_route_and_loses_nothing() {
    use axum::{routing::get, Router};

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);

    let app = Router::new()
        .route(
            "/internal/contribution/view",
            get(move || {
                let recorder = Arc::clone(&recorder);
                async move {
                    recorder
                        .lock()
                        .unwrap()
                        .push("/internal/contribution/view".to_string());
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        DAEMON_VIEW_JSON,
                    )
                }
            }),
        )
        // Anything else is a 404 the client must report as an
        // error, so a client that drifts onto another path fails
        // loudly here rather than returning an empty ledger.
        .fallback(|| async { axum::http::StatusCode::NOT_FOUND });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let got: Vec<NodeContributionsDto> = sovereign_turn_client::TurnClient::new(base)
        .contribution_view()
        .await
        .expect("the daemon's contribution view is readable over the wire");

    assert_eq!(
        *seen.lock().unwrap(),
        ["/internal/contribution/view"],
        "the client asks the route the daemon actually registers \
         (commonwealth-api/src/server.rs:516)"
    );
    assert_eq!(
        got.iter().map(|c| c.node_id.as_str()).collect::<Vec<_>>(),
        [
            "0a0b0c0d0e0f101112131415161718aa",
            "ff0b0c0d0e0f101112131415161718bb"
        ],
        "the host's order arrives unchanged — the client does not re-sort"
    );
    assert_eq!(got[0].inference_served_wall_seconds, 33.5);
    assert_eq!(got[0].corpora_hosted.len(), 2);
    assert_eq!(got[0].corpora_hosted[1].corpus_id, "gutenberg");
    assert_eq!(got[1].bytes_received, 7);
}

/// A host that REFUSES is an error, never an empty ledger.
///
/// The one way this migration could regress silently: a boot that
/// reached no serving host answers `Ok(vec![])`, and that arm is a
/// `bootstrap_mode` check rather than an in-process read. If a
/// failed HTTP call could also produce an empty vec, "the mesh has
/// served nothing" and "the daemon would not answer" would render
/// identically and the operator would have no way to tell
/// (ARCH principle 6). They must not collapse.
#[tokio::test]
async fn a_refusing_host_is_an_error_not_an_empty_ledger() {
    use axum::{routing::get, Router};

    let app = Router::new().route(
        "/internal/contribution/view",
        get(|| async {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "contribution_view: aggregate failed: store closed",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let err = sovereign_turn_client::TurnClient::new(base)
        .contribution_view::<Vec<NodeContributionsDto>>()
        .await
        .expect_err("a 500 is a refusal, not an empty ledger");
    let text = err.to_string();
    assert!(
        text.contains("store closed"),
        "the host's own words reach the operator; got {text}"
    );
}

/// The FRONTEND contract. `mesh_get_contributions` serialises this
/// DTO back over the Tauri bridge, and the Members panel reads
/// these key names. The migration changed where the values come
/// from and must not have changed a single key.
#[test]
fn the_dto_reserialises_with_the_keys_the_members_panel_reads() {
    let got: Vec<NodeContributionsDto> = serde_json::from_str(DAEMON_VIEW_JSON).unwrap();
    let wire = serde_json::to_value(&got).unwrap();
    let row = &wire[0];
    for key in [
        "node_id",
        "window_days",
        "inference_served_requests",
        "inference_served_tokens",
        "inference_served_wall_seconds",
        "inference_consumed_requests",
        "inference_consumed_tokens",
        "corpora_hosted",
        "bytes_served",
        "bytes_received",
    ] {
        assert!(!row[key].is_null(), "the frontend reads `{key}`");
    }
    for key in [
        "corpus_id",
        "corpus_name",
        "size_gb",
        "queries_served",
        "is_sole_host",
    ] {
        assert!(
            !row["corpora_hosted"][0][key].is_null(),
            "the frontend reads `corpora_hosted[].{key}`"
        );
    }
    assert_eq!(
        row.as_object().unwrap().len(),
        10,
        "no key added or dropped"
    );
}

/// The peer-preference half of the same contract (svt-3).
///
/// Exactly what `sovereign_daemon::routes_internal::peer_preference::
/// peer_preference_list` serialises: `Vec<VenuePreferenceDto>`, plain
/// field names, no serde renames. Before svt-3 the Local arm built these
/// DTOs in-process from `commonwealth_state::PeerPreferenceStore::list`
/// and the Attach arm returned an empty list; both arms now parse this
/// route, so what used to be a mapping bug becomes a PARSE bug — and
/// this is where it lands.
///
/// Distinct non-zero values per field, and a second row whose `reason`
/// is absent, because `Option<String>` is the one field a wrong serde
/// attribute can blank without failing.
#[test]
fn the_daemon_preference_view_parses_into_the_dto_field_for_field() {
    const DAEMON_PREFS_JSON: &str = r#"[
          {
            "node_id": "0a0b0c0d0e0f101112131415161718aa",
            "multiplier": 0.25,
            "reason": "throttled while it backfills",
            "set_at": 1757000000
          },
          {
            "node_id": "ff0b0c0d0e0f101112131415161718bb",
            "multiplier": 1.0,
            "reason": null,
            "set_at": 1757000001
          }
        ]"#;

    let got: Vec<VenuePreferenceDto> =
        serde_json::from_str(DAEMON_PREFS_JSON).expect("the daemon's own shape parses");
    assert_eq!(got.len(), 2);

    assert_eq!(got[0].node_id, "0a0b0c0d0e0f101112131415161718aa");
    assert_eq!(got[0].multiplier, 0.25, "an f64, not rounded");
    assert_eq!(
        got[0].reason.as_deref(),
        Some("throttled while it backfills")
    );
    assert_eq!(got[0].set_at, 1_757_000_000);

    assert_eq!(got[1].node_id, "ff0b0c0d0e0f101112131415161718bb");
    assert_eq!(got[1].multiplier, 1.0, "the top of the clamp survives");
    assert_eq!(got[1].reason, None, "an absent note stays absent");
    assert_eq!(got[1].set_at, 1_757_000_001);
}
