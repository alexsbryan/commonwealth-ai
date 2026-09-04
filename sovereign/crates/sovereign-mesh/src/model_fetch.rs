// SPDX-License-Identifier: AGPL-3.0-or-later
//! Client side of peer-to-peer model file distribution.
//!
//! Talks to `commonwealth-api::routes_internal::model_files` on a
//! peer's `:9742` (internal port). Used by:
//!
//!   - `sovereign mesh fetch-model <name>` — explicit operator
//!     command to pull a single file
//!   - (Stage 2, follow-up) the daemon's startup path when a
//!     configured GGUF is missing from disk and at least one mesh
//!     peer advertises it on `/internal/v1/models/list`
//!
//! The wire types come from `commonwealth_core::model`. They were declared
//! here as well until 2026-09-04, under a header claiming the commonwealth-api
//! dependency was only transitive and that the tests below locked the wire
//! format. Both claims were false: the dependency is direct and declared in
//! this crate's Cargo.toml, and `spawn_test_server` serialises THIS file's own
//! types, so it proved the client agreed with itself. What the fork actually
//! cost is visible at `rpc_warm_http.rs` — the server grew HTTP Range support
//! in 2026-06 and this client still cannot ask for one.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub use commonwealth_core::model::{ModelFileInfo, ModelFileListing};

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("peer doesn't advertise '{0}'")]
    NotAdvertised(String),
    #[error("integrity check failed for '{name}': expected sha256 {expected}, got {got}")]
    IntegrityMismatch {
        name: String,
        expected: String,
        got: String,
    },
    #[error("peer returned non-success status: {0}")]
    HttpStatus(reqwest::StatusCode),
}

/// `GET /internal/v1/models/list` against a peer's internal port.
/// The `peer_base` must be the form `http://<host>:9742` — the
/// `/internal/v1/...` suffix is appended here.
pub async fn list_peer_files(
    http: &reqwest::Client,
    peer_base: &str,
) -> Result<ModelFileListing, FetchError> {
    let url = commonwealth_core::model::models_list_url(peer_base);
    let resp = http.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(FetchError::HttpStatus(resp.status()));
    }
    Ok(resp.json().await?)
}

/// Stream a model file from a peer to `dest_dir/<name>`. Verifies
/// the SHA-256 against the listing entry on the fly — there's no
/// second pass over the file, the bytes go straight to disk and
/// the hasher sees the same bytes the writer sees.
///
/// Uses an atomic rename via `<dest>.<pid>.partial` so a crash
/// mid-download never leaves a half-written GGUF where llama.cpp
/// might try to load it.
///
/// `progress` is invoked roughly every `~16 MiB` with
/// `(bytes_downloaded, total_bytes)` — UI hook for a progress bar
/// or log line. Pass `|_, _| {}` to ignore.
pub async fn fetch_model_to_dir(
    http: &reqwest::Client,
    peer_base: &str,
    info: &ModelFileInfo,
    dest_dir: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, FetchError> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    std::fs::create_dir_all(dest_dir)?;
    let final_path = dest_dir.join(&info.name);
    let partial_path = dest_dir.join(format!(".{}.{}.partial", info.name, std::process::id()));

    let url = commonwealth_core::model::model_file_url(
        peer_base,
        &urlencoding::encode(&info.name),
    );
    let resp = http.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(FetchError::HttpStatus(resp.status()));
    }

    let mut out = tokio::fs::File::create(&partial_path).await?;
    let mut hasher = Sha256::new();
    let mut bytes_total: u64 = 0;
    let mut last_progress_emit: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        out.write_all(&chunk).await?;
        bytes_total += chunk.len() as u64;
        if bytes_total - last_progress_emit > 16 * 1024 * 1024 {
            progress(bytes_total, info.size_bytes);
            last_progress_emit = bytes_total;
        }
    }
    out.flush().await?;
    drop(out);

    let got = format!("{:x}", hasher.finalize());
    if got != info.sha256 {
        // Drop the bad partial so a retry starts clean. We don't
        // need to be tidy on every error path — if removal fails
        // here the partial gets reclaimed by the next fetch (same
        // name template) or by an operator cleanup. But best effort.
        let _ = std::fs::remove_file(&partial_path);
        return Err(FetchError::IntegrityMismatch {
            name: info.name.clone(),
            expected: info.sha256.clone(),
            got,
        });
    }
    // Atomic on POSIX — llama.cpp can never observe a torn write.
    std::fs::rename(&partial_path, &final_path)?;
    progress(bytes_total, info.size_bytes);
    Ok(final_path)
}

/// Convenience: list a peer's files, find one by name, fetch it.
/// Returns `NotAdvertised` if the peer doesn't advertise the name.
pub async fn fetch_named_model_from_peer(
    http: &reqwest::Client,
    peer_base: &str,
    name: &str,
    dest_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, FetchError> {
    let listing = list_peer_files(http, peer_base).await?;
    let info = listing
        .files
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(|| FetchError::NotAdvertised(name.to_string()))?;
    fetch_model_to_dir(http, peer_base, &info, dest_dir, progress).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    /// Bring up an in-process axum server matching the production
    /// `/internal/v1/models/*` wire shape, serving from a tempdir.
    /// Returns `(base_url, _server_handle)`. Drop the handle and
    /// the server stops at the end of the test.
    async fn spawn_test_server(dir: &Path) -> (String, tokio::task::JoinHandle<()>) {
        // Compute the listing ourselves to mirror what the real
        // handler would do — keeps the test independent of the
        // commonwealth-api crate (which would be a circular dep
        // for this sovereign-mesh test).
        let dir_owned = dir.to_path_buf();
        let dir_for_list = dir_owned.clone();
        let list_handler = move || {
            let dir = dir_for_list.clone();
            async move {
                let mut files = Vec::new();
                for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    let name = path.file_name().unwrap().to_string_lossy().to_string();
                    let size = std::fs::metadata(&path).unwrap().len();
                    let mut hasher = Sha256::new();
                    hasher.update(std::fs::read(&path).unwrap());
                    let sha = format!("{:x}", hasher.finalize());
                    files.push(ModelFileInfo {
                        name,
                        size_bytes: size,
                        sha256: sha,
                    });
                }
                axum::Json(ModelFileListing { files })
            }
        };
        let dir_for_serve = dir_owned.clone();
        let serve_handler = move |axum::extract::Path(name): axum::extract::Path<String>| {
            let dir = dir_for_serve.clone();
            async move {
                let path = dir.join(&name);
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => {
                        return (axum::http::StatusCode::NOT_FOUND, Vec::new()).into_response();
                    }
                };
                use axum::response::IntoResponse;
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let sha = format!("{:x}", hasher.finalize());
                let mut resp = (axum::http::StatusCode::OK, bytes).into_response();
                resp.headers_mut()
                    .insert("X-Sha256", axum::http::HeaderValue::from_str(&sha).unwrap());
                resp
            }
        };
        use axum::response::IntoResponse;

        let app = Router::new()
            .route("/internal/v1/models/list", get(list_handler))
            .route("/internal/v1/models/file/{name}", get(serve_handler));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, handle)
    }

    #[tokio::test]
    async fn list_peer_files_returns_dir_contents() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.gguf"), b"AAAA").unwrap();
        std::fs::write(tmp.path().join("b.gguf"), b"BBBBB").unwrap();
        let (base, _h) = spawn_test_server(tmp.path()).await;

        let client = reqwest::Client::new();
        let listing = list_peer_files(&client, &base).await.unwrap();
        let names: Vec<_> = listing.files.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"a.gguf"));
        assert!(names.contains(&"b.gguf"));
        // sha matches a known string
        let a_info = listing.files.iter().find(|f| f.name == "a.gguf").unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"AAAA");
        assert_eq!(a_info.sha256, format!("{:x}", hasher.finalize()));
    }

    #[tokio::test]
    async fn fetch_to_dir_writes_atomic_and_verifies_sha() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0u8..=255u8).cycle().take(64 * 1024).collect();
        std::fs::write(src.path().join("model.gguf"), &payload).unwrap();
        let (base, _h) = spawn_test_server(src.path()).await;

        let client = reqwest::Client::new();
        let listing = list_peer_files(&client, &base).await.unwrap();
        let info = listing.files.into_iter().next().unwrap();
        let got_path = fetch_model_to_dir(&client, &base, &info, dest.path(), |_, _| {})
            .await
            .unwrap();

        assert_eq!(got_path, dest.path().join("model.gguf"));
        assert_eq!(std::fs::read(&got_path).unwrap(), payload);
        // No partial files left behind.
        let leftover: Vec<_> = std::fs::read_dir(dest.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".partial"))
            .collect();
        assert!(
            leftover.is_empty(),
            "partial file should have been atomically renamed"
        );
    }

    #[tokio::test]
    async fn fetch_rejects_tampered_response() {
        // Synthesise an info struct with a bogus SHA. The client
        // must refuse with IntegrityMismatch and not leave the
        // bad bytes at the final path.
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("model.gguf"), b"real bytes").unwrap();
        let (base, _h) = spawn_test_server(src.path()).await;

        let bogus = ModelFileInfo {
            name: "model.gguf".into(),
            size_bytes: 10,
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        };
        let client = reqwest::Client::new();
        let err = fetch_model_to_dir(&client, &base, &bogus, dest.path(), |_, _| {})
            .await
            .unwrap_err();
        match err {
            FetchError::IntegrityMismatch { name, .. } => assert_eq!(name, "model.gguf"),
            other => panic!("expected IntegrityMismatch, got {:?}", other),
        }
        assert!(
            !dest.path().join("model.gguf").exists(),
            "tampered file must not land at the final path"
        );
    }

    #[tokio::test]
    async fn fetch_named_404s_when_not_advertised() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("present.gguf"), b"x").unwrap();
        let (base, _h) = spawn_test_server(src.path()).await;

        let client = reqwest::Client::new();
        let err =
            fetch_named_model_from_peer(&client, &base, "missing.gguf", dest.path(), |_, _| {})
                .await
                .unwrap_err();
        assert!(matches!(err, FetchError::NotAdvertised(ref n) if n == "missing.gguf"));
    }
}
