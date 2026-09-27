// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn starts no other process (pb-rails-untether, phase-b-31): booted with
//! a REAL cw-rails binary on `CW_RAILS_BIN` and nothing answering on its
//! `rails_base`, it answers a chat turn through the serve it dials, and no
//! cw-rails runs and no `rails.lock` exists afterwards; a ring read names the
//! absence and the verb that ends it. cw-rails is brought
//! up by `svrn mesh up` alone (sovereign-cli-mesh tests/rails_up_e2e.rs).
//!
//! The file keeps its name so nextest's `daemon-boot` group
//! (.config/nextest.toml) still serializes its boot. Linux only: the holder
//! count reads `/proc/*/fd`, the one way to ask "who holds this lock" that
//! needs no tool on the host.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sovereign_turn_client::reach::locate_sibling;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-daemon");

/// `env`, else `name` beside the sovereign-daemon this test was built with.
/// Absent is a FAILURE naming the build, never a skip (five-programs-62).
fn sibling(name: &str, env: &str, package: &str) -> PathBuf {
    if std::env::var_os(env).is_some() {
        return locate_sibling(name, env)
            .unwrap_or_else(|| panic!("{env} is set but names no file"));
    }
    let beside = Path::new(BIN).with_file_name(name);
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

/// Kills every cw-rails holding this root's lock on every exit path, so a
/// regression that brings one up leaks nothing past the test.
struct Reaper(PathBuf);

impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in holders(&self.0) {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
    }
}

/// The pids with an open fd on `lock`.
fn holders(lock: &Path) -> Vec<u32> {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    procs
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_dir(format!("/proc/{pid}/fd"))
                .map(|fds| {
                    fds.filter_map(Result::ok)
                        .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == lock))
                })
                .unwrap_or(false)
        })
        .collect()
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
    s.set_read_timeout(Some(Duration::from_secs(20))).ok()?;
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

/// Poll `done`; on timeout, panic with the tail of `see` — the tempdir is
/// gone by the time anyone reads the failure.
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

/// PROOF (pb-rails-untether): svrn boots, dials the serve it is pointed at,
/// answers a chat turn, and starts no cw-rails, although a real one is on
/// `CW_RAILS_BIN` and nothing answers on `rails_base`. PLANT: put
/// `ensure_rails` back in the boot and a cw-rails holds `rails.lock`.
#[test]
fn svrn_boots_and_answers_a_turn_and_starts_no_cw_rails() {
    let rails_bin = sibling("cw-rails", "CW_RAILS_BIN", "commonwealth-rails");
    let serve_bin = sibling("sovereign-serve", "SOVEREIGN_SERVE_BIN", "sovereign-serve");
    let root = tempfile::tempdir().expect("tempdir");
    let (home, data, rails_dir) = (
        root.path().join("home"),
        root.path().join("data"),
        root.path().join("rails"),
    );
    for d in [&home, &data, &rails_dir] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (client, internal, rails_port, serve_port) =
        (free_port(), free_port(), free_port(), free_port());
    let lock = rails_dir.join("rails.lock");
    let _reaper = Reaper(lock.clone());
    // The mock engine loads no weights (scripts/program-lift.toml, svrn's smoke).
    let config = data.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[engine]\nkind = \"mock\"\n\n[models]\nprimary = \"{r}/mock.gguf\"\n\
             embed = \"{r}/mock-embed.gguf\"\n\n[daemon]\nclient_port = {client}\n\
             internal_port = {internal}\nrails_base = \"http://127.0.0.1:{rails_port}\"\n\n\
             [data]\ndir = \"{d}\"\n",
            r = data.display(),
            d = data.display()
        ),
    )
    .expect("config");
    let serve_log = root.path().join("serve.log");
    let _serve = Killed(
        Command::new(&serve_bin)
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

    let stderr = root.path().join("daemon.stderr.log");
    let _daemon = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", root.path().join("svrnmesh"))
            .env("CW_RAILS_DIR", &rails_dir)
            .env("CW_RAILS_BIN", &rails_bin)
            .env("SOVEREIGN_SERVE_PORT", serve_port.to_string())
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&stderr).expect("stderr")))
            .spawn()
            .expect("spawn sovereign-daemon"),
    );
    wait_until(
        "the daemon answered 200 on /v1/models",
        Duration::from_secs(120),
        &stderr,
        || request(client, "GET", "/v1/models", None).is_some_and(|(s, _)| s == 200),
    );

    let turn = r#"{"messages":[{"role":"user","content":"hello"}]}"#;
    let (code, answer) = request(client, "POST", "/v1/chat/completions", Some(turn))
        .expect("the daemon answers a chat turn");
    let answer: serde_json::Value = serde_json::from_str(&answer)
        .unwrap_or_else(|e| panic!("HTTP {code}, unreadable turn ({e}): {answer}"));
    assert!(
        answer["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "HTTP {code}: the turn carried no answer: {answer}"
    );

    // A ring read with cw-rails absent: a named absence naming the verb that
    // brings it up, never an empty journal (principle 6).
    let (code, body) = request(client, "GET", "/v1/rail/log?namespace=notes", None)
        .expect("the daemon answers a ring read");
    assert!(
        code >= 500 && body.contains("not reachable") && body.contains("`svrn mesh up`"),
        "HTTP {code}: a ring read names cw-rails' absence and `svrn mesh up`: {body}"
    );

    assert!(
        holders(&lock).is_empty() && !lock.exists(),
        "svrn's boot started a cw-rails ({} exists, holders {:?})",
        lock.display(),
        holders(&lock)
    );
    assert!(
        request(rails_port, "GET", "/v1/mesh/status", None).is_none(),
        "something answers on rails_base :{rails_port}; svrn brings nothing up"
    );
}
