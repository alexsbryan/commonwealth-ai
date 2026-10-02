// SPDX-License-Identifier: AGPL-3.0-or-later
//! A write a solo cw-rails acknowledged survives a SIGKILL sent the moment
//! the answer arrives (pc-solo-durable, five-programs-66). The store is in
//! memory and a solo node has no peer copy, so the journal is the only place
//! the write can live; fp-solo-clients' e2e lost one killed inside the pump's
//! 2 s window.
#![cfg(unix)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;

/// A store namespace and a local-only one: both journal on a solo node.
const NAMESPACES: [&str; 2] = ["kv-durable", "portfolio-private"];

/// Writes per namespace, all acknowledged before the kill.
const WRITES: usize = 25;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Kills its cw-rails on drop, so a failed assert leaves no orphan.
struct Rails(Child);

impl Drop for Rails {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `cw-rails run` on `root`, solo and hermetic, once `/v1/mesh/status`
/// answers. Its stderr goes to `log`, read back if it exits first.
async fn start(root: &Path, log: &Path) -> (Rails, String) {
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_cw-rails"))
        .args(["run", "--local-only", "--listen", &port.to_string()])
        .arg("--data-dir")
        .arg(root)
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(log).unwrap())
        .spawn()
        .unwrap();
    let mut rails = Rails(child);
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(exit) = rails.0.try_wait().unwrap() {
            let said = std::fs::read_to_string(log).unwrap_or_default();
            panic!("cw-rails exited {exit} before serving:\n{said}");
        }
        if reqwest::get(format!("{base}/v1/mesh/status"))
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return (rails, base);
        }
        assert!(
            Instant::now() < deadline,
            "cw-rails never served within 60s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn an_acknowledged_write_survives_a_sigkill_of_a_solo_cw_rails() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("rails");
    let client = reqwest::Client::new();
    let b64 = base64::engine::general_purpose::STANDARD;

    let (mut first, base) = start(&root, &parent.path().join("first.log")).await;
    let status: serde_json::Value = client
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let self_id = status["self"]["node_id_hex"]
        .as_str()
        .and_then(commonwealth_core::ids::NodeId::from_hex)
        .unwrap_or_else(|| panic!("status names no node id: {status}"));
    let mut acked = Vec::new();
    for namespace in NAMESPACES {
        for i in 0..WRITES {
            let sent = Instant::now();
            let changed: bool = client
                .post(format!("{base}/v1/mesh/kv/entry"))
                .json(&serde_json::json!({
                    "app_id": namespace,
                    "key": format!("holding/{i}"),
                    "value": b64.encode(format!("{namespace}/{i}")),
                    "origin": self_id,
                }))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            acked.push(sent.elapsed());
            assert!(changed, "{namespace}: the door wrote nothing");
        }
    }
    // The cost of the fix, read off the same run: ack latency per write.
    acked.sort();
    eprintln!(
        "ack latency over {} writes: p50 {} us, p95 {} us, max {} us",
        acked.len(),
        acked[acked.len() / 2].as_micros(),
        acked[acked.len() * 95 / 100].as_micros(),
        acked[acked.len() - 1].as_micros()
    );
    // SIGKILL, inside the pump's interval: nothing runs after the answer.
    first.0.kill().unwrap();
    first.0.wait().unwrap();

    let (_second, base) = start(&root, &parent.path().join("second.log")).await;
    for namespace in NAMESPACES {
        for i in 0..WRITES {
            let entry: serde_json::Value = client
                .get(format!(
                    "{base}/v1/mesh/kv/entry?app_id={namespace}&key=holding/{i}"
                ))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            let value = entry["value"].as_str().unwrap_or_else(|| {
                panic!("{namespace}/{i}: an acknowledged write was lost: {entry}")
            });
            assert_eq!(
                b64.decode(value).unwrap(),
                format!("{namespace}/{i}").as_bytes()
            );
        }
    }
}
