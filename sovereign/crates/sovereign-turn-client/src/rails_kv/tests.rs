// SPDX-License-Identifier: AGPL-3.0-or-later
//! A dead serving process is a named `Backend` absence within the timeout,
//! from a multi-thread worker and from a current-thread runtime alike.

use std::time::{Duration, Instant};

use sovereign_contracts::peer::{ReplicatedKv, ReplicatedKvError};

use super::RailsKv;

/// Port 1 on loopback: nothing listens, so every dial is refused.
const DEAD: &str = "http://127.0.0.1:1";

fn assert_absent<T: std::fmt::Debug>(r: Result<T, ReplicatedKvError>, path: &str) {
    let ReplicatedKvError::Backend(e) = r.expect_err("a dead dial has no answer");
    assert!(
        e.contains(&format!("{DEAD}{path}")),
        "the absence names the URL: {e}"
    );
}

fn every_verb_is_absent() {
    let kv = RailsKv::new(DEAD);
    let started = Instant::now();
    assert_absent(kv.get("a", "k"), "/v1/mesh/kv/entry");
    assert_absent(
        kv.set(
            "a",
            "k",
            bytes::Bytes::from_static(b"v"),
            sovereign_contracts::principal::NodeId::from_u128(1),
        ),
        "/v1/mesh/kv/entry",
    );
    assert_absent(kv.delete("a", "k"), "/v1/mesh/kv/entry");
    assert_absent(kv.scan("a", ""), "/v1/mesh/kv/entries");
    assert!(
        started.elapsed() < 4 * Duration::from_secs(2) + Duration::from_secs(1),
        "four dead dials answered inside their timeouts: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_door_is_a_named_absence_from_a_worker() {
    every_verb_is_absent();
}

#[tokio::test]
async fn a_dead_door_is_a_named_absence_from_a_current_thread_runtime() {
    every_verb_is_absent();
}

#[test]
fn a_dead_door_is_a_named_absence_off_any_runtime() {
    every_verb_is_absent();
}

// ── Against a door ─────────────────────────────────────────────────────────
// A stand-in door serving the contracts' wire over a map, NOT cw-rails'
// KvHost: a real KvHost would be a svrn -> cmnwlth dev edge (five-programs-57).
// What the stand-in cannot catch — drift in cw-rails' hand-encoded bodies — is
// pinned by `cw_rails_to_entry_literal_decodes` below and cw-rails' kv/tests.

mod door {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use axum::extract::{Query, State};
    use axum::routing::get;
    use axum::{Json, Router};
    use bytes::Bytes;
    use sovereign_contracts::peer::{
        KvLookup, KvScanQuery, KvSetBody, ReplicatedKv, ReplicatedKvEntry,
    };
    use sovereign_contracts::principal::NodeId;

    use super::super::RailsKv;

    type Store = Arc<Mutex<BTreeMap<(String, String), ReplicatedKvEntry>>>;

    fn stand_in() -> Router {
        Router::new()
            .route(
                "/v1/mesh/kv/entry",
                get(
                    |State(s): State<Store>, Query(q): Query<KvLookup>| async move {
                        Json(s.lock().unwrap().get(&(q.app_id, q.key)).cloned())
                    },
                )
                .post(
                    |State(s): State<Store>, Json(b): Json<KvSetBody>| async move {
                        let e = ReplicatedKvEntry {
                            app_id: b.app_id.clone(),
                            key: b.key.clone(),
                            value: b.value,
                            timestamp: 1,
                            origin: b.origin,
                        };
                        let prev = s.lock().unwrap().insert((b.app_id, b.key), e.clone());
                        Json(prev.map(|p| p.value != e.value).unwrap_or(true))
                    },
                )
                .delete(
                    |State(s): State<Store>, Query(q): Query<KvLookup>| async move {
                        Json(s.lock().unwrap().remove(&(q.app_id, q.key)).is_some())
                    },
                ),
            )
            .route(
                "/v1/mesh/kv/entries",
                get(
                    |State(s): State<Store>, Query(q): Query<KvScanQuery>| async move {
                        Json(
                            s.lock()
                                .unwrap()
                                .values()
                                .filter(|e| e.app_id == q.app_id && e.key.starts_with(&q.prefix))
                                .cloned()
                                .collect::<Vec<_>>(),
                        )
                    },
                ),
            )
            .with_state(Store::default())
    }

    /// Serve `app` on its own thread and runtime; the base URL.
    fn serve_on_own_thread(app: Router) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(format!("http://{}", l.local_addr().unwrap()))
                    .unwrap();
                axum::serve(l, app).await.unwrap();
            });
        });
        rx.recv().unwrap()
    }

    /// Serve `app` on the caller's (multi-thread) runtime; the base URL.
    async fn serve_here(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        base
    }

    fn round_trip(kv: &RailsKv) {
        let me = NodeId::from_u128(7);
        assert_eq!(kv.get("a", "k").unwrap(), None);
        assert!(kv.set("a", "k", Bytes::from_static(b"v1"), me).unwrap());
        let got = kv.get("a", "k").unwrap().unwrap();
        assert_eq!(&got.value[..], b"v1");
        assert_eq!(got.origin, me);
        assert!(kv.set("a", "k2", Bytes::from_static(b"v2"), me).unwrap());
        assert_eq!(kv.scan("a", "k").unwrap().len(), 2);
        assert!(kv.delete("a", "k").unwrap());
        assert!(!kv.delete("a", "k").unwrap());
        assert_eq!(kv.scan("a", "").unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_set_delete_scan_round_trip_from_a_worker() {
        round_trip(&RailsKv::new(serve_here(stand_in()).await));
    }

    #[tokio::test]
    async fn get_set_delete_scan_round_trip_from_a_current_thread_runtime() {
        round_trip(&RailsKv::new(serve_on_own_thread(stand_in())));
    }

    /// -56's falsifier: p99 over ~50 ms stops the flip. Printed, not gated —
    /// a loaded CI host is not the measurement.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_get_round_trip_is_measured() {
        let kv = RailsKv::new(serve_here(stand_in()).await);
        kv.set("a", "k", Bytes::from_static(b"v"), NodeId::from_u128(7))
            .unwrap();
        let mut us: Vec<u128> = (0..400)
            .map(|_| {
                let t = Instant::now();
                kv.get("a", "k").unwrap().unwrap();
                t.elapsed().as_micros()
            })
            .collect();
        us.sort_unstable();
        println!(
            "MEASURED rails-kv get round trip n={} p50={}us p99={}us max={}us",
            us.len(),
            us[us.len() / 2],
            us[us.len() * 99 / 100],
            us[us.len() - 1]
        );
    }

    /// cw-rails' `to_entry` (commonwealth-rails/src/kv.rs) encodes by hand;
    /// this is its shape written out, the same literal cw-rails' own
    /// kv/tests pin. `origin` is `NodeId`'s 16-byte array, `value` base64.
    fn to_entry_literal() -> serde_json::Value {
        serde_json::json!({
            "app_id": "ns",
            "key": "claims/a",
            "value": "aGVsbG8=",
            "timestamp": 42,
            "origin": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7],
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cw_rails_to_entry_literal_decodes() {
        let app = Router::new()
            .route(
                "/v1/mesh/kv/entry",
                get(|| async { Json(to_entry_literal()) }),
            )
            .route(
                "/v1/mesh/kv/entries",
                get(|| async { Json(serde_json::json!([to_entry_literal()])) }),
            );
        let kv = RailsKv::new(serve_here(app).await);
        let want = ReplicatedKvEntry {
            app_id: "ns".into(),
            key: "claims/a".into(),
            value: Bytes::from_static(b"hello"),
            timestamp: 42,
            origin: NodeId::from_u128(7),
        };
        let got = kv.get("ns", "claims/a").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(got, Some(want.clone()));
        let rows = kv.scan("ns", "claims/").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rows, vec![want]);
    }

    /// The set door parses exactly `{app_id, key, value: base64, origin}`
    /// (cw-rails' kv/tests post this literal).
    #[test]
    fn the_set_body_is_the_field_set_cw_rails_parses() {
        let body = KvSetBody {
            app_id: "ns".into(),
            key: "claims/a".into(),
            value: Bytes::from_static(b"hello"),
            origin: NodeId::from_u128(7),
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "app_id": "ns",
                "key": "claims/a",
                "value": "aGVsbG8=",
                "origin": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7],
            })
        );
    }
}
