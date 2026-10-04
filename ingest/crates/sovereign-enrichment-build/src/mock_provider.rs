// SPDX-License-Identifier: AGPL-3.0-or-later
//! A one-shot OpenAI-compatible host for dispatch tests: it accepts one
//! connection, records the request body, and answers with the body it was
//! given. Shared by the egress reds (`sovereign-pipeline`) and the dialect
//! tests here, behind `test-support` like [`crate::test_env`].

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A plain-content completion, the shape a text-only host returns.
pub const CONTENT_OK: &str = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

/// Bind on loopback; returns the base URL (no `/v1`) and the recorded bodies.
pub async fn mock_openai_host(response_body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&recorded);
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 8192];
        loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&buf[..pos]).to_string();
                let content_length = header
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let body_start = pos + 4;
                while buf.len() < body_start + content_length {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let end = (body_start + content_length).min(buf.len());
                rec.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[body_start..end]).to_string());
                break;
            }
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        let _ = sock.write_all(resp.as_bytes()).await;
    });
    (format!("http://{addr}"), recorded)
}
