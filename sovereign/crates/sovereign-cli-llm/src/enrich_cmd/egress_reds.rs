// SPDX-License-Identifier: AGPL-3.0-or-later
//! R-5 red — a personal-corpus chunk must not reach a remote-model
//! payload.
//!
//! Order deep-research-t2a, red R-5: "a personal-corpus chunk can
//! reach a remote payload at HEAD — `enrich_cmd/providers.rs` still
//! carries zero privacy tokens". This test drives the shipped
//! `enrich --provider` dispatch (`DaemonInferenceClient` +
//! `ProviderRegistry`) with an operator-configured REMOTE
//! OpenAI-compatible provider pointed at a local mock dispatcher
//! that records request bodies, and asks it to complete a prompt
//! containing a personal-corpus chunk.
//!
//! At HEAD nothing refuses: the payload arrives at the mock, the
//! recording is non-empty, and this test FAILS (the red).
//!
//! After the fix (the egress boundary in sovereign-core::egress,
//! custody-release check + run-scoped consent grant, default-deny):
//! the same call is refused BEFORE any request leaves the machine —
//! the mock records nothing and `complete` returns an error that
//! names what was withheld. The red then goes green with zero
//! changes to this test's assertions.

use super::inference_client::DaemonInferenceClient;
use super::test_env::scoped_home;
use corpus_engine::enrichment::pipeline::ChatPrompt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A barebones HTTP dispatcher standing in for a remote model
/// provider. Accepts one connection, records the request body, and
/// answers with a minimal OpenAI-compatible completion so the
/// client's dispatch completes cleanly.
async fn mock_remote_dispatcher() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&recorded);
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        // Read until the whole body is in hand.
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
                let body = String::from_utf8_lossy(&buf[body_start..body_start + content_length])
                    .to_string();
                rec.lock().unwrap().push(body);
                break;
            }
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let resp_body = r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2},"finish_reason":"stop"}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        let _ = sock.write_all(resp.as_bytes()).await;
    });
    (format!("http://{addr}"), recorded)
}

/// R-5. The personal-chunk-to-remote-payload red. Fails at HEAD
/// (the payload arrives); green after the boundary refuses before
/// any request leaves the machine.
#[tokio::test]
async fn personal_chunk_must_not_reach_a_remote_payload() {
    let _home = scoped_home();
    let (base_url, recorded) = mock_remote_dispatcher().await;

    // Operator config: one REMOTE OpenAI-compatible provider pointed
    // at the dispatcher.
    let home = std::env::var("HOME").expect("scoped_home set HOME");
    let cfg_dir = format!("{home}/.config/sovereign");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        format!("{cfg_dir}/providers.toml"),
        format!("[providers.mockprov]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\n"),
    )
    .unwrap();

    // The shipped enrich --provider dispatch path, pointed at the
    // remote provider for the chat model.
    let client = DaemonInferenceClient::new(&base_url, "mockprov:remote-model", "embed").unwrap();

    // A personal-corpus chunk — the estate's own extraction content:
    // the material the boundary must refuse to send to a remote
    // provider.
    let chunk = "PRIVATE-CORPUS-MARKER: the 2024 board minutes of the private foundation, including the unlisted grant deliberations and the compensation figures, which must never leave this machine.";
    let prompt = ChatPrompt::new("Extract the named entities.", chunk);

    let result = tokio::time::timeout(Duration::from_secs(30), client.complete(&prompt)).await;

    let received = recorded.lock().unwrap().clone();
    assert!(
        received.is_empty(),
        "R-5 red (must fail at HEAD): a personal-corpus chunk reached the remote payload — {} request(s) recorded, first body: {}",
        received.len(),
        received
            .first()
            .map(|b| b.chars().take(120).collect::<String>())
            .unwrap_or_default(),
    );

    let err = result
        .expect("complete must not hang — the boundary refuses before any request")
        .expect_err("the boundary must refuse a personal-custody chunk to a remote provider");
    let msg = err.to_string();
    assert!(
        msg.contains("personal")
            || msg.contains("custody")
            || msg.contains("consent")
            || msg.contains("grant"),
        "the refusal must be typed and name what was withheld (personal custody / consent grant): {msg}"
    );
}
