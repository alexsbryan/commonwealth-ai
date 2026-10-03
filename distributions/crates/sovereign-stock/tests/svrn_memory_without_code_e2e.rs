// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn keeps its memory with NO code program (pb-notes-memory, the "svrn
//! without code" developer; phase-b-30 F4 (a)).
//!
//! svrn alone (the `sovereign-daemon` binary, which composes no code) boots
//! on a mock-engine serve and answers a chat turn; a lesson is written
//! through the desktop's `/v1/notes`; the daemon is killed and booted again,
//! and the lesson is still served and is the one the turn path's reader
//! (`load_active_lessons`) picks up from svrn's store. svrn never created the
//! code program's `notes.db`. Then the stock binary boots on the same root,
//! code composed beside svrn, and code's own `notes` tool does not see the
//! lesson while svrn's route still does.
//!
//! Linux only, like its sibling process e2es.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sovereign_turn_client::reach::locate_sibling;

/// This package's binary, beside which a stock install puts the others.
const STOCK: &str = env!("CARGO_BIN_EXE_sovereign-stock");

/// What the lesson says; searched for in code's answer.
const LESSON: &str = "Answer the quarterly questions in one sentence.";

/// svrn alone, the binary under test. The test is the distribution's because
/// it boots two programs' binaries side by side (moved from sovereign-daemon by
/// pb-distribution-svrn-lift-2: a lifted svrn has no serve or stock binary).
/// Absent is a FAILURE naming the build, never a skip (five-programs-62).
fn svrn() -> PathBuf {
    let bin = Path::new(STOCK).with_file_name("sovereign-daemon");
    assert!(
        bin.is_file(),
        "{} is missing: build it with `cargo build -p sovereign-daemon --bin sovereign-daemon --features treesitter`",
        bin.display()
    );
    bin
}

/// `env`, else `name` beside this package's binary.
/// Absent is a FAILURE naming the build, never a skip (five-programs-62).
fn sibling(name: &str, env: &str, package: &str) -> PathBuf {
    if std::env::var_os(env).is_some() {
        return locate_sibling(name, env)
            .unwrap_or_else(|| panic!("{env} is set but names no file"));
    }
    let beside = Path::new(STOCK).with_file_name(name);
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p {package}`, or set {env}",
        beside.display()
    );
    beside
}

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

/// One loopback request's status and body, or `None` while nothing listens.
fn request(port: u16, method: &str, path: &str, json: Option<&str>) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    let body = json.unwrap_or("");
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let status = raw.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, raw.split_once("\r\n\r\n")?.1.to_string()))
}

fn json(port: u16, method: &str, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    let (code, text) = request(port, method, path, Some(&body.to_string()))
        .unwrap_or_else(|| panic!("nothing answered {method} {path} on :{port}"));
    let value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{method} {path}: HTTP {code}, not JSON ({e}): {text}"));
    (code, value)
}

/// Poll `done`; on timeout, panic with the tail of `see`.
fn wait_until(what: &str, within: Duration, see: &Path, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        if Instant::now() >= deadline {
            let text = std::fs::read_to_string(see).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(30).collect();
            panic!(
                "never saw: {what}, within {within:?}; last lines of {}:\n{}",
                see.display(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

struct Root {
    _dir: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
    rails: PathBuf,
    svrnmesh: PathBuf,
    config: PathBuf,
    logs: PathBuf,
    client: u16,
}

fn root() -> Root {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().to_path_buf();
    let (home, data, rails, logs) = (
        p.join("home"),
        p.join("data"),
        p.join("rails"),
        p.join("logs"),
    );
    for d in [&home, &data, &rails, &logs] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (client, internal, dead_rails) = (free_port(), free_port(), free_port());
    let config = data.join("config.toml");
    // The mock engine loads no weights; cw-rails is a closed port.
    std::fs::write(
        &config,
        format!(
            "[engine]\nkind = \"mock\"\n\n[models]\nprimary = \"{d}/mock.gguf\"\n\
             embed = \"{d}/mock-embed.gguf\"\n\n[daemon]\nclient_port = {client}\n\
             internal_port = {internal}\nrails_base = \"http://127.0.0.1:{dead_rails}\"\n\n\
             [data]\ndir = \"{d}\"\n",
            d = data.display()
        ),
    )
    .expect("config");
    Root {
        svrnmesh: p.join("svrnmesh"),
        _dir: dir,
        home,
        data,
        rails,
        config,
        logs,
        client,
    }
}

/// Boot `bin` (`run --config`) on `root`, dialing serve at `serve_port`, and
/// wait until its client port answers.
fn boot(bin: &Path, root: &Root, serve_port: u16, log: &str) -> Killed {
    let log = root.logs.join(log);
    let child = Killed(
        Command::new(bin)
            .args(["run", "--config"])
            .arg(&root.config)
            .env("HOME", &root.home)
            .env("SVRNMESH_DATA_DIR", &root.svrnmesh)
            .env("CW_RAILS_DIR", &root.rails)
            .env("CW_RAILS_BIN", root.rails.join("no-cw-rails"))
            .env("SOVEREIGN_SERVE_PORT", serve_port.to_string())
            .env_remove("SOVEREIGN_WORKSPACE_DIR")
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display())),
    );
    wait_until(
        "the client port answered /v1/models",
        Duration::from_secs(120),
        &log,
        || request(root.client, "GET", "/v1/models", None).is_some_and(|(s, _)| s == 200),
    );
    child
}

fn lesson_ids(port: u16) -> Vec<String> {
    let (code, listed) = json(
        port,
        "POST",
        "/v1/notes/query",
        serde_json::json!({ "kinds": ["lesson"], "include_retired": true }),
    );
    assert_eq!(code, 200, "the lesson list: {listed}");
    listed["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("no notes envelope: {listed}"))
        .iter()
        .filter_map(|n| n["id"].as_str().map(str::to_string))
        .collect()
}

fn mcp_call(port: u16, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
    let (_, reply) = json(
        port,
        "POST",
        "/mcp",
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                            "params": { "name": tool, "arguments": arguments } }),
    );
    reply
}

/// PROOF (pb-notes-memory): a lesson svrn writes with no code program is
/// kept across a restart in svrn's own store, and code's `notes` tool does
/// not see it.
#[test]
fn svrn_keeps_a_lesson_with_no_code_program_and_codes_notes_do_not_see_it() {
    let serve_bin = sibling("sovereign-serve", "SOVEREIGN_SERVE_BIN", "sovereign-serve");
    let stock_bin = sibling("sovereign-stock", "SOVEREIGN_STOCK_BIN", "sovereign-stock");
    let r = root();

    let serve_port = free_port();
    let serve_log = r.logs.join("serve.log");
    let serve = Killed(
        Command::new(&serve_bin)
            .arg("--data-dir")
            .arg(&r.data)
            .args(["--listen", &format!("127.0.0.1:{serve_port}")])
            .env("HOME", &r.home)
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&serve_log).expect("log")))
            .spawn()
            .expect("spawn sovereign-serve"),
    );
    wait_until(
        "serve answered /health",
        Duration::from_secs(60),
        &serve_log,
        || request(serve_port, "GET", "/health", None).is_some_and(|(s, _)| s == 200),
    );

    // svrn alone: it chats, and it names the code program for code's tools.
    let daemon = boot(&svrn(), &r, serve_port, "svrn-1.log");
    let (code, turn) = json(
        r.client,
        "POST",
        "/v1/chat/completions",
        serde_json::json!({ "messages": [{ "role": "user", "content": "hello" }] }),
    );
    assert!(
        turn["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "HTTP {code}: svrn alone answered no chat turn: {turn}"
    );
    let absent = mcp_call(r.client, "notes", serde_json::json!({}));
    assert!(
        absent["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("svrn code mcp")),
        "svrn alone serves no code tool and names the code server: {absent}"
    );

    // A lesson, the way the desktop's lesson pane writes one.
    let payload = serde_json::json!({
        "display": LESSON, "prompt_form": LESSON, "enforcement": "prompt", "enabled": true,
    })
    .to_string();
    let (code, created) = json(
        r.client,
        "POST",
        "/v1/notes",
        serde_json::json!({ "kind": "lesson", "content": LESSON, "session_id": "conv-1",
                            "scope": "global", "source": "agent", "payload_json": payload }),
    );
    assert_eq!(code, 201, "the lesson was not written: {created}");
    let id = created["id"].as_str().expect("the store's id").to_string();

    // Restart: the lesson is still svrn's.
    drop(daemon);
    wait_until(
        "svrn's port closed",
        Duration::from_secs(20),
        &serve_log,
        || request(r.client, "GET", "/v1/models", None).is_none(),
    );
    let daemon = boot(&svrn(), &r, serve_port, "svrn-2.log");
    assert!(
        lesson_ids(r.client).contains(&id),
        "the lesson did not survive svrn's restart"
    );
    drop(daemon);
    drop(serve);

    // The turn path's reader recalls it from svrn's store, and svrn never
    // made the code program's store.
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let recalled = rt.block_on(async {
        let store = sovereign_store::sqlite::SqliteStateStore::open(&r.data.join("sovereign.db"))
            .expect("svrn's store");
        let set = sovereign_core::lessons::load_active_lessons(Some(
            &store as &dyn sovereign_contracts::notes::AgentNotes,
        ))
        .await;
        set.prompt.map(|l| l.note_id)
    });
    assert_eq!(
        recalled.as_deref(),
        Some(id.as_str()),
        "the turn's lesson reader"
    );
    assert!(
        !r.data.join("notes.db").exists(),
        "svrn alone created the code program's notes.db"
    );

    // Code composed beside svrn (the stock binary, its own serve): code's
    // notes tool does not see svrn's lesson; svrn's route still does.
    let stock = boot(&stock_bin, &r, free_port(), "stock.log");
    let seen = mcp_call(
        r.client,
        "notes",
        serde_json::json!({ "kinds": ["lesson"] }),
    );
    assert!(
        seen.get("error").is_none(),
        "code's notes tool did not answer: {seen}"
    );
    let text = seen["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !text.contains(&id) && !text.contains(LESSON),
        "code's notes tool sees svrn's lesson: {text}"
    );
    assert!(
        r.data.join("notes.db").exists(),
        "code, composed, opened no notes.db — the tool answered from somewhere else"
    );
    assert!(
        lesson_ids(r.client).contains(&id),
        "svrn lost the lesson beside code"
    );
    drop(stock);
}
