// SPDX-License-Identifier: AGPL-3.0-or-later
//! Document-follow logic for the HTTP API acquirer.
//!
//! When a recipe sets `[acquire.follow]`, the per-page response is
//! treated as an *index* — the JSON contains a list of URLs that
//! point at the actual documents. This module pulls those URLs out
//! via JSONPath and persists each fetched document under
//! `<acquired-dir>/docs/<sha-of-url>.<ext>` so the extractor can
//! walk the directory.
//!
//! When no `[acquire.follow]` block is set, the acquirer skips this
//! step and writes the page response itself; that mode is wired in
//! the orchestrator (`mod.rs`).

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::recipe::{DocFormat, FollowConfig};

/// Extract document URLs from a page response body.
pub fn extract_document_urls(body: &Value, jsonpath_expr: &str) -> Result<Vec<String>> {
    use jsonpath_rust::JsonPath;

    let path = JsonPath::try_from(jsonpath_expr).map_err(|e| {
        Error::Recipe(format!(
            "invalid follow.document_url_path JSONPath `{jsonpath_expr}`: {e}"
        ))
    })?;
    let mut urls = Vec::new();
    if let Value::Array(arr) = path.find(body) {
        for v in arr {
            if let Some(s) = v.as_str() {
                urls.push(s.to_string());
            }
        }
    }
    Ok(urls)
}

/// Compute the on-disk path for a document URL under `docs_dir`.
/// Filename is `<sha256-of-url>.<ext>`; the extension comes from the
/// recipe-declared format.
pub fn document_path(docs_dir: &Path, url: &str, format: DocFormat) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let hex = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let ext = match format {
        DocFormat::Html => "html",
        DocFormat::Json => "json",
        DocFormat::Xml => "xml",
        DocFormat::Plaintext => "txt",
    };
    docs_dir.join(format!("{hex}.{ext}"))
}

/// Fetch a single document URL, write it to disk, and return the
/// final on-disk path. If the file already exists (resume), short-
/// circuits without re-fetching.
///
/// `headers` is the per-binding map of templated headers (e.g.
/// `Authorization: Token <api_token>`) that the orchestrator
/// rendered against the active `for_each` binding. Apply it on
/// every follow fetch — same auth surface as the page request.
pub async fn fetch_document(
    client: &reqwest::Client,
    url: &str,
    docs_dir: &Path,
    follow: &FollowConfig,
    headers: &reqwest::header::HeaderMap,
) -> Result<PathBuf> {
    let path = document_path(docs_dir, url, follow.document_format);
    if path.exists() {
        return Ok(path);
    }
    let request = client.get(url);
    let request = if headers.is_empty() {
        request
    } else {
        request.headers(headers.clone())
    };
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(Error::Recipe(format!(
            "follow fetch of `{url}` failed with HTTP {}",
            response.status()
        )));
    }
    let bytes = response.bytes().await?;
    // Write atomically: write to a `.part` neighbour then rename, so
    // a crashed mid-write doesn't leave a half-document on disk that
    // resume would skip.
    let part = path.with_extension(format!(
        "{}.part",
        path.extension().and_then(|s| s.to_str()).unwrap_or("dat")
    ));
    std::fs::write(&part, &bytes)?;
    std::fs::rename(&part, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_urls_resolves_jsonpath() {
        let body = json!({
            "hits": {"hits": [
                {"_source": {"file_url": "https://a.example/1.html"}},
                {"_source": {"file_url": "https://a.example/2.html"}},
            ]}
        });
        let urls = extract_document_urls(&body, "$.hits.hits[*]._source.file_url").unwrap();
        assert_eq!(
            urls,
            vec![
                "https://a.example/1.html".to_string(),
                "https://a.example/2.html".to_string(),
            ]
        );
    }

    #[test]
    fn extract_urls_invalid_path_errors() {
        let body = json!({});
        let err = extract_document_urls(&body, "(((not jsonpath").unwrap_err();
        assert!(format!("{err}").contains("JSONPath"));
    }

    #[test]
    fn document_path_is_stable_per_url() {
        let dir = std::path::Path::new("/tmp/foo");
        let p1 = document_path(dir, "https://a.example/x", DocFormat::Html);
        let p2 = document_path(dir, "https://a.example/x", DocFormat::Html);
        assert_eq!(p1, p2);
        let p3 = document_path(dir, "https://a.example/y", DocFormat::Html);
        assert_ne!(p1, p3);
    }

    #[test]
    fn document_path_extension_matches_format() {
        let dir = std::path::Path::new("/tmp/foo");
        for (fmt, ext) in [
            (DocFormat::Html, "html"),
            (DocFormat::Json, "json"),
            (DocFormat::Xml, "xml"),
            (DocFormat::Plaintext, "txt"),
        ] {
            let p = document_path(dir, "https://a.example/x", fmt);
            assert_eq!(p.extension().and_then(|s| s.to_str()), Some(ext));
        }
    }
}
