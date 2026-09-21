// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the warm-RPC HTTP surface — see `rpc_warm_http.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::*;
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// Serve a fixed byte buffer with `Range` support, mirroring the production
/// `serve_model_file` 206 path — enough to prove the warmer's round-trip
/// without dragging in commonwealth-api (a circular dep for this crate).
async fn spawn_range_server(data: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let data = Arc::new(data);
    let handler = move |headers: axum::http::HeaderMap| {
        let data = Arc::clone(&data);
        async move {
            let size = data.len() as u64;
            if let Some((s, e)) = headers
                .get(axum::http::header::RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|h| {
                    let spec = h.trim().strip_prefix("bytes=")?;
                    let (a, b) = spec.split_once('-')?;
                    Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
                })
            {
                let slice = data[s as usize..=(e as usize)].to_vec();
                return (
                    axum::http::StatusCode::PARTIAL_CONTENT,
                    [(
                        axum::http::header::CONTENT_RANGE,
                        format!("bytes {s}-{e}/{size}"),
                    )],
                    slice,
                )
                    .into_response();
            }
            (axum::http::StatusCode::OK, (*data).clone()).into_response()
        }
    };
    use axum::response::IntoResponse;
    let app = Router::new().route("/internal/v1/models/file/m.gguf", get(handler));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        format!("http://{addr}/internal/v1/models/file/m.gguf"),
        server,
    )
}

/// `tg-2-strangers-are-refused`, the range warm's half: every range GET
/// carries the pair it was handed, and none carries anything when it was
/// handed none.
///
/// The source is a PEER's `/internal/v1/models/file/*`, so the absent case
/// is a 401 on the default `internal_auth` — and an unstamped range warm is
/// what this function built before the pair existed.
#[tokio::test]
async fn every_range_get_carries_the_pair_it_was_given() {
    let seen: Arc<std::sync::Mutex<Vec<Option<String>>>> = Default::default();
    let sink = Arc::clone(&seen);
    let data: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
    let body = Arc::new(data.clone());
    let handler = move |headers: axum::http::HeaderMap| {
        let sink = Arc::clone(&sink);
        let body = Arc::clone(&body);
        async move {
            sink.lock().unwrap().push(
                headers
                    .get("x-mesh-proof")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            );
            let (s, e) = headers
                .get(axum::http::header::RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|h| {
                    let spec = h.trim().strip_prefix("bytes=")?;
                    let (a, b) = spec.split_once('-')?;
                    Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                })
                .expect("the warm always sends a range");
            (
                axum::http::StatusCode::PARTIAL_CONTENT,
                body[s..=e].to_vec(),
            )
        }
    };
    let app = Router::new().route("/internal/v1/models/file/m.gguf", get(handler));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let url = format!("http://{addr}/internal/v1/models/file/m.gguf");

    let mk = |off: usize, len: usize| {
        let mut h = Fnv1a::new();
        h.update(&data[off..off + len]);
        TensorRange {
            gguf_offset: off as u64,
            nbytes: len as u64,
            hash: h.finish(),
            file_idx: 0,
        }
    };
    let tensors = vec![mk(0, 64), mk(128, 32)];
    let http = reqwest::Client::new();

    let stamped = tempfile::tempdir().unwrap();
    warm_cache_from_ranges(
        &http,
        &[url.clone()],
        &tensors,
        stamped.path(),
        &[],
        Some(("x-mesh-proof", "feed.face")),
    )
    .await
    .expect("stamped warm");

    let bare = tempfile::tempdir().unwrap();
    warm_cache_from_ranges(&http, &[url], &tensors, bare.path(), &[], None)
        .await
        .expect("unstamped warm");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            Some("feed.face".to_string()),
            Some("feed.face".to_string()),
            None,
            None
        ],
        "one entry per tensor per run: both stamped, then both bare"
    );
}

#[tokio::test]
async fn warm_from_ranges_writes_hash_named_cache_files() {
    // A fake "GGUF": two distinct tensor regions in one buffer.
    let mut data = vec![0u8; 4096];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let (url, server) = spawn_range_server(data.clone()).await;

    // Two tensors at known offsets; compute their true FNV hashes.
    let mk = |off: usize, len: usize| {
        let mut h = Fnv1a::new();
        h.update(&data[off..off + len]);
        TensorRange {
            gguf_offset: off as u64,
            nbytes: len as u64,
            hash: h.finish(),
            file_idx: 0,
        }
    };
    let tensors = vec![mk(0, 1000), mk(1000, 2000)];

    let cache = tempfile::tempdir().unwrap();
    let http = reqwest::Client::new();
    let stats = warm_cache_from_ranges(&http, &[url.clone()], &tensors, cache.path(), &[], None)
        .await
        .expect("warm");
    assert_eq!(stats.written, 2);
    assert_eq!(stats.bytes_written, 3000);

    // Each cache file is named by its hash and holds the exact bytes.
    for t in &tensors {
        let f = cache.path().join(cache_file_name(t.hash));
        let got = std::fs::read(&f).unwrap();
        assert_eq!(got.len() as u64, t.nbytes);
        assert_eq!(
            &got[..],
            &data[t.gguf_offset as usize..(t.gguf_offset + t.nbytes) as usize]
        );
    }

    // Idempotent: a second run writes nothing new.
    let again = warm_cache_from_ranges(&http, &[url], &tensors, cache.path(), &[], None)
        .await
        .unwrap();
    assert_eq!(again.written, 0);
    assert_eq!(again.already_present, 2);

    server.abort();
}

#[tokio::test]
async fn warm_from_ranges_rejects_a_hash_mismatch() {
    let data = vec![7u8; 2048];
    let (url, server) = spawn_range_server(data).await;
    // A tensor claiming a hash the bytes don't produce → must refuse (else the
    // host's SET_TENSOR_HASH would miss and stream → deadlock).
    let bad = vec![TensorRange {
        gguf_offset: 0,
        nbytes: 1000,
        hash: 0xdead_beef_dead_beef,
        file_idx: 0,
    }];
    let cache = tempfile::tempdir().unwrap();
    let http = reqwest::Client::new();
    let err = warm_cache_from_ranges(&http, &[url], &bad, cache.path(), &[], None)
        .await
        .unwrap_err();
    assert!(err.contains("hash mismatch"), "got: {err}");
    // No cache file left behind.
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    server.abort();
}

#[test]
fn request_round_trips_through_json() {
    let req = RpcWarmShardRequest {
        model_id: "m.gguf".into(),
        device_index: 1,
        plan: Vec::new(),
        source: RpcWarmSource::ByteRanges {
            source_urls: vec!["http://h/internal/v1/models/file/m.gguf".into()],
            tensors: vec![TensorRange {
                gguf_offset: 10,
                nbytes: 20,
                hash: 30,
                file_idx: 0,
            }],
            file_urls: vec![],
        },
        host_node_id: Some("0123456789abcdef0123456789abcdef".into()),
    };
    let v = serde_json::to_value(&req).unwrap();
    // The opaque-JSON seam in commonwealth-api reads `model_id` for path
    // resolution — it must be a top-level string.
    assert_eq!(v.get("model_id").and_then(|x| x.as_str()), Some("m.gguf"));
    let back: RpcWarmShardRequest = serde_json::from_value(v).unwrap();
    assert_eq!(back.device_index, 1);
    assert_eq!(
        back.host_node_id.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    matches!(back.source, RpcWarmSource::ByteRanges { .. });
}

#[test]
fn old_wire_body_without_host_node_id_still_parses() {
    // A pre-identity host omits the field entirely — a rolling-upgrade
    // worker must not reject the body.
    let v = serde_json::json!({
        "model_id": "m.gguf",
        "device_index": 0,
        "plan": [],
        "source": { "mode": "whole_gguf", "peer_bases": ["http://10.0.0.1:9742"] }
    });
    let req: RpcWarmShardRequest = serde_json::from_value(v).unwrap();
    assert_eq!(req.host_node_id, None);
}

#[test]
fn split_sibling_names_generates_the_full_set() {
    assert_eq!(
        split_sibling_names("m-00001-of-00003.gguf"),
        vec![
            "m-00001-of-00003.gguf",
            "m-00002-of-00003.gguf",
            "m-00003-of-00003.gguf"
        ]
    );
    // Non-split and degenerate names pass through untouched.
    assert_eq!(split_sibling_names("model.gguf"), vec!["model.gguf"]);
    assert_eq!(
        split_sibling_names("m-00001-of-00001.gguf"),
        vec!["m-00001-of-00001.gguf"]
    );
}

#[tokio::test]
async fn warm_from_ranges_routes_tensors_to_their_own_file() {
    // Two "shard files" with distinct contents on separate servers; a
    // tensor from each. Per-file routing must fetch each range from ITS
    // file — the split-GGUF fix (fetching both from file 0 would
    // hash-mismatch tensor 1).
    let data0 = vec![11u8; 2048];
    let mut data1 = vec![0u8; 2048];
    for (i, b) in data1.iter_mut().enumerate() {
        *b = (i % 97) as u8;
    }
    let (url0, s0) = spawn_range_server(data0.clone()).await;
    let (url1, s1) = spawn_range_server(data1.clone()).await;

    let mk = |data: &[u8], off: usize, len: usize, file_idx: u32| {
        let mut h = Fnv1a::new();
        h.update(&data[off..off + len]);
        TensorRange {
            gguf_offset: off as u64,
            nbytes: len as u64,
            hash: h.finish(),
            file_idx,
        }
    };
    let tensors = vec![mk(&data0, 0, 1000, 0), mk(&data1, 500, 1200, 1)];
    let file_urls = vec![vec![url0.clone()], vec![url1.clone()]];

    let cache = tempfile::tempdir().unwrap();
    let http = reqwest::Client::new();
    let stats = warm_cache_from_ranges(&http, &[], &tensors, cache.path(), &file_urls, None)
        .await
        .expect("split warm");
    assert_eq!(stats.written, 2);
    // Each cache entry holds the bytes of ITS OWN file's range.
    for (t, data) in [(&tensors[0], &data0), (&tensors[1], &data1)] {
        let got = std::fs::read(cache.path().join(cache_file_name(t.hash))).unwrap();
        assert_eq!(
            &got[..],
            &data[t.gguf_offset as usize..(t.gguf_offset + t.nbytes) as usize]
        );
    }
    s0.abort();
    s1.abort();
}

#[test]
fn raw_fallback_refused_for_loopback_worker() {
    // A loopback worker_ip means a bridge-local endpoint (task 6): the
    // hand-built raw URL would target OURSELF, and a self-warm reports
    // success while the real worker stays cold → upload deadlock.
    assert!(!raw_warm_fallback_allowed("127.0.0.1"));
    assert!(!raw_warm_fallback_allowed("::1"));
    // Real remote IPs and unparseable hostnames keep the legacy fallback.
    assert!(raw_warm_fallback_allowed("192.168.1.2"));
    assert!(raw_warm_fallback_allowed("100.104.36.28"));
    assert!(raw_warm_fallback_allowed("beefymac.local"));
}

#[test]
fn merge_bases_prefers_transport_and_dedups() {
    let raw = vec![
        "http://192.168.1.19:9742".to_string(),
        "http://127.0.0.1:60001".to_string(),
    ];
    // The transport bridge duplicates one raw entry — it must lead the
    // merged order, appearing exactly once, with the rest behind it.
    let merged = merge_bases(["http://127.0.0.1:60001".to_string()], &raw);
    assert_eq!(
        merged,
        vec![
            "http://127.0.0.1:60001".to_string(),
            "http://192.168.1.19:9742".to_string(),
        ]
    );
}
