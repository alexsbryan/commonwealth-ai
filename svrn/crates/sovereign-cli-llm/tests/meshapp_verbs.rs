// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn meshapp` runs from svrn's cli-llm sibling (pb-meshapp-apps): the
//! corpus explorer reads svrn's corpora, so its verbs are svrn's. Runs the
//! real sibling binary against temp dirs, never the operator's `~/.svrnmesh`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-llm");

/// Two entities, so the graph has something the fixture alone supplies.
const ATOMS_JSON: &str = r#"{
  "schema_version": "2.3",
  "atoms": [
    {"atom_type":"Entity","data":{
      "id":"entity-aaa","canonical_name":"El Paso","entity_type":"institution",
      "first_appearance":{"chunk_id":"sec_00002","passage_preview":"El Paso Corp."},
      "description":"Energy company.","salience":0.5,"enrichment_depth":"extracted"}},
    {"atom_type":"Entity","data":{
      "id":"entity-bbb","canonical_name":"Kenneth Lay","entity_type":"person",
      "first_appearance":{"chunk_id":"sec_00001","passage_preview":"Ken Lay"},
      "description":"Chairman.","salience":0.9,"enrichment_depth":"extracted"}}
  ]
}"#;

/// Kills the dev server when the test ends, pass or fail.
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    l.local_addr().expect("local addr").port()
}

#[tokio::test]
async fn meshapp_dev_serves_the_bundle_and_answers_graph_from_the_index() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bundle = tmp.path().join("meshapp").join("demo");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(
        bundle.join("index.html"),
        "<html><head></head><body>demo</body></html>",
    )
    .unwrap();
    std::fs::write(bundle.join("meshapp.json"), r#"{"corpus":"governance"}"#).unwrap();
    let index = tmp.path().join("index");
    std::fs::create_dir_all(index.join("atlas")).unwrap();
    std::fs::write(index.join("atlas").join("atoms.json"), ATOMS_JSON).unwrap();

    let port = free_port();
    let mut child = Child(
        Command::new(BIN)
            .args(["meshapp", "dev", "demo", "--dir"])
            .arg(&bundle)
            .arg("--index")
            .arg(&index)
            .args(["--port", &port.to_string()])
            .env("SVRNMESH_DATA_DIR", tmp.path())
            .env("SOVEREIGN_DATA_DIR", tmp.path())
            .env_remove("RUST_LOG")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sovereign-cli-llm"),
    );

    // The mount trace is the first thing the shell prints once it serves.
    let stderr = child.0.stderr.take().expect("piped stderr");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = Vec::new();
    let mount = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) if line.contains("shell: mounted") => break line,
            Ok(line) => seen.push(line),
            Err(_) => panic!("no mount trace on stderr within 60s: {seen:#?}"),
        }
    };
    assert!(
        mount.contains("INFO") && mount.contains("meshapp_dev"),
        "{mount}"
    );

    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();
    let page = http
        .get(format!("{base}/"))
        .send()
        .await
        .expect("GET /")
        .text()
        .await
        .unwrap();
    assert!(
        page.contains("demo") && page.contains("/__meshapp_dev.js"),
        "the bundle is served with the dev shim injected: {page}"
    );

    let graph: serde_json::Value = http
        .post(format!("{base}/__meshapp/graph"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("POST /__meshapp/graph")
        .json()
        .await
        .expect("graph answers JSON");
    let text = graph.to_string();
    assert!(
        text.contains("entity-aaa") && text.contains("Kenneth Lay"),
        "the graph is the fixture's two entities: {graph:#}"
    );
}

#[test]
fn meshapp_list_names_an_installed_app() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // An installed app is a directory holding its `meshapp.json`.
    let app = tmp.path().join("meshapps").join("seeded-app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("meshapp.json"), r#"{"corpus":"governance"}"#).unwrap();
    let out = Command::new(BIN)
        .args(["meshapp", "list"])
        .current_dir(tmp.path())
        .env("SVRNMESH_DATA_DIR", tmp.path())
        .env("SOVEREIGN_DATA_DIR", tmp.path())
        .output()
        .expect("spawn sovereign-cli-llm");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Installed ("), "{stdout}");
    assert!(stdout.contains("seeded-app"), "{stdout}");
}
