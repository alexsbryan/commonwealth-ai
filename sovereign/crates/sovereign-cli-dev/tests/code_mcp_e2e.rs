// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code mcp` run as the BUILT binary (phase-b pb-code-server): code
//! intelligence alone, with no daemon, no model and no cw-rails. The fixture
//! is a temp git repo, a SCIP graph with one call edge, and a data root whose
//! config points the rails base and the daemon URL at dead loopback ports, so
//! nothing here reaches a deployed daemon or the operator's cw-rails.

#![cfg(feature = "workbench")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use corpus_engine_scip::scip_graph::{ScipGraph, ScipRefRecord, ScipSymbolRecord};
use serde_json::{json, Value};
use sovereign_contracts::setup_config::SetupConfig;

/// Nothing listens on loopback ports 1 or 2: a dial is refused at once.
const DEAD_RAILS: &str = "http://127.0.0.1:1";
const DEAD_DAEMON: &str = "http://127.0.0.1:2";

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    /// A git repo with one source file, a SCIP graph where `fixture_caller`
    /// calls `fixture_target`, and a data root naming dead doors.
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".sovereign")).expect("repo dir");
        std::fs::create_dir_all(repo.join("src")).expect("src dir");
        let source = repo.join("src/lib.rs");
        std::fs::write(
            &source,
            "pub fn fixture_target() {}\n\npub fn fixture_caller() {\n    fixture_target();\n}\n",
        )
        .expect("fixture source");
        // Committed, with the server's own `.sovereign/` ignored, so the
        // tree is clean and the Reindexer resumes the project without a
        // startup rebuild (its graph is stamped at HEAD below).
        std::fs::write(repo.join(".gitignore"), ".sovereign/\n").expect("fixture .gitignore");
        for args in [
            &["init", "-q"][..],
            &["add", "-A"][..],
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "-m",
                "fixture",
            ][..],
        ] {
            let git = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .expect("run git");
            assert!(git.success(), "git {args:?}");
        }
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("git rev-parse");
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();

        let root = dir.path().join("root");
        let mut config = SetupConfig::unconfigured();
        config.node.entry = Some(DEAD_DAEMON.to_string());
        config.daemon.rails_base = Some(DEAD_RAILS.to_string());
        let path = SetupConfig::path_in(&root);
        config.save_to(&path).expect("write the fixture config");
        let loaded = SetupConfig::load_from(&path).expect("the fixture config loads");
        assert_eq!(loaded.daemon.rails_base.as_deref(), Some(DEAD_RAILS));

        let corpus = dir.path().join("indexes/fixture");
        std::fs::create_dir_all(&corpus).expect("corpus dir");
        let graph = ScipGraph::open(&corpus.join("scip_graph.db"), "fixture").expect("scip graph");
        let file = source.display().to_string();
        let symbol = |name: &str, line: i32| ScipSymbolRecord {
            name: name.to_string(),
            qualified_name: format!("fixture src/lib.rs/{name}()."),
            kind: "function".to_string(),
            file_path: file.clone(),
            line_start: line,
            line_end: line + 1,
            language: "rust".to_string(),
        };
        graph
            .ingest_symbols_and_refs(
                vec![symbol("fixture_target", 0), symbol("fixture_caller", 2)],
                vec![ScipRefRecord {
                    caller_symbol: "fixture_caller".to_string(),
                    callee_symbol: "fixture_target".to_string(),
                    caller_qualified: "fixture src/lib.rs/fixture_caller().".to_string(),
                    callee_qualified: "fixture src/lib.rs/fixture_target().".to_string(),
                    file_path: file,
                    line: 3,
                    start_col: 4,
                    end_line: 3,
                    end_col: 18,
                    ref_kind: "call".to_string(),
                }],
            )
            .await
            .expect("ingest the fixture graph");
        graph.record_rebuild("fixture", Some(&head), None).await;

        // The project the code server's Reindexer resumes at startup.
        let mut registry = sovereign_contracts::watcher_projects::Registry::default();
        registry.upsert(sovereign_contracts::watcher_projects::ProjectEntry::new(
            "fixture", &repo,
        ));
        registry
            .save_to(&root.join("projects.json"))
            .expect("write the fixture project registry");
        Self { dir }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    /// `sovereign-cli-dev code mcp` on `port`, sandboxed to this fixture.
    fn command(&self, port: u16) -> Command {
        let home: &Path = self.dir.path();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sovereign-cli-dev"));
        cmd.args(["code", "mcp", "--port", &port.to_string(), "--data-dir"])
            .arg(self.path("indexes"))
            .arg("--sovereign-dir")
            .arg(self.path("repo/.sovereign"))
            .current_dir(self.path("repo"))
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("SVRNMESH_DATA_DIR", self.path("root"))
            .env_remove("SOVEREIGN_DATA_DIR")
            .env("SVRNMESH_DAEMON_URL", DEAD_DAEMON)
            .env_remove("SOVEREIGN_DAEMON_URL");
        cmd
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port()
}

fn text(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stderr\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The server child, killed on drop so a failed assert leaves no listener.
struct Server {
    child: Child,
    log: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// Spawn the server and wait for `/mcp/stats`, the probe `svrn serve
/// --background` uses. A server that exits first fails with its stderr.
async fn start(fx: &Fixture, port: u16) -> (Server, String) {
    let log = fx.path("server.log");
    let stderr = std::fs::File::create(&log).expect("server log");
    let child = fx
        .command(port)
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .expect("spawn sovereign-cli-dev code mcp");
    let mut server = Server { child, log };
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(Some(status)) = server.child.try_wait() {
            panic!("code mcp exited {status} before serving:\n{}", server.log());
        }
        if let Ok(r) = client.get(format!("{base}/mcp/stats")).send().await {
            if r.status().is_success() {
                return (server, base);
            }
        }
        assert!(
            Instant::now() < deadline,
            "code mcp never served:\n{}",
            server.log()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn rpc(base: &str, id: u64, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
        .send()
        .await
        .expect("post /mcp")
        .json()
        .await
        .expect("a JSON-RPC reply")
}

/// The row's proof: with no daemon, no model and no cw-rails, `symbols` and
/// `callers` answer from the fixture graph, and `declare_scope` answers the
/// named absence of cw-rails' KV door, naming the door it dialed and the
/// verb that brings it up.
#[tokio::test]
async fn code_mcp_answers_alone_and_names_the_absent_rails() {
    let fx = Fixture::new().await;
    let (server, base) = start(&fx, free_port()).await;

    let init = rpc(&base, 1, "initialize", json!({})).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "sovereign-code");
    assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], true);

    let list = rpc(&base, 2, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("a tool list")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for id in ["symbols", "callers", "declare_scope", "work_in_flight"] {
        assert!(names.contains(&id), "{id} is listed: {names:?}");
    }

    let symbols = rpc(
        &base,
        3,
        "tools/call",
        json!({ "name": "symbols", "arguments": { "name": "fixture_target" } }),
    )
    .await;
    let symbols_text = symbols["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert_eq!(symbols["result"]["isError"], false, "{symbols}");
    assert!(
        symbols_text.contains("src/lib.rs"),
        "symbols names the definition's file: {symbols_text}"
    );

    let callers = rpc(
        &base,
        4,
        "tools/call",
        json!({ "name": "callers", "arguments": { "symbol": "fixture_target" } }),
    )
    .await;
    let callers_text = callers["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert_eq!(callers["result"]["isError"], false, "{callers}");
    assert!(
        callers_text.contains("fixture_caller"),
        "callers finds the fixture edge: {callers_text}"
    );

    let claim = rpc(
        &base,
        5,
        "tools/call",
        json!({ "name": "declare_scope",
                "arguments": { "symbols": ["fixture_target"], "intent": "e2e" } }),
    )
    .await;
    let claim_text = claim["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert_eq!(claim["result"]["isError"], true, "{claim}");
    assert!(
        claim_text.contains(&format!("{DEAD_RAILS}/v1/mesh/kv"))
            && claim_text.contains("svrn mesh up"),
        "the claim names the rails door it dialed and how to bring it up: {claim_text}\n{}",
        server.log()
    );
}

/// `:9741/mcp` is one address: the second process to bind a port refuses by
/// name, naming the address, and serves nothing (principle 6). Neither
/// stops the other.
#[tokio::test]
async fn a_second_binder_refuses_naming_the_port() {
    let fx = Fixture::new().await;
    let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let port = holder.local_addr().expect("held addr").port();
    let out = fx.command(port).output().expect("run code mcp");
    assert!(
        !out.status.success(),
        "the second binder refuses: {}",
        text(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&format!("127.0.0.1:{port}")),
        "the refusal names the address: {}",
        text(&out)
    );
    drop(holder);
}

/// A notes store that cannot open is refused by path, never replaced by an
/// in-memory one whose writes vanish at exit.
#[tokio::test]
async fn an_unopenable_notes_store_is_refused_by_path() {
    let fx = Fixture::new().await;
    // Code's one notes store is the data root's (pb-notes-verbs), not the repo's.
    let notes = fx.path("root/notes.db");
    std::fs::create_dir_all(&notes).expect("a directory where notes.db goes");
    let out = fx.command(free_port()).output().expect("run code mcp");
    assert!(!out.status.success(), "refused: {}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&notes.display().to_string()),
        "the refusal names the path: {}",
        text(&out)
    );
}

/// Text of a `symbols` answer for `name`.
async fn symbols_text(base: &str, id: u64, name: &str) -> String {
    let out = rpc(
        base,
        id,
        "tools/call",
        json!({ "name": "symbols", "arguments": { "name": name } }),
    )
    .await;
    out["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// The row's proof (phase-b pb-code-freshness): with no daemon, an edit to a
/// source file shows in `symbols` through the code server's own Reindexer,
/// well inside the 30 s the deleted poll took, and before any tool had read
/// the graph (the Reindexer loads the deferred graph before it writes).
#[tokio::test]
async fn an_edit_shows_in_symbols_through_the_reindexer() {
    let fx = Fixture::new().await;
    let (server, base) = start(&fx, free_port()).await;

    let source = fx.path("repo/src/lib.rs");
    let mut text = std::fs::read_to_string(&source).expect("fixture source");
    text.push_str("\npub fn fixture_added_by_edit() {}\n");
    std::fs::write(&source, text).expect("edit the fixture source");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut id = 10;
    loop {
        let answer = symbols_text(&base, id, "fixture_added_by_edit").await;
        if answer.contains("src/lib.rs") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the edit never reached `symbols` through the Reindexer: {answer}\n{}",
            server.log()
        );
        id += 1;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        symbols_text(&base, id + 1, "fixture_target")
            .await
            .contains("src/lib.rs"),
        "the graph loaded from disk still answers"
    );
}

/// Moved from sovereign-daemon's spec_gate_e2e with the SpecWatcher
/// (pb-code-freshness): writing `.sovereign/features/*/spec.md` reaches a
/// connected client as `notifications/tools/list_changed` over `GET /mcp`.
/// The gate's render (spec/drift in or out) is proven beside it in
/// `sovereign_code::mcp_surface`'s tests.
#[tokio::test]
async fn a_spec_write_pushes_tools_list_changed_over_sse() {
    use futures::StreamExt;

    let fx = Fixture::new().await;
    let (server, base) = start(&fx, free_port()).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/mcp"))
        .send()
        .await
        .expect("GET /mcp");
    assert!(resp.status().is_success());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(Ok(bytes)) = stream.next().await {
            buf.extend_from_slice(&bytes);
            while let Some(i) = buf.windows(2).position(|w| w == b"\n\n") {
                let event: Vec<u8> = buf.drain(..i + 2).collect();
                for line in String::from_utf8_lossy(&event).lines() {
                    if let Some(data) = line.strip_prefix("data:") {
                        let _ = tx.send(data.trim().to_string());
                    }
                }
            }
        }
    });
    let endpoint = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the endpoint event")
        .expect("an event");
    assert_eq!(endpoint, "/mcp");

    let feature = fx.path("repo/.sovereign/features/foo");
    std::fs::create_dir_all(&feature).expect("feature dir");
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(feature.join("spec.md"), "# foo\n").expect("write the spec");

    let notif = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("no tools/list_changed within 10 s:\n{}", server.log()))
        .expect("an event");
    let parsed: Value = serde_json::from_str(&notif).expect("a JSON-RPC frame");
    assert_eq!(parsed["method"], "notifications/tools/list_changed");
}
