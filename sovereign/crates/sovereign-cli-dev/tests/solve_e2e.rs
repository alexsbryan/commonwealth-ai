// SPDX-License-Identifier: AGPL-3.0-or-later
//! The TDD solver served by `svrn code` alone (phase-b pb-meshapp-solve):
//! the BUILT `code mcp` binary with no svrn daemon, its solver's chat on a
//! fixture serve at a free `SOVEREIGN_SERVE_PORT`. `svrn solve` names the
//! code server's port with `--daemon`, because a deployed daemon may hold
//! :9741. The fixture repo's checker is a `counts:` command (PASS/FAIL
//! lines), so no test framework is needed, and the fixture serve answers
//! every candidate with the one edit that turns it green.

#![cfg(feature = "workbench")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Nothing listens on loopback port 2: a dial to svrn is refused at once.
const DEAD_DAEMON: &str = "http://127.0.0.1:2";

/// The one candidate edit: `add` rewritten to pass the checker.
const FIX: &str = "```json\n{\"action\": \"rewrite_function\", \"name\": \"add\"}\n```\n\n```python\ndef add(a, b):\n    return a + b\n```";

/// PASS once `add` returns the sum, FAIL before.
const CHECKER: &str =
    "counts: sh -c 'grep -q \"return a + b\" evaluator.py && echo PASS add || echo FAIL add'";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port()
}

/// A serve stand-in: its ready door and a chat route that answers [`FIX`],
/// counting the chat calls it took.
async fn fixture_serve(port: u16) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let router = axum::Router::new()
        .route(
            "/v1/models",
            axum::routing::get(|| async { axum::Json(json!({ "object": "list", "data": [] })) }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                let counted = Arc::clone(&counted);
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "choices": [{ "message": { "role": "assistant", "content": FIX } }],
                        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
                    }))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind the fixture serve");
    tokio::spawn(async move { axum::serve(listener, router).await });
    calls
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A committed repo whose `add` fails the checker.
fn fixture_repo(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).expect("repo dir");
    std::fs::write(repo.join("evaluator.py"), "def add(a, b):\n    return 0\n")
        .expect("fixture source");
    git(&repo, &["init", "-q", "--initial-branch=main"]);
    // The solver lands a reached run with a plain `git commit` in the
    // server's sandboxed HOME, so the identity lives in the repo.
    git(&repo, &["config", "user.name", "fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "fixture"]);
    repo
}

/// The code server child, killed on drop so a failed assert leaves no
/// listener.
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

/// `sovereign-cli-dev code mcp` on `port`, sandboxed under `root`, with its
/// solver's serve at `serve_port` and svrn at a dead port.
async fn start_code(root: &Path, port: u16, serve_port: u16) -> (Server, String) {
    let log = root.join("code.log");
    std::fs::create_dir_all(root.join("indexes")).expect("indexes dir");
    std::fs::create_dir_all(root.join("sovereign")).expect("sovereign dir");
    let child = Command::new(env!("CARGO_BIN_EXE_sovereign-cli-dev"))
        .args(["code", "mcp", "--port", &port.to_string(), "--data-dir"])
        .arg(root.join("indexes"))
        .arg("--sovereign-dir")
        .arg(root.join("sovereign"))
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("SVRNMESH_DATA_DIR", root.join("svrnmesh"))
        .env_remove("SOVEREIGN_DATA_DIR")
        .env("SVRNMESH_DAEMON_URL", DEAD_DAEMON)
        .env_remove("SOVEREIGN_DAEMON_URL")
        .env("SOVEREIGN_SERVE_PORT", serve_port.to_string())
        .env("RUST_LOG", "sovereign_code=debug,info")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).expect("code log"))
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

/// A tool call's JSON answer.
async fn call_tool(base: &str, id: u64, name: &str, arguments: Value) -> Value {
    let reply = rpc(
        base,
        id,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await;
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{name} answered no text: {reply}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{name} answered non-JSON ({e}): {text}"))
}

/// The row's proof: with no svrn daemon, `svrn solve` completes one round
/// against serve through standalone code, and the MCP `solve` tool on the
/// same server does the same.
#[tokio::test]
async fn code_alone_solves_one_round_against_serve() {
    let dir = tempfile::tempdir().expect("tempdir");
    let serve_port = free_port();
    let chat_calls = fixture_serve(serve_port).await;
    let (server, base) = start_code(dir.path(), free_port(), serve_port).await;

    // `svrn solve` (the dispatcher execs this sibling) against code's port.
    let repo = fixture_repo(dir.path(), "cli-repo");
    let out = tokio::task::spawn_blocking({
        let (repo, base) = (repo.clone(), base.clone());
        move || {
            Command::new(env!("CARGO_BIN_EXE_sovereign-cli-dev"))
                .args(["solve"])
                .arg(&repo)
                .args(["make add return the sum", "--watch", "--verb", "fix"])
                .args(["--test-command", CHECKER, "--daemon", &base])
                .env("SVRNMESH_DAEMON_URL", DEAD_DAEMON)
                .env_remove("SOVEREIGN_DAEMON_URL")
                .output()
                .expect("run sovereign-cli-dev solve")
        }
    })
    .await
    .expect("the solve verb ran");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "svrn solve failed ({:?}):\n{stdout}\n{}\n--- code log\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
        server.log()
    );
    assert!(
        stdout.contains("✓ reached — 1 passing / 0 failing after 1 round(s)"),
        "{stdout}"
    );
    let source = std::fs::read_to_string(repo.join("evaluator.py")).expect("fixture source");
    assert!(
        source.contains("return a + b"),
        "the round's edit did not land: {source}"
    );
    assert!(
        git(&repo, &["log", "-1", "--format=%s"]).starts_with("solve: make add return the sum"),
        "a reached run lands its commit"
    );
    let cli_calls = chat_calls.load(Ordering::SeqCst);
    assert!(cli_calls > 0, "the round never dialed serve");

    // The MCP `solve` tool on the same server.
    let list = rpc(&base, 1, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("a tool list")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for id in ["solve", "solve_status", "solve_cancel"] {
        assert!(names.contains(&id), "{id} is listed: {names:?}");
    }
    let repo = fixture_repo(dir.path(), "mcp-repo");
    let submitted = call_tool(
        &base,
        2,
        "solve",
        json!({
            "workdir": repo,
            "goal": "make add return the sum",
            "verb": "fix",
            "test_command": CHECKER,
        }),
    )
    .await;
    let job_id = submitted["job_id"]
        .as_str()
        .unwrap_or_else(|| panic!("solve gave no job: {submitted}"))
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        let status = call_tool(&base, 3, "solve_status", json!({ "job_id": job_id })).await;
        if status["state"] != "running" {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the MCP job never finished: {status}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(status["state"], "done", "{status}");
    assert_eq!(
        status["result"]["result"]["status"]["status"], "reached",
        "{status}"
    );
    assert_eq!(status["result"]["result"]["rounds"], 1, "{status}");
    assert!(
        std::fs::read_to_string(repo.join("evaluator.py"))
            .expect("fixture source")
            .contains("return a + b"),
        "the MCP round's edit did not land"
    );
    assert!(
        chat_calls.load(Ordering::SeqCst) > cli_calls,
        "the MCP round never dialed serve"
    );
    drop(server);
}
