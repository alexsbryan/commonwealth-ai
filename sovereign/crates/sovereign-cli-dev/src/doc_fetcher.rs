//! Documentation fetcher — the production implementation of
//! `honesty::Fetcher` that turns a URL into bytes indexed in the
//! project's `ProjectDocsStore`.
//!
//! ## What it does
//!
//! 1. HTTP-GET the URL (single-shot; no crawling).
//! 2. Stash the body under `<repo_root>/.sovereign/docs/<slug>.md`
//!    with a one-line provenance header (`> Source: <url>`). The
//!    file-on-disk copy means future sessions can re-index without
//!    re-fetching; it also means the docs survive offline work.
//! 3. Call `ProjectDocsStore::index_file` on the stashed file. The
//!    chunker treats the body as markdown — HTML mostly round-trips
//!    through the chunker as one big blob, which is fine; FTS still
//!    matches substrings.
//!
//! ## Why a closure for HTTP
//!
//! Keeps the fetcher testable without wiring reqwest into unit
//! tests. Production wraps an `async reqwest::Client` via
//! [`reqwest_http`]; tests pass an in-memory map.
//!
//! ## How it composes with the honesty protocol
//!
//! `ProjectDocsFetcher` implements `honesty::Fetcher` so the
//! runtime "I don't have docs for X, want me to fetch [url]?"
//! flow (future work) can plug it straight into a
//! `HonestyProtocol`. M6.6 itself uses the direct
//! [`fetch_many`] path — the docs-URL question collects a batch
//! up front, not one at a time with dedup.

// Same pattern as honesty.rs + found.rs — the runtime-fallback
// consumer lands after M6.6; until then parts of the module are
// only reachable via tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use corpus_engine_notes::ProjectDocsStore;

use crate::honesty::{FetchOutcome, Fetcher};

// ─── HTTP seam ───────────────────────────────────────────────────────────────

/// Result of fetching an HTTP resource: the raw bytes plus the
/// `Content-Type` header (if present) so the stasher can pick a
/// filename extension.
pub struct HttpFetched {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

/// Callable HTTP fetcher. Production uses reqwest; tests script
/// responses. Sync return: the caller's inside a tokio runtime
/// already, and doing one-shot GETs synchronously keeps the
/// Fetcher trait surface simple (no async_trait here).
pub type HttpFn = Box<dyn Fn(&str) -> Result<HttpFetched, String> + Send + Sync + 'static>;

/// Build a production HTTP fetcher backed by `reqwest::Client`.
/// 30-second timeout; 5MB cap on response body to avoid pathological
/// fetches pulling in a whole Wikipedia mirror.
pub fn reqwest_http(rt: tokio::runtime::Handle) -> HttpFn {
    Box::new(move |url: &str| {
        let url = url.to_string();
        let rt = rt.clone();
        tokio::task::block_in_place(move || {
            rt.block_on(async move {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .user_agent("sovereign-project-found/1")
                    .build()
                    .map_err(|e| format!("reqwest client: {e}"))?;
                let resp = client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e: reqwest::Error| format!("GET {url}: {e}"))?;
                if !resp.status().is_success() {
                    return Err(format!("GET {url}: HTTP {}", resp.status()));
                }
                let content_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e: reqwest::Error| format!("body {url}: {e}"))?;
                const MAX_BYTES: usize = 5 * 1024 * 1024;
                if bytes.len() > MAX_BYTES {
                    return Err(format!(
                        "response body too large ({} bytes > {MAX_BYTES})",
                        bytes.len()
                    ));
                }
                Ok(HttpFetched {
                    bytes: bytes.to_vec(),
                    content_type,
                })
            })
        })
    })
}

// ─── Stashing ────────────────────────────────────────────────────────────────

/// Where fetched documentation lives on disk. Stable path —
/// `<repo_root>/.sovereign/docs/`. Committing these files is the
/// operator's call; in practice teams should, so the docs survive
/// clean clones.
pub fn docs_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("docs")
}

/// Derive a safe, deterministic filename from a URL. The slug
/// preserves enough of the URL to be debuggable without being
/// overlong or containing path-unsafe characters.
///
/// Approach: take the URL, strip the scheme, replace any non
/// `[A-Za-z0-9._-]` byte with `_`, cap at 80 chars, append a
/// 6-char hash of the original URL so distinct URLs that collapse
/// to the same safe slug stay distinct.
pub fn slug_for(url: &str) -> String {
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let mut slug: String = stripped
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '_',
        })
        .collect();
    if slug.len() > 80 {
        slug.truncate(80);
    }
    // Short hash disambiguates collisions from the truncation /
    // underscore-mapping.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let suffix = format!("{:06x}", (h & 0xff_ffff) as u32);
    format!("{slug}.{suffix}.md")
}

/// Render the stashed body: one-line provenance header plus the
/// response content. We don't try to HTML→Markdown here; the
/// chunker treats everything as markdown but will still index
/// HTML as plain text, which is enough for FTS hits. If a project
/// needs richer handling we add a converter later.
pub fn render_stashed(url: &str, bytes: &[u8]) -> String {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            // Binary or invalid UTF-8 — store a pointer to the
            // original URL instead of garbage. FTS will still match
            // the URL itself.
            return format!(
                "> Source: {url}\n\n_(Response was not valid UTF-8; original bytes not indexed.)_\n"
            );
        }
    };
    format!("> Source: {url}\n\n{}\n", text.trim_end())
}

/// Write a rendered doc body to the docs directory. Returns the
/// path written. Creates parent directories on demand.
pub fn stash_doc(repo_root: &Path, url: &str, body: &str) -> std::io::Result<PathBuf> {
    let dir = docs_dir(repo_root);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(slug_for(url));
    std::fs::write(&path, body)?;
    Ok(path)
}

// ─── Fetcher ─────────────────────────────────────────────────────────────────

/// Production `honesty::Fetcher` that stashes + indexes. Holds the
/// HTTP seam and the doc store; callers construct it per session.
pub struct ProjectDocsFetcher<'a> {
    pub http: HttpFn,
    pub store: &'a ProjectDocsStore,
    pub repo_root: PathBuf,
    pub rt: tokio::runtime::Handle,
}

impl<'a> ProjectDocsFetcher<'a> {
    /// Batch-fetch helper for the M6.6 "paste N URLs" flow. Returns
    /// per-URL outcomes in input order so the caller can print a
    /// faithful summary. Errors never short-circuit the batch — a
    /// bad URL shouldn't lose the rest of the user's paste.
    pub fn fetch_many(&self, urls: &[String]) -> Vec<FetchSummary> {
        urls.iter()
            .map(|u| {
                let outcome = self.fetch_and_index(u);
                FetchSummary {
                    url: u.clone(),
                    outcome,
                }
            })
            .collect()
    }
}

#[derive(Debug)]
pub struct FetchSummary {
    pub url: String,
    pub outcome: FetchOutcome,
}

impl<'a> Fetcher for ProjectDocsFetcher<'a> {
    fn fetch_and_index(&self, source: &str) -> FetchOutcome {
        let fetched = match (self.http)(source) {
            Ok(f) => f,
            Err(e) => return FetchOutcome::Err(e),
        };
        let body = render_stashed(source, &fetched.bytes);
        let path = match stash_doc(&self.repo_root, source, &body) {
            Ok(p) => p,
            Err(e) => return FetchOutcome::Err(format!("stash: {e}")),
        };
        let rt = self.rt.clone();
        let repo_root = self.repo_root.clone();
        let store = self.store;
        let chunks_res = tokio::task::block_in_place(move || {
            rt.block_on(async move { store.index_file(&path, &repo_root).await })
        });
        match chunks_res {
            Ok(n) => FetchOutcome::Ok { bytes_indexed: n },
            Err(e) => FetchOutcome::Err(format!("index: {e}")),
        }
    }
}

// ─── URL parsing ─────────────────────────────────────────────────────────────

/// Split a user paste into URLs. Accepts:
/// - One URL per line (most natural for multiline paste),
/// - Multiple URLs on one line separated by whitespace (common
///   when the user types quickly),
/// - Surrounding whitespace,
/// - Blank lines.
///
/// Filters to things that look like HTTP(S) URLs. Typos and stray
/// words are dropped silently — the user sees the parsed list
/// before fetch, so they get a chance to correct.
pub fn parse_urls(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in input.split_whitespace() {
        if token.starts_with("http://") || token.starts_with("https://") {
            out.push(
                token
                    .trim_end_matches(&[',', ';', ')', ']'][..])
                    .to_string(),
            );
        }
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    #[test]
    fn slug_preserves_debuggable_prefix_and_disambiguates() {
        let a = slug_for("https://example.com/docs/websocket");
        let b = slug_for("https://example.com/docs/websocket#reconnect");
        assert!(a.starts_with("example.com_docs_websocket"));
        assert!(b.starts_with("example.com_docs_websocket_reconnect"));
        assert_ne!(a, b, "distinct URLs must hash-disambiguate");
        assert!(a.ends_with(".md"));
    }

    #[test]
    fn slug_caps_length_at_80_plus_suffix() {
        let long_url = format!("https://example.com/{}", "x".repeat(500));
        let slug = slug_for(&long_url);
        // 80 char prefix + ".xxxxxx.md" = 90 chars
        assert!(slug.len() <= 90, "slug too long: {}", slug.len());
    }

    #[test]
    fn render_stashed_prepends_provenance_header() {
        let body = render_stashed("https://docs.example.com/api", b"# Heading\n\nsome content");
        assert!(body.starts_with("> Source: https://docs.example.com/api\n"));
        assert!(body.contains("# Heading"));
    }

    #[test]
    fn render_stashed_noops_on_invalid_utf8() {
        let body = render_stashed("https://x/binary", &[0xff, 0xfe, 0x00, 0x01]);
        assert!(body.contains("not valid UTF-8"));
        assert!(body.contains("https://x/binary"));
    }

    #[test]
    fn parse_urls_handles_mixed_whitespace_and_punctuation() {
        let input = "https://a.com/one\nhttps://b.com/two, https://c.com/three;\n   not-a-url\nhttps://d.com/four)\n";
        let urls = parse_urls(input);
        assert_eq!(
            urls,
            vec![
                "https://a.com/one",
                "https://b.com/two",
                "https://c.com/three",
                "https://d.com/four",
            ]
        );
    }

    #[test]
    fn parse_urls_rejects_non_http() {
        let input = "ftp://example.com/x file:///etc/passwd javascript:alert(1)";
        let urls = parse_urls(input);
        assert!(urls.is_empty());
    }

    // ── ProjectDocsFetcher integration ─────────────────────────

    fn make_store(dir: &Path) -> ProjectDocsStore {
        ProjectDocsStore::open(&dir.join("project_docs.db")).unwrap()
    }

    fn scripted_http(
        responses: std::collections::HashMap<String, HttpFetched>,
    ) -> (HttpFn, Arc<Mutex<Vec<String>>>) {
        let called = Arc::new(Mutex::new(Vec::new()));
        let called2 = Arc::clone(&called);
        let responses = Arc::new(Mutex::new(responses));
        let f: HttpFn = Box::new(move |url: &str| {
            called2.lock().unwrap().push(url.to_string());
            let mut guard = responses.lock().unwrap();
            match guard.remove(url) {
                Some(r) => Ok(r),
                None => Err(format!("no scripted response for {url}")),
            }
        });
        (f, called)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_and_index_writes_to_disk_and_into_store() {
        let tmp = tempdir().unwrap();
        let repo_root = tmp.path().to_path_buf();
        std::fs::create_dir_all(repo_root.join(".sovereign")).unwrap();
        let store = make_store(&repo_root.join(".sovereign"));

        let url = "https://docs.example.com/reconnect";
        let body =
            b"# Reconnect\n\nClients should backoff exponentially with the keyword **gluon**.";
        let mut responses = std::collections::HashMap::new();
        responses.insert(
            url.to_string(),
            HttpFetched {
                bytes: body.to_vec(),
                content_type: Some("text/markdown".into()),
            },
        );
        let (http, _called) = scripted_http(responses);

        let fetcher = ProjectDocsFetcher {
            http,
            store: &store,
            repo_root: repo_root.clone(),
            rt: tokio::runtime::Handle::current(),
        };
        let outcome = fetcher.fetch_and_index(url);
        match outcome {
            FetchOutcome::Ok { bytes_indexed } => {
                assert!(bytes_indexed > 0, "at least one chunk should be indexed");
            }
            FetchOutcome::Err(e) => panic!("expected Ok, got Err({e})"),
        }

        // Disk artifact exists.
        let slug = slug_for(url);
        let stashed = repo_root.join(".sovereign").join("docs").join(&slug);
        assert!(
            stashed.exists(),
            "stashed file missing at {}",
            stashed.display()
        );
        let disk = std::fs::read_to_string(&stashed).unwrap();
        assert!(disk.contains("> Source: https://docs.example.com/reconnect"));
        assert!(disk.contains("gluon"));

        // FTS search finds the unique keyword.
        let hits = store.search("gluon", 5).await.unwrap();
        assert!(
            hits.iter().any(|h| h.file_path.contains(&slug)),
            "unique keyword must be findable after fetch"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_and_index_records_err_on_http_failure() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".sovereign")).unwrap();
        let store = make_store(&tmp.path().join(".sovereign"));
        let (http, _called) = scripted_http(std::collections::HashMap::new());

        let fetcher = ProjectDocsFetcher {
            http,
            store: &store,
            repo_root: tmp.path().to_path_buf(),
            rt: tokio::runtime::Handle::current(),
        };
        let outcome = fetcher.fetch_and_index("https://not-scripted.invalid/");
        match outcome {
            FetchOutcome::Err(e) => {
                assert!(e.contains("no scripted response"));
            }
            _ => panic!("expected Err"),
        }
        // No file stashed on error.
        let any_stashed = std::fs::read_dir(tmp.path().join(".sovereign").join("docs"))
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
        assert!(!any_stashed, "nothing should be stashed on http failure");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_many_does_not_short_circuit_on_one_failure() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".sovereign")).unwrap();
        let store = make_store(&tmp.path().join(".sovereign"));

        let ok_url = "https://ok.example.com/";
        let fail_url = "https://fail.example.com/";
        let mut responses = std::collections::HashMap::new();
        responses.insert(
            ok_url.to_string(),
            HttpFetched {
                bytes: b"# docs\n\ngood content keyword koala".to_vec(),
                content_type: None,
            },
        );
        // fail_url deliberately unscripted → error
        let (http, _called) = scripted_http(responses);
        let fetcher = ProjectDocsFetcher {
            http,
            store: &store,
            repo_root: tmp.path().to_path_buf(),
            rt: tokio::runtime::Handle::current(),
        };

        let urls = vec![fail_url.to_string(), ok_url.to_string()];
        let summary = fetcher.fetch_many(&urls);
        assert_eq!(summary.len(), 2);
        match &summary[0].outcome {
            FetchOutcome::Err(_) => {}
            _ => panic!("first URL was expected to fail"),
        }
        match &summary[1].outcome {
            FetchOutcome::Ok { .. } => {}
            _ => panic!("second URL was expected to succeed"),
        }
        // The OK side is findable.
        let hits = store.search("koala", 5).await.unwrap();
        assert!(hits.iter().any(|h| h.file_path.contains("ok.example.com")));
    }
}
