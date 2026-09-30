// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's LLM verbs retrieve through ingest's composed engine
//! (pb-cli-llm-ingest-move-compose).
//!
//! cli-llm builds no engine: `sovereign-cli-llm-stock` hands its entry the
//! stock distribution's ingest composition, as the stock daemon gets it. Boots
//! the stock daemon (the argv the sovereign-daemon binary takes: `run --config
//! <path>`) on a temp root with the model-free mock engine, installs a
//! one-file fixture corpus through ingest's port, then asks two retrieval
//! lanes of the composed binary — `chat inspect` and the `__probe` retrieve
//! stage — for the question the fixture answers. Both return the passages
//! the in-process engine returned at this row's start (`expected`). The bare
//! `sovereign-cli-llm` names the absence on the same two lanes.
//!
//! Linux only, like its sibling process e2es.
#![cfg(target_os = "linux")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const STOCK: &str = env!("CARGO_BIN_EXE_sovereign-stock");
const COMPOSED: &str = env!("CARGO_BIN_EXE_sovereign-cli-llm-stock");

/// The fixture's paragraphs, and the one the question's terms hit.
const ALPHA: &str = "Alpha paragraph: the lighthouse keeper logs every passing ship.";
const BETA: &str = "Beta paragraph: the orchard harvest begins after the first frost.";
const QUESTION: &str = "lighthouse keeper ship";
const CORPUS: &str = "compose-proof";

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

fn recipe(source: &Path) -> String {
    format!(
        r#"
[corpus]
id = "{CORPUS}"
name = "Compose proof"
description = "a fixture the CLI retrieves from through ingest's composed engine"
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

/// The stock daemon, booted, with the fixture corpus installed.
struct Fixture {
    root: tempfile::TempDir,
    data: PathBuf,
    client_base: String,
    serve: u16,
    _stock: Killed,
}

fn boot() -> Fixture {
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
    let stock = Killed(
        Command::new(STOCK)
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
        || client().get(format!("{client_base}/status")).send().is_ok(),
    );

    let source = root.path().join("source.txt");
    std::fs::write(&source, format!("{ALPHA}\n\n{BETA}\n")).expect("source");
    let (status, imported) = call(
        reqwest::Method::POST,
        &format!("{client_base}/internal/corpus/recipes/import"),
        Some(json!({ "toml_text": recipe(&source) })),
    );
    assert!(
        status == 200 && imported["success"] == json!(true),
        "the recipe imports: HTTP {status} {imported}"
    );
    let (status, spawned) = call(
        reqwest::Method::POST,
        &format!("{internal_base}/internal/corpus/install"),
        Some(json!({ "corpus_id": CORPUS })),
    );
    assert_eq!(status, 200, "the install is spawned: {spawned}");
    wait_until(
        "the fixture corpus hosted",
        Duration::from_secs(90),
        &log,
        || {
            let (_, status) = call(reqwest::Method::GET, &format!("{client_base}/status"), None);
            status["knowledge"]["hosted_corpora"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == CORPUS))
        },
    );
    Fixture {
        root,
        data,
        client_base,
        serve,
        _stock: stock,
    }
}

/// Run `bin <verb> --daemon … --data-dir … <rest>` against the fixture.
fn cli(fx: &Fixture, bin: &Path, verb: &[&str], rest: &[&str]) -> Output {
    Command::new(bin)
        .args(verb)
        // The mock engine advertises no embedding-like id, so name it.
        .args([
            "--daemon",
            &fx.client_base,
            "--embed-model",
            "mock",
            "--data-dir",
        ])
        .arg(&fx.data)
        .args(rest)
        .env("HOME", fx.root.path().join("home"))
        .env("SVRNMESH_DATA_DIR", fx.root.path().join("svrnmesh"))
        .env("SOVEREIGN_SERVE_PORT", fx.serve.to_string())
        .env_remove("RUST_LOG")
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", bin.display()))
}

fn says(out: &Output) -> String {
    format!(
        "exit {:?}\n--- stdout\n{}\n--- stderr\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `chat inspect --format json`: the retrieved `(corpus_id, content)` pairs.
fn inspect(fx: &Fixture, bin: &Path) -> Result<Vec<(String, String)>, String> {
    let out = cli(
        fx,
        bin,
        &["chat", "inspect"],
        &[QUESTION, "--corpus", CORPUS, "--format", "json"],
    );
    if !out.status.success() {
        return Err(says(&out));
    }
    let body: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("{e}: {}", says(&out)))?;
    Ok(body["corpora"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| {
            let id = c["corpus_id"].as_str().unwrap_or_default().to_string();
            c["chunks"].as_array().into_iter().flatten().map(move |ch| {
                (
                    id.clone(),
                    ch["content"]
                        .as_str()
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                )
            })
        })
        .collect())
}

/// `__probe` retrieve: the one question's pool as `(corpus_id, content)`.
fn probe_retrieve(fx: &Fixture, bin: &Path) -> Result<Vec<(String, String)>, String> {
    let dir = fx.root.path().join(format!(
        "probe-{}",
        bin.file_name().and_then(|n| n.to_str()).unwrap_or("bin")
    ));
    std::fs::create_dir_all(&dir).expect("probe dir");
    let (request, output) = (dir.join("request.json"), dir.join("evidence.json"));
    std::fs::write(
        &request,
        serde_json::to_vec(&json!({
            "mode": "retrieve",
            "questions": [{ "id": "q1", "question": QUESTION }],
            "corpus": CORPUS,
            "limit": 5,
            "isolate": false,
        }))
        .expect("request encodes"),
    )
    .expect("request");
    let out = cli(
        fx,
        bin,
        &["__probe"],
        &[
            "--request",
            request.to_str().expect("utf-8"),
            "--output",
            output.to_str().expect("utf-8"),
        ],
    );
    if !out.status.success() {
        return Err(says(&out));
    }
    let evidence: Value =
        serde_json::from_slice(&std::fs::read(&output).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let row = &evidence["rows"][0];
    if !row["error"].is_null() {
        return Err(format!("the question errored: {row}"));
    }
    Ok(row["chunks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            (
                c["corpus_id"].as_str().unwrap_or_default().to_string(),
                c["content"].as_str().unwrap_or_default().trim().to_string(),
            )
        })
        .collect())
}

/// The probe's `epistemic` stage: the corpus the coverage verdict names as
/// nearest (the `epistemic_demo` example's measurement, pb-cli-llm-ingest-move).
fn probe_epistemic(fx: &Fixture, bin: &Path) -> Result<Option<String>, String> {
    let dir = fx.root.path().join(format!(
        "epistemic-{}",
        bin.file_name().and_then(|n| n.to_str()).unwrap_or("bin")
    ));
    std::fs::create_dir_all(&dir).expect("probe dir");
    let (request, output) = (dir.join("request.json"), dir.join("evidence.json"));
    std::fs::write(
        &request,
        serde_json::to_vec(&json!({
            "mode": "epistemic",
            "questions": [{ "id": "q1", "question": QUESTION }],
            "corpus": "",
            "limit": 0,
            "isolate": false,
        }))
        .expect("request encodes"),
    )
    .expect("request");
    let out = cli(
        fx,
        bin,
        &["__probe"],
        &[
            "--request",
            request.to_str().expect("utf-8"),
            "--output",
            output.to_str().expect("utf-8"),
        ],
    );
    if !out.status.success() {
        return Err(says(&out));
    }
    let evidence: Value =
        serde_json::from_slice(&std::fs::read(&output).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let row = &evidence["rows"][0];
    if !row["error"].is_null() || row["coverage"].is_null() {
        return Err(format!("the question has no coverage verdict: {row}"));
    }
    Ok(row["coverage"]["best_corpus"].as_str().map(str::to_string))
}

/// What the in-process engine retrieved for `QUESTION` at this row's start
/// (the bare binary built at f6684957d, before the switch), as
/// `(corpus_id, content)`: the paragraph chunker keeps the short fixture as
/// one chunk under its file's title.
fn expected() -> Vec<(String, String)> {
    vec![(CORPUS.to_string(), format!("source\n\n{ALPHA}\n\n{BETA}"))]
}

/// PROOF (pb-cli-llm-ingest-move-compose): the composed binary retrieves the
/// fixture's passage on `chat inspect` and on the probe's retrieve stage,
/// exactly as the in-process engine did; the bare binary names ingest
/// absent on both.
#[test]
fn the_composed_cli_retrieves_through_ingests_engine_and_the_bare_one_names_the_absence() {
    let bare = Path::new(COMPOSED).with_file_name("sovereign-cli-llm");
    assert!(
        bare.is_file(),
        "the bare sibling is built beside the composed one: {}",
        bare.display()
    );
    let fx = boot();
    let composed = Path::new(COMPOSED);

    assert_eq!(
        inspect(&fx, composed).unwrap_or_else(|e| panic!("chat inspect: {e}")),
        expected(),
        "`chat inspect` through the composed binary"
    );
    assert_eq!(
        probe_retrieve(&fx, composed).unwrap_or_else(|e| panic!("__probe retrieve: {e}")),
        expected(),
        "the probe's retrieve stage through the composed binary"
    );
    assert_eq!(
        probe_epistemic(&fx, composed).unwrap_or_else(|e| panic!("__probe epistemic: {e}")),
        Some(CORPUS.to_string()),
        "the probe's epistemic stage finds the fixture corpus nearest"
    );

    for (lane, got) in [
        ("chat inspect", inspect(&fx, &bare).map(|_| ())),
        ("__probe retrieve", probe_retrieve(&fx, &bare).map(|_| ())),
        ("__probe epistemic", probe_epistemic(&fx, &bare).map(|_| ())),
    ] {
        let said = got.expect_err("the bare binary reads no corpus");
        assert!(
            said.contains("no ingest program is composed in this process"),
            "{lane} on the bare binary names the absence:\n{said}"
        );
    }
}
