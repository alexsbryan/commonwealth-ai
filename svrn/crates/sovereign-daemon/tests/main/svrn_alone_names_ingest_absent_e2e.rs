// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn alone composes no ingest, and says so by name (pb-ingest-dial-daemon).
//!
//! The `sovereign-daemon` binary (`process::run(.., None)`) links no
//! corpus-engine and builds no engine. It boots on a mock-engine serve and:
//!
//! - its boot log names the absence;
//! - `/mcp` lists none of ingest's tools (`wikipedia_fetch`, `sec_facts`,
//!   the corpus/atlas plane) and still serves its own (`solve`);
//! - the landscape digest, the watched-folder routes and the collaborate
//!   kickoff, and the recipe-project routes (pb-ingest-rehome-daemon),
//!   answer 503 naming the ingest program, never a 404.
//!
//! The same acts completing on the stock binary are sovereign-stock's
//! `ingest_composed_e2e`. Linux only, like its sibling process e2es.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sovereign_turn_client::reach::locate_sibling;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-daemon");

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

/// `SOVEREIGN_SERVE_BIN`, else `sovereign-serve` beside this binary. Absent
/// is a FAILURE naming the build, never a skip (five-programs-62).
fn serve_bin() -> PathBuf {
    if std::env::var_os("SOVEREIGN_SERVE_BIN").is_some() {
        return locate_sibling("sovereign-serve", "SOVEREIGN_SERVE_BIN")
            .expect("SOVEREIGN_SERVE_BIN is set but names no file");
    }
    let beside = Path::new(BIN).with_file_name("sovereign-serve");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p sovereign-serve`",
        beside.display()
    );
    beside
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

/// PROOF, absence half (pb-ingest-dial-daemon): svrn alone names the ingest
/// program wherever it would have needed it.
#[test]
fn svrn_alone_names_the_ingest_program_where_it_needs_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path();
    let (home, data, rails) = (p.join("home"), p.join("data"), p.join("rails"));
    for d in [&home, &data, &rails] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (client, internal, dead_rails, serve_port) =
        (free_port(), free_port(), free_port(), free_port());
    let config = data.join("config.toml");
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

    let serve_log = p.join("serve.log");
    let _serve = Killed(
        Command::new(serve_bin())
            .arg("--data-dir")
            .arg(&data)
            .args(["--listen", &format!("127.0.0.1:{serve_port}")])
            .env("HOME", &home)
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

    let log = p.join("svrn.log");
    let _svrn = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", p.join("svrnmesh"))
            .env("CW_RAILS_DIR", &rails)
            .env("CW_RAILS_BIN", rails.join("no-cw-rails"))
            .env("SOVEREIGN_SERVE_PORT", serve_port.to_string())
            .env_remove("SOVEREIGN_WORKSPACE_DIR")
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-daemon"),
    );
    wait_until(
        "the client port answered /v1/models",
        Duration::from_secs(120),
        &log,
        || request(client, "GET", "/v1/models", None).is_some_and(|(s, _)| s == 200),
    );

    let boot = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        boot.contains("no ingest program in this process"),
        "svrn alone's boot names the absence:\n{boot}"
    );

    // /mcp: svrn's own tools, none of ingest's.
    let (_, listed) = request(
        client,
        "POST",
        "/mcp",
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#),
    )
    .expect("/mcp answered");
    // svrn's list still answers. With no ingest it is empty: `solve`, the
    // one tool it held beside ingest's, is code's since pb-meshapp-solve.
    assert!(
        listed.contains("\"tools\":["),
        "svrn's own tools still list: {listed}"
    );
    assert!(
        !listed.contains("\"solve\""),
        "the solver is code's, not svrn's: {listed}"
    );
    for tool in [
        "wikipedia_fetch",
        "sec_facts",
        "parcel_analytics",
        "corpus_search",
    ] {
        assert!(
            !listed.contains(&format!("\"{tool}\"")),
            "svrn alone lists ingest's `{tool}`: {listed}"
        );
    }

    // The routes that need ingest answer 503 naming it. The recipe-project
    // store is ingest's since pb-ingest-rehome-daemon.
    for (port, method, path, body) in [
        (client, "POST", "/v1/knowledge/landscape_digest", "{}"),
        (client, "GET", "/internal/corpus/local", ""),
        (client, "GET", "/v1/features/projects", ""),
        (client, "GET", "/v1/recipe-projects", ""),
        (
            internal,
            "POST",
            "/internal/corpus/collaborate",
            r#"{"corpus_id":"sep"}"#,
        ),
    ] {
        let (status, text) = request(port, method, path, Some(body))
            .unwrap_or_else(|| panic!("nothing answered {method} {path}"));
        assert_eq!(status, 503, "{method} {path}: {text}");
        assert!(
            text.contains("ingest program"),
            "{method} {path} names the ingest program: {text}"
        );
    }
}
