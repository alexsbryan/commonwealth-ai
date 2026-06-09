// SPDX-License-Identifier: AGPL-3.0-or-later
//! OCR cleanup pass via the daemon's `/v1/chat/completions` endpoint.
//!
//! Tesseract emits raw text with column-flow artefacts, hyphenation
//! breaks, page headers, and spurious whitespace. Indexing it raw
//! degrades retrieval quality more than it helps. We send each page
//! to the already-loaded fast slot with a tightly-scoped reformatting
//! prompt and feed the cleaned markdown forward.
//!
//! All LLM work in this codebase flows through the daemon — never
//! bundled in-process. The cleanup call is a normal OpenAI-compatible
//! chat completion: any model loaded under the `cleanup_model` id in
//! `OcrCtx` will work, including peer-routed inference on a mesh.
//!
//! On failure (HTTP error, timeout, malformed response) the caller
//! treats the page as a per-page failure and inserts the
//! `<!-- could not be read -->` marker.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::OcrCtx;

const CLEANUP_SYSTEM_PROMPT: &str = "\
You are an OCR cleanup tool. The user's input is the raw output of an \
OCR engine on one page of a scanned document. Your job is to return \
the same content as clean markdown that a search index can use.\n\n\
Rules:\n\
- Fix obvious OCR errors (rn → m, l → 1 in numeric contexts, 0 → O \
  inside words, etc.) only when you are confident.\n\
- Reflow paragraphs that OCR broke across line endings. Restore \
  hyphenated words split at end-of-line.\n\
- Drop running headers, running footers, isolated page numbers, and \
  scanner artefacts (\"Page 3 of 12\", date stamps, watermark text).\n\
- Preserve genuine headings as `## Heading`. Preserve list structure.\n\
- If the page contains a table, render it as a markdown table when \
  feasible; otherwise as labelled rows.\n\
- Do not invent text that isn't in the OCR. If a passage is too \
  garbled to clean, leave it as-is rather than guessing.\n\
- Output the cleaned page only. No preamble, no commentary, no \
  fences around the whole output.";

#[derive(Debug, Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    /// Cap output at a sensible bound. A standard OCR'd page is at
    /// most ~1500 tokens; 4096 leaves headroom for verbose tables.
    max_tokens: u32,
    /// Disable thinking mode if the daemon's resolved model is
    /// thinking-capable — cleanup is a deterministic reformatting
    /// task, not a reasoning task.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

#[derive(Debug)]
pub enum CleanupError {
    /// Could not reach the daemon. Usually means it's not running —
    /// surface clearly so the desktop layer can suggest a restart.
    Unreachable(String),
    /// Daemon returned a non-2xx response.
    Http { status: u16, body: String },
    /// Response shape didn't match what we expected.
    Malformed(String),
    /// Request timed out.
    Timeout,
}

impl CleanupError {
    pub fn user_message(&self) -> String {
        match self {
            CleanupError::Unreachable(e) => {
                format!("could not reach inference daemon: {e}")
            }
            CleanupError::Http { status, body } => {
                format!(
                    "daemon rejected cleanup request ({status}): {}",
                    body.trim()
                )
            }
            CleanupError::Malformed(e) => format!("daemon response malformed: {e}"),
            CleanupError::Timeout => "cleanup request timed out".to_string(),
        }
    }
}

/// Send one page of raw OCR text through the cleanup model. Returns
/// the cleaned markdown verbatim — no trimming, no normalization —
/// so the caller can reason about what the model emitted.
pub async fn cleanup_page(raw: &str, ctx: &OcrCtx) -> Result<String, CleanupError> {
    // Ship empties straight through. Calling the model on an empty
    // page wastes a request and produces hallucinated content.
    if raw.trim().is_empty() {
        return Ok(String::new());
    }

    let url = format!(
        "{}/v1/chat/completions",
        ctx.daemon_base_url.trim_end_matches('/')
    );

    let req_body = ChatCompletionsRequest {
        model: &ctx.cleanup_model,
        messages: vec![
            ChatMessage {
                role: "system",
                content: CLEANUP_SYSTEM_PROMPT,
            },
            ChatMessage {
                role: "user",
                content: raw,
            },
        ],
        temperature: 0.1,
        max_tokens: 4096,
        thinking: Some(false),
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(ctx.cleanup_timeout_secs))
        .build()
        .map_err(|e| CleanupError::Unreachable(format!("build client: {e}")))?;

    let resp = client
        .post(&url)
        .json(&req_body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                CleanupError::Timeout
            } else {
                CleanupError::Unreachable(e.to_string())
            }
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CleanupError::Http {
            status: status.as_u16(),
            body,
        });
    }

    let parsed: ChatCompletionsResponse = resp
        .json()
        .await
        .map_err(|e| CleanupError::Malformed(format!("parse json: {e}")))?;

    let first = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| CleanupError::Malformed("no choices in response".to_string()))?;

    Ok(first.message.content)
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    /// Tiny test HTTP server that captures the first POST body and
    /// replies with a canned chat-completions JSON. Avoids pulling in
    /// `mockito` / `wiremock` for one test.
    async fn spawn_one_shot(
        canned_response_body: &'static str,
    ) -> (String, Arc<Mutex<Option<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16 * 1024];
            let mut total = Vec::new();
            // Read until end of headers + the declared content-length.
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                let s = String::from_utf8_lossy(&total);
                if let Some(hdr_end) = s.find("\r\n\r\n") {
                    let headers = &s[..hdr_end];
                    let body_start = hdr_end + 4;
                    let cl = headers
                        .lines()
                        .find_map(|l| {
                            let l = l.to_ascii_lowercase();
                            l.strip_prefix("content-length: ")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if total.len() >= body_start + cl {
                        let body = String::from_utf8_lossy(&total[body_start..body_start + cl])
                            .to_string();
                        *captured_clone.lock().await = Some(body);
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                canned_response_body.len(),
                canned_response_body
            );
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.shutdown().await.unwrap();
        });
        (format!("http://127.0.0.1:{port}"), captured)
    }

    #[tokio::test]
    async fn empty_input_returns_empty_without_calling_daemon() {
        // Daemon URL points at port 1 (always closed) — proves we
        // never tried to reach it.
        let ctx = OcrCtx::for_test(
            PathBuf::from("/bin/true"),
            PathBuf::from("/nonexistent"),
            "http://127.0.0.1:1".into(),
        );
        let out = cleanup_page("   \n   \n", &ctx).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn round_trip_ok_response() {
        let canned =
            r#"{"choices":[{"message":{"role":"assistant","content":"Cleaned **text**."}}]}"#;
        let (url, _captured) = spawn_one_shot(canned).await;
        let ctx = OcrCtx::for_test(
            PathBuf::from("/bin/true"),
            PathBuf::from("/nonexistent"),
            url,
        );
        let out = cleanup_page("raw OCR\nbroken\nlines", &ctx).await.unwrap();
        assert_eq!(out, "Cleaned **text**.");
    }

    #[tokio::test]
    async fn request_includes_system_and_user_messages() {
        let canned = r#"{"choices":[{"message":{"role":"assistant","content":""}}]}"#;
        let (url, captured) = spawn_one_shot(canned).await;
        let ctx = OcrCtx::for_test(
            PathBuf::from("/bin/true"),
            PathBuf::from("/nonexistent"),
            url,
        );
        let _ = cleanup_page("input page text", &ctx).await.unwrap();
        let body = captured.lock().await.clone().expect("body captured");
        // Spot-check the request shape — model id + raw page text +
        // system prompt anchor phrase all present.
        assert!(body.contains("\"model\":\"fast\""), "body: {body}");
        assert!(body.contains("input page text"), "body: {body}");
        assert!(body.contains("OCR cleanup tool"), "body: {body}");
    }

    #[tokio::test]
    async fn unreachable_daemon_yields_typed_error() {
        // Port 1 is privileged and reliably-closed; connect refused.
        let ctx = OcrCtx::for_test(
            PathBuf::from("/bin/true"),
            PathBuf::from("/nonexistent"),
            "http://127.0.0.1:1".into(),
        );
        // Non-empty raw forces an HTTP attempt.
        let res = cleanup_page("real content", &ctx).await;
        match res {
            Err(CleanupError::Unreachable(_)) | Err(CleanupError::Timeout) => {}
            other => panic!("expected Unreachable/Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_response_yields_typed_error() {
        let canned = r#"{"not_choices":[]}"#;
        let (url, _captured) = spawn_one_shot(canned).await;
        let ctx = OcrCtx::for_test(
            PathBuf::from("/bin/true"),
            PathBuf::from("/nonexistent"),
            url,
        );
        let res = cleanup_page("anything", &ctx).await;
        match res {
            Err(CleanupError::Malformed(_)) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
