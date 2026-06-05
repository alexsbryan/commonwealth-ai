//! Peer-to-peer model file distribution over the mesh's internal
//! port. The friend-onboarding workstream (WS5) wants new peers to
//! pull GGUFs from an existing mesh member instead of needing R2 /
//! S3 credentials of their own. The same primitive also lets a
//! fresh cloud pod skip the R2 sync step entirely — the laptop
//! already has the files, and the tailnet path is competitive with
//! R2 throughput once a direct WireGuard tunnel forms.
//!
//! Wire shape (both endpoints under `/internal/v1/models/*`):
//!
//!   GET  /internal/v1/models/list
//!     → 200  { "files": [{ "name", "size_bytes", "sha256" }, …] }
//!         lists only the files explicitly registered via
//!         `AppState::install_servable_model_files` — i.e. the
//!         GGUFs this daemon is configured to load. Not a
//!         directory browser; arbitrary files under the same
//!         folder are NOT exposed.
//!
//!   GET  /internal/v1/models/file/{name}
//!     → 200  application/octet-stream, Content-Length set,
//!            X-Sha256 header for end-to-end verification
//!     → 404  when {name} isn't on the allowlist OR the file no
//!            longer exists on disk
//!     → 503  when no files are registered (early boot / test
//!            fixtures that didn't call `install_…`)
//!
//! **Trust boundary**: the internal port is bound on the tailnet
//! interface only (per the daemon's bind config). Anyone on the
//! tailnet can fetch model files. This matches the existing
//! `/internal/index/serve` trust posture — once you've joined the
//! mesh, peer-to-peer file movement is unrestricted.
//!
//! **Integrity**: the SHA-256 is computed once on first listing and
//! cached in process memory keyed by (path, mtime, size). A model
//! file's content is immutable in practice, so the cache rarely
//! invalidates. Clients verify the `X-Sha256` response header
//! against the listing entry to detect transport corruption or a
//! mid-flight file replacement.

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio_util::io::ReaderStream;

use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFileInfo {
    pub name: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListResponse {
    pub files: Vec<ModelFileInfo>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

/// Per-process SHA-256 cache. Computing a 21 GB GGUF hash takes
/// 2-3 minutes on modern hardware; computing it on every list
/// request would make peer discovery uselessly slow. The cache
/// key includes mtime + size so a model swap-on-disk invalidates
/// the prior hash without operator intervention.
static SHA_CACHE: Mutex<Vec<CachedHash>> = Mutex::new(Vec::new());

#[derive(Debug, Clone)]
struct CachedHash {
    path: PathBuf,
    mtime_unix: i64,
    size_bytes: u64,
    sha256: String,
}

fn read_file_meta(path: &Path) -> std::io::Result<(u64, i64)> {
    let m = std::fs::metadata(path)?;
    let size = m.len();
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok((size, mtime))
}

fn cached_or_compute_sha(path: &Path, size: u64, mtime: i64) -> std::io::Result<String> {
    {
        let cache = SHA_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache
            .iter()
            .find(|e| e.path == path && e.mtime_unix == mtime && e.size_bytes == size)
        {
            return Ok(hit.sha256.clone());
        }
    }
    // Cold path: stream the file through SHA-256. 8 MiB buffer is
    // large enough to amortise syscalls without spending RAM on a
    // multi-GB working set.
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hex = format!("{:x}", hasher.finalize());
    let mut cache = SHA_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // Evict any stale entry for the same path before inserting the
    // fresh one (mtime/size differ → stale).
    cache.retain(|e| e.path != path);
    cache.push(CachedHash {
        path: path.to_path_buf(),
        mtime_unix: mtime,
        size_bytes: size,
        sha256: hex.clone(),
    });
    Ok(hex)
}

/// GET /internal/v1/models/list — enumerate this daemon's
/// servable GGUF files with verification metadata.
pub async fn list_model_files(State(state): State<AppState>) -> Json<ListResponse> {
    let allowlist = state.inner.servable_model_files.load();
    let mut files = Vec::with_capacity(allowlist.len());
    for path in allowlist.iter() {
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            tracing::warn!(
                path = %path.display(),
                "model_files: skipping non-UTF-8 file name on the allowlist"
            );
            continue;
        };
        let (size_bytes, mtime) = match read_file_meta(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "model_files: skipping missing/unreadable allowlist entry"
                );
                continue;
            }
        };
        // SHA-256 is expensive; spawn-blocking so the runtime can
        // serve concurrent requests during the hash. A 21 GB
        // GGUF takes ~2-3 min on first compute, but every
        // subsequent listing is microseconds via the cache.
        let path_clone = path.clone();
        let sha = match tokio::task::spawn_blocking(move || {
            cached_or_compute_sha(&path_clone, size_bytes, mtime)
        })
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "model_files: SHA-256 computation failed; skipping listing entry"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "model_files: hashing task panicked; skipping"
                );
                continue;
            }
        };
        files.push(ModelFileInfo {
            name: name.to_string(),
            size_bytes,
            sha256: sha,
        });
    }
    Json(ListResponse { files })
}

/// Parse a single HTTP byte-range header (`bytes=START-END`, open-ended
/// `bytes=START-`, or suffix `bytes=-N`) into an inclusive `(start, end)` clamped
/// to `[0, size)`. Returns `None` for a multi-range header, a non-`bytes` unit, or
/// an unsatisfiable range — the caller then serves the whole file (200). Pure +
/// unit-tested. The byte-range warm path (`#5b`) only ever sends explicit
/// `START-END` ranges, but a correct parser handles the other RFC-7233 forms too.
fn parse_single_byte_range(header: &str, size: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        // Multi-range needs multipart/byteranges (not worth it here); an empty
        // file has no satisfiable range.
        return None;
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let (start_s, end_s) = (start_s.trim(), end_s.trim());
    let (start, end) = match (start_s.is_empty(), end_s.is_empty()) {
        // `bytes=-N` — the final N bytes.
        (true, false) => {
            let n: u64 = end_s.parse().ok()?;
            if n == 0 {
                return None;
            }
            (size.saturating_sub(n), size - 1)
        }
        // `bytes=START-` — START to EOF.
        (false, true) => (start_s.parse().ok()?, size - 1),
        // `bytes=START-END` (END clamped to the last byte).
        (false, false) => {
            let s: u64 = start_s.parse().ok()?;
            let e: u64 = end_s.parse().ok()?;
            (s, e.min(size - 1))
        }
        (true, true) => return None,
    };
    if start > end || start >= size {
        return None; // unsatisfiable
    }
    Some((start, end))
}

/// GET /internal/v1/models/file/{name} — stream a model file, or a single byte
/// range when the client sends `Range: bytes=START-END`. Range support is what
/// lets a distributed worker fetch ONLY its shard's tensors (the `#5b` byte-range
/// warm path) instead of the whole GGUF, keeping it at O(model/N) on disk.
pub async fn serve_model_file(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    headers_in: HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let allowlist = state.inner.servable_model_files.load();
    if allowlist.is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no servable model files registered".into(),
            }),
        ));
    }

    // Match only on `file_name()` — never on a substring or path
    // segment supplied by the client. This prevents
    // `../etc/passwd`-style probes from ever reaching `open`.
    let Some(path) = allowlist
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(name.as_str()))
        .cloned()
    else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: format!("'{name}' is not on this daemon's servable model list"),
            }),
        ));
    };

    let (size_bytes, mtime) = read_file_meta(&path).map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: format!("file disappeared between listing and serve: {e}"),
            }),
        )
    })?;

    // Range request → 206 Partial Content. Skip the whole-file SHA entirely: it
    // reads the whole file (defeating the point of a range fetch) and is
    // irrelevant to a partial body — the byte-range warmer verifies each tensor's
    // own FNV hash, not a file digest.
    if let Some((start, end)) = headers_in
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| parse_single_byte_range(h, size_bytes))
    {
        let length = end - start + 1;
        let mut file = tokio::fs::File::open(&path).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody {
                    error: format!("open: {e}"),
                }),
            )
        })?;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        file.seek(std::io::SeekFrom::Start(start)).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody {
                    error: format!("seek: {e}"),
                }),
            )
        })?;
        tracing::debug!(
            path = %path.display(),
            name = %name,
            start,
            end,
            length,
            "model_files: serving byte range to peer"
        );
        let body = Body::from_stream(ReaderStream::new(file.take(length)));
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(axum::http::header::CONTENT_LENGTH, HeaderValue::from(length));
        headers.insert(
            axum::http::header::ACCEPT_RANGES,
            HeaderValue::from_static("bytes"),
        );
        headers.insert(
            axum::http::header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size_bytes}"))
                .unwrap_or(HeaderValue::from_static("")),
        );
        return Ok((StatusCode::PARTIAL_CONTENT, headers, body).into_response());
    }

    // Whole-file. Cheap: the cache should be warm from the prior list. We do
    // this so the X-Sha256 header reflects the *current* bytes —
    // a swap-on-disk between list and serve invalidates the cache
    // and we'd recompute. Slow path is fine; correctness > latency.
    let path_for_sha = path.clone();
    let sha = tokio::task::spawn_blocking(move || {
        cached_or_compute_sha(&path_for_sha, size_bytes, mtime)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("sha task panicked: {e}"),
            }),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("sha compute failed: {e}"),
            }),
        )
    })?;

    let file = tokio::fs::File::open(&path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("open: {e}"),
            }),
        )
    })?;
    tracing::info!(
        path = %path.display(),
        name = %name,
        size_bytes,
        "model_files: streaming to peer"
    );
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        HeaderValue::from(size_bytes),
    );
    // Advertise range support so a worker knows it can shard-fetch.
    headers.insert(
        axum::http::header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    headers.insert(
        "X-Sha256",
        HeaderValue::from_str(&sha).unwrap_or(HeaderValue::from_static("")),
    );

    Ok((StatusCode::OK, headers, body).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use commonwealth_core::ids::{MeshId, NodeId};
    use commonwealth_core::mesh::Mesh;
    use std::io::Write;
    use tower::util::ServiceExt;

    #[test]
    fn parse_single_byte_range_covers_rfc7233_forms() {
        let size = 1000;
        // Explicit START-END (inclusive) — the form the byte-range warmer sends.
        assert_eq!(parse_single_byte_range("bytes=0-99", size), Some((0, 99)));
        assert_eq!(parse_single_byte_range("bytes=100-199", size), Some((100, 199)));
        // END past EOF clamps to the last byte.
        assert_eq!(parse_single_byte_range("bytes=900-5000", size), Some((900, 999)));
        // Open-ended START- → to EOF.
        assert_eq!(parse_single_byte_range("bytes=500-", size), Some((500, 999)));
        // Suffix -N → final N bytes.
        assert_eq!(parse_single_byte_range("bytes=-200", size), Some((800, 999)));
        // Whitespace tolerated.
        assert_eq!(parse_single_byte_range(" bytes=0-9 ", size), Some((0, 9)));
        // Unsatisfiable / unsupported → None (caller serves the whole file).
        assert_eq!(parse_single_byte_range("bytes=1000-1001", size), None); // start >= size
        assert_eq!(parse_single_byte_range("bytes=50-10", size), None); // start > end
        assert_eq!(parse_single_byte_range("bytes=0-10,20-30", size), None); // multi-range
        assert_eq!(parse_single_byte_range("items=0-10", size), None); // wrong unit
        assert_eq!(parse_single_byte_range("bytes=abc", size), None); // garbage
        assert_eq!(parse_single_byte_range("bytes=0-0", 0), None); // empty file
    }

    fn fixture_state(files: Vec<PathBuf>) -> AppState {
        let mesh = Mesh {
            id: MeshId::generate(),
            name: "test".into(),
            join_key_hash: [0u8; 32],
            members: Default::default(),
            peers: vec![],
        };
        let state = AppState::new(NodeId::generate(), mesh);
        state.install_servable_model_files(files);
        state
    }

    fn router(state: AppState) -> Router {
        Router::new()
            .route("/internal/v1/models/list", get(list_model_files))
            .route("/internal/v1/models/file/{name}", get(serve_model_file))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_returns_registered_files_with_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Darwin-9B-Opus.Q4_K_M.gguf");
        // Real (small) content so the SHA is something we can
        // pin in the assertion if needed; here we just check
        // the shape, the value, and the well-known-empty-string
        // hash for a 4-byte file is stable.
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"GGUF").unwrap();
        f.flush().unwrap();
        drop(f);

        let state = fixture_state(vec![path.clone()]);
        let app = router(state);
        let resp = app
            .oneshot(
                Request::get("/internal/v1/models/list")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let parsed: ListResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].name, "Darwin-9B-Opus.Q4_K_M.gguf");
        assert_eq!(parsed.files[0].size_bytes, 4);
        // SHA-256("GGUF") is deterministic — pin it. Catches an
        // accidental "first-N-bytes" or "metadata-only" regression.
        // Reproduce: `printf 'GGUF' | sha256sum`.
        assert_eq!(
            parsed.files[0].sha256,
            "b83633aa785344791618f2fddf131b010ea04912a60430760b070bad293f65bd"
        );
    }

    #[tokio::test]
    async fn serve_returns_streamed_bytes_with_sha_header() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("model.gguf");
        let payload: Vec<u8> = (0u8..=255u8).cycle().take(1024 * 32).collect();
        std::fs::write(&path, &payload).unwrap();

        let state = fixture_state(vec![path.clone()]);
        let app = router(state);
        let resp = app
            .oneshot(
                Request::get("/internal/v1/models/file/model.gguf")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sha_header = resp
            .headers()
            .get("X-Sha256")
            .map(|v| v.to_str().unwrap().to_string());
        assert!(sha_header.is_some(), "X-Sha256 must be set");
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.len(), payload.len());
        assert_eq!(body.as_ref(), payload.as_slice());

        // The header SHA must match what we'd compute locally over
        // the response bytes — that's the end-to-end integrity
        // guarantee the client relies on.
        let mut hasher = Sha256::new();
        hasher.update(&body);
        let expected = format!("{:x}", hasher.finalize());
        assert_eq!(sha_header.unwrap(), expected);
    }

    #[tokio::test]
    async fn serve_404s_for_unlisted_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed.gguf");
        std::fs::write(&allowed, b"hi").unwrap();
        // Create a second file in the same dir that is NOT on
        // the allowlist. The endpoint must refuse it even though
        // it exists.
        let sneak = tmp.path().join("not-allowed.gguf");
        std::fs::write(&sneak, b"secret").unwrap();

        let state = fixture_state(vec![allowed]);
        let app = router(state);
        let resp = app
            .oneshot(
                Request::get("/internal/v1/models/file/not-allowed.gguf")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn serve_503_when_allowlist_empty() {
        let state = fixture_state(vec![]);
        let app = router(state);
        let resp = app
            .oneshot(
                Request::get("/internal/v1/models/file/anything.gguf")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn sha_cache_hits_on_unchanged_file() {
        // Hash the same file twice; the second call must come from
        // the cache. We observe this indirectly via timing — first
        // call has to read the disk, the second returns instantly.
        // 4 MiB so first compute takes long enough to measure.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hash-cache.gguf");
        let buf = vec![7u8; 4 * 1024 * 1024];
        std::fs::write(&path, &buf).unwrap();
        let (size, mtime) = read_file_meta(&path).unwrap();

        let a = cached_or_compute_sha(&path, size, mtime).unwrap();
        let b = cached_or_compute_sha(&path, size, mtime).unwrap();
        assert_eq!(a, b);
        // Modify the file → cache entry must invalidate and the
        // new content must produce a different hash.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, b"different").unwrap();
        let (size2, mtime2) = read_file_meta(&path).unwrap();
        let c = cached_or_compute_sha(&path, size2, mtime2).unwrap();
        assert_ne!(a, c, "cache must invalidate when mtime/size changes");
    }
}
