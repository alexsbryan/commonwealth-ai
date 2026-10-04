// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest runs through its port in the stock process (pb-ingest-dial-daemon).
//!
//! svrn's daemon links no corpus-engine; the stock binary builds the engine
//! with ingest's face and hands it to svrn through `process::HostedIngest`.
//! Boots the `sovereign-stock` binary the way `svrn daemon run` execs it (the
//! argv the sovereign-daemon binary takes: `run --config <path>`) on a temp
//! root with the model-free mock engine and cw-rails pinned to a closed port,
//! then drives four ingest acts
//! svrn performs only through ingest's port:
//!
//! - a registry install: a local-file recipe imported through the recipe
//!   route, installed through `/internal/corpus/install`, served back as a
//!   canonical a member pulls (`/internal/corpus/canonical/{id}`, the
//!   member-act half of collaborate/pull, HUMAN-fp7 (a));
//! - a watched-folder ingest through the local-corpus routes, to its
//!   terminal counts;
//! - the governance recipe svrn renders, accepted by ingest's parser as a
//!   custom-ontology recipe (`render_governance_recipe`; moved here from
//!   d8_surface by pb-ingest-dial-daemon-tests-reads);
//! - recipe-author projects over ingest's recipe-project port: a store
//!   row, and a project the composition creates (pb-ingest-rehome-daemon).
//!
//! Linux only, like its sibling process e2es.
#![cfg(target_os = "linux")]

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-stock");

/// Kills and reaps the process on every exit path, panics included.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("ephemeral port")
        .port()
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client")
}

fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(40).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Poll `done`; on timeout, panic with the log's tail.
fn wait_until(what: &str, within: Duration, log: &Path, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(
            Instant::now() < deadline,
            "never saw: {what}, within {within:?}; last lines of {}:\n{}",
            log.display(),
            log_tail(log)
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `(status, body)` of one request; the body is `Null` when it is not JSON.
fn call(method: reqwest::Method, url: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = client().request(method, url);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req
        .send()
        .unwrap_or_else(|e| panic!("nothing answered {url}: {e}"));
    let status = resp.status().as_u16();
    (status, resp.json().unwrap_or(Value::Null))
}

/// A local-file recipe (`daemon_port_parity`'s shape): acquired, extracted and
/// chunked with no network, shared on the mesh by default.
fn local_recipe(id: &str, source: &Path) -> String {
    format!(
        r#"
[corpus]
id = "{id}"
name = "Stock proof"
description = "a recipe ingested through ingest's port in the stock process"
license = "Public Domain"
size_compressed_gb = 0.001
size_indexed_gb = 0.001

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 0

[index]
fts = true
vector = false
"#,
        path = source.display()
    )
}

/// PROOF (pb-ingest-dial-daemon): a registry install, a member pull of its
/// canonical, a watched-folder ingest and the governance recipe's parse all
/// complete through ingest's port on the stock binary.
#[test]
fn the_stock_install_ingests_through_ingests_port() {
    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir, data) = (
        root.path().join("home"),
        root.path().join("rails"),
        root.path().join("data"),
    );
    for d in [&home, &rails_dir, &data] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (svrn, internal, serve, dead_rails) = (free_port(), free_port(), free_port(), free_port());
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[engine]\nkind = \"mock\"\n\n[models]\nprimary = \"{r}/mock.gguf\"\n\
             embed = \"{r}/mock-embed.gguf\"\n\n[daemon]\nclient_port = {svrn}\n\
             internal_port = {internal}\nrails_base = \"http://127.0.0.1:{dead_rails}\"\n\n\
             [data]\ndir = \"{d}\"\n",
            r = root.path().display(),
            d = data.display()
        ),
    )
    .expect("config");
    let log = root.path().join("stock.stderr.log");
    let mut stock = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", root.path().join("svrnmesh"))
            .env("CW_RAILS_DIR", &rails_dir)
            .env("CW_RAILS_BIN", root.path().join("no-cw-rails"))
            .env("SOVEREIGN_SERVE_PORT", serve.to_string())
            .env_remove("SOVEREIGN_WORKSPACE_DIR")
            .env_remove("RUST_LOG")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    let client_base = format!("http://127.0.0.1:{svrn}");
    let internal_base = format!("http://127.0.0.1:{internal}");
    wait_until(
        "svrn's /status answered",
        Duration::from_secs(120),
        &log,
        || {
            assert!(
                stock.0.try_wait().expect("try_wait").is_none(),
                "the stock process exited during boot:\n{}",
                log_tail(&log)
            );
            client().get(format!("{client_base}/status")).send().is_ok()
        },
    );
    let boot = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        boot.contains("the ingest program is composed in this process"),
        "the stock process did not compose ingest:\n{}",
        log_tail(&log)
    );

    // ── A registry install, and a member's pull of its canonical ──────
    let source = root.path().join("source.txt");
    std::fs::write(
        &source,
        "Alpha paragraph, long enough to survive a chunker.\n\n\
         Beta paragraph, likewise long enough to be kept.\n",
    )
    .expect("source");
    let corpus = "stock-proof";
    let (status, imported) = call(
        reqwest::Method::POST,
        &format!("{client_base}/internal/corpus/recipes/import"),
        Some(json!({ "toml_text": local_recipe(corpus, &source) })),
    );
    assert!(
        status == 200 && imported["success"] == json!(true),
        "the recipe imports into ingest's registry: HTTP {status} {imported}"
    );
    let (status, spawned) = call(
        reqwest::Method::POST,
        &format!("{internal_base}/internal/corpus/install"),
        Some(json!({ "corpus_id": corpus })),
    );
    assert_eq!(status, 200, "the install is spawned: {spawned}");
    wait_until(
        "the installed corpus hosted (`/status` knowledge.hosted_corpora)",
        Duration::from_secs(90),
        &log,
        || {
            let (_, status) = call(reqwest::Method::GET, &format!("{client_base}/status"), None);
            status["knowledge"]["hosted_corpora"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == corpus))
        },
    );
    let pulled = client()
        .get(format!(
            "{internal_base}/internal/corpus/canonical/{corpus}"
        ))
        .send()
        .expect("the canonical route answered");
    let status = pulled.status().as_u16();
    let fingerprint = pulled
        .headers()
        .get("X-Canonical-Fingerprint")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = pulled.bytes().map(|b| b.len()).unwrap_or(0);
    assert_eq!(status, 200, "a member pulls the installed canonical");
    assert!(
        fingerprint.is_some_and(|f| !f.is_empty()),
        "the stream names the canonical's fingerprint"
    );
    assert!(bytes > 0, "the canonical streams as a non-empty archive");

    // ── A watched-folder ingest, to its terminal counts ───────────────
    let folder = root.path().join("watched");
    std::fs::create_dir_all(&folder).expect("folder");
    for n in 0..3 {
        std::fs::write(
            folder.join(format!("doc-{n}.txt")),
            format!("document {n}: quiet hours begin at 11 PM"),
        )
        .expect("doc");
    }
    let cfg = sovereign_contracts::daemon_wire::LocalCorpusConfig::watched_folder(
        folder,
        "Watched proof".to_string(),
        Default::default(),
    );
    let (status, stored) = call(
        reqwest::Method::POST,
        &format!("{client_base}/internal/corpus/local"),
        Some(serde_json::to_value(&cfg).expect("config encodes")),
    );
    assert_eq!(status, 200, "the watched folder registers: {stored}");
    let id = stored["id"].as_str().expect("a corpus id").to_string();
    let (status, ack) = call(
        reqwest::Method::POST,
        &format!("{client_base}/internal/corpus/local/{id}/ingest"),
        Some(json!({})),
    );
    assert_eq!(status, 202, "the ingest is submitted: {ack}");
    let mut last = Value::Null;
    wait_until(
        "the watched-folder ingest reached its terminal frame",
        Duration::from_secs(90),
        &log,
        || {
            let (_, body) = call(
                reqwest::Method::GET,
                &format!("{client_base}/internal/corpus/local/{id}/ingest/progress"),
                None,
            );
            last = body;
            last["finished"] == json!(true)
        },
    );
    let outcome = &last["outcome"];
    assert!(
        outcome["error"].is_null(),
        "the watched-folder ingest succeeds: {last}"
    );
    assert_eq!(
        outcome["stats"]["files_indexed"],
        json!(3),
        "three files in, three files indexed: {last}"
    );
    assert!(
        outcome["stats"]["chunks_written"].as_u64().unwrap_or(0) > 0,
        "the ingest wrote chunks: {last}"
    );

    // ── The governance recipe, parsed by ingest ───────────────────────
    let (status, written) = call(
        reqwest::Method::POST,
        &format!("{client_base}/internal/governance/house-rules/recipe"),
        Some(json!({ "display_name": "House \"rules\"", "source_path": "/srv/house" })),
    );
    assert_eq!(
        status, 200,
        "ingest's parser accepts svrn's governance template as a custom-ontology \
         recipe: {written}"
    );
    assert!(
        written["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("house-rules/recipe.toml")),
        "the recipe lands under ingest's recipes dir: {written}"
    );

    // ── Recipe-author projects, through ingest's recipe-project port ──
    // (pb-ingest-rehome-daemon.) The store row round-trips, a taken id is a
    // conflict, and a project created by the composition lists, serves its
    // dashboard and lays its tree down under svrn's root.
    let boot = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        boot.contains("recipe-author features.db opened"),
        "ingest's recipe authoring opened the project store:\n{}",
        log_tail(&log)
    );
    let row = json!({ "id": "feat-quiet-hours", "title": "Quiet hours", "charter_md": "Whole." });
    let features = format!("{client_base}/v1/features/projects");
    let (status, provisioned) = call(reqwest::Method::POST, &features, Some(row.clone()));
    assert_eq!(status, 201, "the store row provisions: {provisioned}");
    let (status, one) = call(
        reqwest::Method::GET,
        &format!("{features}/feat-quiet-hours"),
        None,
    );
    assert_eq!(
        (status, &one["charter_md"]),
        (200, &json!("Whole.")),
        "the row reads back: {one}"
    );
    let (status, dup) = call(reqwest::Method::POST, &features, Some(row));
    assert_eq!(status, 409, "a taken id is a conflict: {dup}");

    let projects = format!("{client_base}/v1/recipe-projects");
    let (status, created) = call(
        reqwest::Method::POST,
        &projects,
        Some(json!({ "title": "Roman coin hoards", "charter_md": "Catalogue them." })),
    );
    assert_eq!(status, 201, "the composition creates a project: {created}");
    let feature_id = created["feature_id"].as_str().expect("an id").to_string();
    let (_, listed) = call(reqwest::Method::GET, &projects, None);
    assert!(
        listed
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["feature_id"] == json!(feature_id))),
        "the project lists: {listed}"
    );
    let (status, dash) = call(
        reqwest::Method::GET,
        &format!("{projects}/{feature_id}/dashboard"),
        None,
    );
    assert_eq!(
        (status, &dash["title"]),
        (200, &json!("Roman coin hoards")),
        "the dashboard serves: {dash}"
    );
    assert!(
        root.path()
            .join("svrnmesh")
            .join("recipe-projects")
            .join(&feature_id)
            .is_dir(),
        "the project's tree is laid down under svrn's root"
    );
}
