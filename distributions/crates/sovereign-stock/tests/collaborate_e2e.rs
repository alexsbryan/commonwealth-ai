// SPDX-License-Identifier: AGPL-3.0-or-later
//! A collaborate ingest completes on the stock binary
//! (pb-distribution-f9-stock-collaborate-e2e; ship gate F9).
//!
//! Boots `sovereign-stock` against a REAL cw-rails (spawned from `CW_RAILS_BIN`,
//! else this build's target dir, as the daemon's fold e2es do), so the embed
//! model the boot advertises lands in cw-rails' ledger and the pull loop reads
//! it back. Then kicks a collaborate ingest off through
//! `/internal/corpus/collaborate` and follows it to the canonical:
//! work units -> the coordinator's own pull loop ->
//! `IngestPort::ingest_with_overrides` -> merge.
//!
//! The source is the smallest the JSONL planner accepts: a plain
//! `.jsonl` of structured-wikipedia article lines, read by the recipe's
//! `local_file` acquire and counted at `_downloads/<id>.extracted.jsonl`
//! (`CorpusEngine::count_jsonl_articles`).
//!
//! Linux only, like its sibling process e2es.
#![cfg(target_os = "linux")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
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

fn tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(40).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Poll `done`; on timeout, panic with both processes' log tails.
fn wait_until(what: &str, within: Duration, logs: &[&Path], mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        if Instant::now() >= deadline {
            let tails: Vec<String> = logs
                .iter()
                .map(|l| format!("last lines of {}:\n{}", l.display(), tail(l)))
                .collect();
            panic!("never saw: {what}, within {within:?}\n{}", tails.join("\n"));
        }
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

/// `CW_RAILS_BIN`, else `cw-rails` in this build's target dir (the test binary
/// runs from `<target>/<profile>/deps/`). Absent is a FAILURE naming the
/// build, never a skip (five-programs-62).
fn cw_rails_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("CW_RAILS_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("the test binary's own path");
    let beside = exe
        .parent()
        .and_then(Path::parent)
        .expect("a test binary under <target>/<profile>/deps")
        .join("cw-rails");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p commonwealth-rails --bin cw-rails`, or set CW_RAILS_BIN",
        beside.display()
    );
    beside
}

/// A collaborate recipe over a plain structured-wikipedia `.jsonl`, unfiltered
/// (the shipped `wikipedia` recipe's L5 filter would keep none of these).
fn jsonl_recipe(id: &str, source: &Path) -> String {
    format!(
        r#"
[corpus]
id = "{id}"
name = "Stock collaborate proof"
description = "a collaborate ingest through ingest's port in the stock process"
license = "CC-BY-SA-4.0"
mesh_sharing = true
size_compressed_gb = 0
size_indexed_gb = 0

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "wikipedia_jsonl"

[chunk]
type = "paragraph"
max_chars = 1024
overlap_chars = 0

[index]
fts = true
vector = false

[enrichment]
enabled = false
"#,
        path = source.display()
    )
}

/// `n` article lines, each with a lead long enough to clear the extractor's
/// minimum section text.
fn articles(n: u64) -> String {
    (0..n)
        .map(|i| {
            json!({
                "name": format!("Article {i}"),
                "identifier": i,
                "url": format!("https://en.wikipedia.org/wiki/A{i}"),
                "abstract": format!(
                    "Article {i} is a lead paragraph written long enough to survive the \
                     extractor's minimum and to be chunked on its own."
                ),
                "sections": []
            })
            .to_string()
                + "\n"
        })
        .collect()
}

/// PROOF (F9): a collaborate ingest kicked off on the stock binary completes
/// through ingest's port, until the canonical holds the slices.
#[test]
fn the_stock_install_completes_a_collaborate_ingest() {
    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir, data) = (
        root.path().join("home"),
        root.path().join("rails"),
        root.path().join("data"),
    );
    for d in [&home, &rails_dir, &data] {
        std::fs::create_dir_all(d).expect("dir");
    }

    // ── One node identity, as the handover leaves it ──────────────────
    // `svrn mesh up` copies svrn's `node_id` to cw-rails
    // (identity_handover.rs); the collaborate planner finds itself in
    // cw-rails' member list by that id.
    let node_id: [u8; 16] = *b"f9-collab-node-1";
    for dir in [&data, &rails_dir] {
        std::fs::write(dir.join("node_id"), node_id).expect("node_id");
    }

    // ── A real cw-rails, solo, on its own root ────────────────────────
    let rails_port = free_port();
    std::fs::write(
        rails_dir.join("rails.toml"),
        format!("name = \"stock-collaborate\"\nlisten = {rails_port}\n"),
    )
    .expect("rails.toml");
    let rails_log = rails_dir.join("cw-rails.log");
    let mut rails = Killed(
        Command::new(cw_rails_bin())
            .args(["run", "--local-only", "--data-dir"])
            .arg(&rails_dir)
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&rails_log).expect("log")))
            .spawn()
            .expect("cw-rails spawns"),
    );
    let rails_base = format!("http://127.0.0.1:{rails_port}");
    wait_until(
        "cw-rails answered /v1/mesh/status",
        Duration::from_secs(60),
        &[&rails_log],
        || {
            assert!(
                rails.0.try_wait().expect("try_wait").is_none(),
                "cw-rails exited during boot:\n{}",
                tail(&rails_log)
            );
            client()
                .get(format!("{rails_base}/v1/mesh/status"))
                .send()
                .is_ok_and(|r| r.status().is_success())
        },
    );

    // ── The stock binary, dialing it ──────────────────────────────────
    let (svrn, internal, serve) = (free_port(), free_port(), free_port());
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[engine]\nkind = \"mock\"\n\n[models]\nprimary = \"{r}/mock.gguf\"\n\
             embed = \"{r}/mock-embed.gguf\"\n\n[daemon]\nclient_port = {svrn}\n\
             internal_port = {internal}\nrails_base = \"{rails_base}\"\n\n\
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
            .env("CW_RAILS_BIN", cw_rails_bin())
            .env("SOVEREIGN_SERVE_PORT", serve.to_string())
            .env_remove("SOVEREIGN_WORKSPACE_DIR")
            .env_remove("SOVEREIGN_USE_LEGACY_PARTITION")
            .env("RUST_LOG", "info,sovereign_daemon::auto_ingest=debug")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    let logs = [log.as_path(), rails_log.as_path()];
    let client_base = format!("http://127.0.0.1:{svrn}");
    let internal_base = format!("http://127.0.0.1:{internal}");
    wait_until(
        "svrn's /status answered",
        Duration::from_secs(120),
        &logs,
        || {
            assert!(
                stock.0.try_wait().expect("try_wait").is_none(),
                "the stock process exited during boot:\n{}",
                tail(&log)
            );
            client().get(format!("{client_base}/status")).send().is_ok()
        },
    );
    wait_until(
        "the boot published its embed model to cw-rails' ledger",
        Duration::from_secs(60),
        &logs,
        || {
            let (status, body) = call(
                reqwest::Method::GET,
                &format!("{rails_base}/v1/ledger/inference/embed-model"),
                None,
            );
            status == 200 && !body.is_null()
        },
    );

    // ── The source, where the JSONL planner counts it ─────────────────
    let corpus = "stock-collab";
    let downloads = data.join("indexes").join("_downloads");
    std::fs::create_dir_all(&downloads).expect("downloads");
    let source = downloads.join(format!("{corpus}.extracted.jsonl"));
    std::fs::write(&source, articles(40)).expect("source");
    let (status, imported) = call(
        reqwest::Method::POST,
        &format!("{client_base}/internal/corpus/recipes/import"),
        Some(json!({ "toml_text": jsonl_recipe(corpus, &source) })),
    );
    assert!(
        status == 200 && imported["success"] == json!(true),
        "the recipe imports into ingest's registry: HTTP {status} {imported}"
    );

    // ── Kick off, and follow it to the canonical ──────────────────────
    let (status, handoff) = call(
        reqwest::Method::POST,
        &format!("{internal_base}/internal/corpus/collaborate"),
        Some(json!({ "corpus_id": corpus })),
    );
    assert_eq!(
        status,
        200,
        "the collaborate kickoff plans a queue: {handoff}\n{}",
        tail(&log)
    );
    assert!(
        handoff["partitions"]
            .as_array()
            .is_some_and(|p| p.is_empty()),
        "the kickoff seeds a pull queue, not static partitions: {handoff}"
    );
    wait_until(
        "this node's pull loop started on the handoff",
        Duration::from_secs(120),
        &logs,
        || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("pull_loop: started")
        },
    );
    wait_until(
        "the merged canonical hosted (`/status` knowledge.hosted_corpora)",
        Duration::from_secs(240),
        &logs,
        || {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(
                !text.contains("pull_loop: ingest_with_overrides failed"),
                "a unit failed in ingest's port:\n{}",
                tail(&log)
            );
            let (_, status) = call(reqwest::Method::GET, &format!("{client_base}/status"), None);
            status["knowledge"]["hosted_corpora"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == corpus))
        },
    );
    let boot = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        boot.contains("complete_unit→merge: queue-mode merge complete"),
        "the canonical came from the queue's merge:\n{}",
        tail(&log)
    );
    let pulled = client()
        .get(format!(
            "{internal_base}/internal/corpus/canonical/{corpus}"
        ))
        .send()
        .expect("the canonical route answered");
    assert_eq!(
        pulled.status().as_u16(),
        200,
        "a member pulls the merged canonical"
    );
    assert!(
        pulled.bytes().map(|b| b.len()).unwrap_or(0) > 0,
        "the canonical streams as a non-empty archive"
    );
}
