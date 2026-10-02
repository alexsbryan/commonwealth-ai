// SPDX-License-Identifier: AGPL-3.0-or-later
//! A legacy store migrates once through the dial and `svrn portfolio` reads
//! the same rows back; a failed read is a refusal that touches nothing.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::Json;
use bytes::Bytes;
use kernel_types::NodeId;
use sovereign_contracts::peer::{KvLookup, KvScanQuery, KvSetBody, ReplicatedKvEntry};
use sovereign_daemon::rails_client::kv::RailsKv;

use super::migrate_if_needed;
use crate::portfolio_cmd::{get_portfolio, PORTFOLIO_PRIVATE_APP_ID};

type Rows = Arc<Mutex<BTreeMap<(String, String), ReplicatedKvEntry>>>;

/// A stand-in for cw-rails' `/v1/mesh/kv/*` doors over a map that outlives
/// every client, served from its own thread so the sync client may block.
fn door() -> (String, Rows) {
    let rows: Rows = Arc::default();
    let app = axum::Router::new()
        .route(
            "/v1/mesh/kv/entry",
            get(
                |State(r): State<Rows>, Query(q): Query<KvLookup>| async move {
                    Json(r.lock().unwrap().get(&(q.app_id, q.key)).cloned())
                },
            )
            .post(
                |State(r): State<Rows>, Json(b): Json<KvSetBody>| async move {
                    let entry = ReplicatedKvEntry {
                        app_id: b.app_id.clone(),
                        key: b.key.clone(),
                        value: b.value,
                        timestamp: 1,
                        origin: b.origin,
                    };
                    Json(r.lock().unwrap().insert((b.app_id, b.key), entry).is_none())
                },
            ),
        )
        .route(
            "/v1/mesh/kv/entries",
            get(
                |State(r): State<Rows>, Query(q): Query<KvScanQuery>| async move {
                    Json(
                        r.lock()
                            .unwrap()
                            .values()
                            .filter(|e| e.app_id == q.app_id && e.key.starts_with(&q.prefix))
                            .cloned()
                            .collect::<Vec<_>>(),
                    )
                },
            ),
        )
        .with_state(Arc::clone(&rows));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, app).await.unwrap();
            })
    });
    (format!("http://{addr}"), rows)
}

fn legacy_row(app_id: &str, key: &str, value: &str) -> ReplicatedKvEntry {
    ReplicatedKvEntry {
        app_id: app_id.into(),
        key: key.into(),
        value: Bytes::from(value.to_string()),
        timestamp: 7,
        origin: NodeId::from_u128(9),
    }
}

fn legacy_file(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("portfolio.db");
    std::fs::write(&path, b"legacy bytes").unwrap();
    path
}

#[test]
fn a_legacy_file_migrates_once_and_the_portfolio_reads_the_same_rows() {
    let (base, _rows) = door();
    let dir = tempfile::tempdir().unwrap();
    let path = legacy_file(dir.path());
    let ids = [(PORTFOLIO_PRIVATE_APP_ID, PORTFOLIO_PRIVATE_APP_ID)];

    let kv = RailsKv::new(&base);
    let n = migrate_if_needed(&path, &ids, &kv, |p, app_ids| {
        assert_eq!(p, path.as_path());
        assert_eq!(app_ids, [PORTFOLIO_PRIVATE_APP_ID.to_string()]);
        Ok(vec![legacy_row(
            PORTFOLIO_PRIVATE_APP_ID,
            "tech",
            r#"["a","b"]"#,
        )])
    })
    .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        get_portfolio(&kv, "tech").unwrap(),
        Some(vec!["a".to_string(), "b".to_string()])
    );

    // A restart: a fresh client reads the migrated store, and the second
    // run migrates nothing without reading the file.
    let restarted = RailsKv::new(&base);
    let again = migrate_if_needed(&path, &ids, &restarted, |_, _| {
        panic!("a migrated file is never read again")
    })
    .unwrap();
    assert_eq!(again, 0);
    assert_eq!(
        get_portfolio(&restarted, "tech").unwrap(),
        Some(vec!["a".to_string(), "b".to_string()])
    );
}

#[test]
fn a_colon_app_id_is_written_under_its_rename() {
    let (base, rows) = door();
    let dir = tempfile::tempdir().unwrap();
    let path = legacy_file(dir.path());
    let kv = RailsKv::new(&base);
    migrate_if_needed(
        &path,
        &[("wikipedia-newsworthy:status", "wikipedia-newsworthy-status")],
        &kv,
        |_, _| {
            Ok(vec![legacy_row(
                "wikipedia-newsworthy:status",
                "last_tick",
                "{}",
            )])
        },
    )
    .unwrap();
    let rows = rows.lock().unwrap();
    assert!(rows.contains_key(&(
        "wikipedia-newsworthy-status".to_string(),
        "last_tick".to_string()
    )));
    assert_eq!(rows.len(), 1);
}

#[test]
fn a_failed_read_is_a_named_refusal_that_leaves_the_file_untouched() {
    let (base, rows) = door();
    let dir = tempfile::tempdir().unwrap();
    let path = legacy_file(dir.path());
    let kv = RailsKv::new(&base);
    let err = migrate_if_needed(
        &path,
        &[(PORTFOLIO_PRIVATE_APP_ID, PORTFOLIO_PRIVATE_APP_ID)],
        &kv,
        |_, _| Err("the file is not a store".to_string()),
    )
    .unwrap_err();
    assert!(
        err.contains("refused") && err.contains("untouched"),
        "{err}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"legacy bytes");
    assert!(!super::marker(&path).exists());
    assert!(rows.lock().unwrap().is_empty());
}

#[test]
fn an_unreachable_store_refuses_and_writes_no_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = legacy_file(dir.path());
    let kv = RailsKv::new("http://127.0.0.1:1");
    let err = migrate_if_needed(
        &path,
        &[(PORTFOLIO_PRIVATE_APP_ID, PORTFOLIO_PRIVATE_APP_ID)],
        &kv,
        |_, _| Ok(vec![legacy_row(PORTFOLIO_PRIVATE_APP_ID, "tech", "[]")]),
    )
    .unwrap_err();
    assert!(err.contains("refused"), "{err}");
    assert!(!super::marker(&path).exists());
    assert!(get_portfolio(&kv, "tech").is_err());
}

#[test]
fn the_portfolio_namespace_is_local_only() {
    assert!(commonwealth_rail_core::is_local_only(
        PORTFOLIO_PRIVATE_APP_ID
    ));
}
