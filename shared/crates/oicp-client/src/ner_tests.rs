// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RemoteNer` against a stub `/v1/ner`: the moved wire, read as the client
//! reads it (pb-cli-llm).

use super::*;
use std::io::{Read, Write};

/// Answer one request per body in `replies`, in order, and hand back each
/// request's head and body.
fn stub(replies: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for body in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            seen.push(String::from_utf8_lossy(&buf[..n]).to_string());
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        }
        seen
    });
    (base, server)
}

const EXTRACTOR: &str =
    r#"{"model_id":"stub-ner","labels":["Person"],"threshold":0.55,"generation":"v2"}"#;

#[tokio::test]
async fn remote_ner_reads_the_moved_wire() {
    let identity =
        Box::leak(format!(r#"{{"extractor":{EXTRACTOR},"mentions":[]}}"#).into_boxed_str());
    let answer = Box::leak(
        format!(
            r#"{{"extractor":{EXTRACTOR},"mentions":[[{{"text":"Ada","label":"Person","char_start":0,"char_end":3,"score":0.9}}],[]]}}"#
        )
        .into_boxed_str(),
    );
    let (base, server) = stub(vec![identity, answer]);
    let remote = RemoteNer::connect(&base)
        .await
        .expect("reachable")
        .expect("an extractor");
    assert_eq!(remote.model_id(), "stub-ner");
    assert_eq!(remote.labels(), vec!["Person".to_string()]);
    assert_eq!(remote.generation(), GlinerGeneration::V2);

    let mentions = remote
        .extract_mentions_batch(&["Ada wrote", "nothing"])
        .expect("mentions");
    assert_eq!(mentions.len(), 2);
    assert_eq!(mentions[0][0].text, "Ada");
    assert_eq!(mentions[0][0].char_end, 3);
    assert!(mentions[1].is_empty());

    let seen = server.join().unwrap();
    assert!(
        seen[0].starts_with(&format!("POST {NER_PATH} ")),
        "{}",
        seen[0]
    );
    assert!(
        seen[1].contains(r#""texts":["Ada wrote","nothing"]"#),
        "{}",
        seen[1]
    );
    assert!(seen[1].contains(r#""pass":"entities""#), "{}", seen[1]);
}

#[tokio::test]
async fn a_node_without_the_model_is_none() {
    let (base, server) = stub(vec![r#"{"extractor":null,"mentions":[]}"#]);
    assert!(RemoteNer::connect(&base)
        .await
        .expect("reachable")
        .is_none());
    server.join().unwrap();
}
