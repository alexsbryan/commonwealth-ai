// SPDX-License-Identifier: AGPL-3.0-or-later
//! Standalone state, against the REAL cw-rails binary (fp-solo-clients,
//! five-programs-63/-65).
//!
//! On a meshless node the daemon's boot brings cw-rails up solo through
//! `rails_client::ensure_rails`, and `/v1/models` answers 200. A row written
//! the way `svrn portfolio` writes (cli-llm's `rails_kv()` is `ensure_rails`
//! then `RailsKv::new` — the two calls below, since that fn is crate-private
//! one crate up) survives killing cw-rails, because the next `rails_kv()`
//! re-ensures it. Two racing `ensure_rails` calls both return Ok with ONE
//! cw-rails on `rails.lock`, and the loser's refusal is in `rails.log`.
//! With no rails.toml, the brought-up cw-rails serves the port the daemon's
//! `rails_base` names, and a local-only daemon's runs with n0 services off
//! (five-programs-66).
//!
//! Its own test binary, not a `tests/main/` module: it sets `CW_RAILS_DIR`
//! and `CW_RAILS_BIN` in this process, so `ensure_rails` here resolves the
//! temp root rather than the developer's `~/.commonwealth-rails`. Linux only:
//! the holder count reads `/proc/*/fd`, the one way to ask "who holds this
//! lock" that needs no tool on the host.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bytes::Bytes;
use kernel_types::NodeId;
use sovereign_contracts::peer::ReplicatedKv;
use sovereign_daemon::rails_client::{ensure_rails, kv::RailsKv};
use sovereign_turn_client::reach::locate_sibling;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-daemon");

/// `CW_RAILS_BIN`, else `cw-rails` beside the sovereign-daemon this test
/// was built with. Absent is a FAILURE naming the build, never a skip
/// (five-programs-62).
fn cw_rails_bin() -> PathBuf {
    if std::env::var_os("CW_RAILS_BIN").is_some() {
        return locate_sibling("cw-rails", "CW_RAILS_BIN")
            .expect("CW_RAILS_BIN is set but names no file");
    }
    let beside = Path::new(BIN).with_file_name("cw-rails");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p commonwealth-rails`, or set CW_RAILS_BIN",
        beside.display()
    );
    beside
}

/// Kills and reaps the daemon on every exit path, panics included.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Kills every cw-rails holding this root's lock on every exit path. The
/// daemon and `ensure_rails` drop their handles by design, so the lock is
/// the one thing that names them.
struct Reaper(PathBuf);

impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in holders(&self.0) {
            kill(pid);
        }
    }
}

fn kill(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
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

/// One loopback GET's status, or `None` while nothing listens.
fn status(port: u16, path: &str) -> Option<u16> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    raw.split_whitespace().nth(1)?.parse().ok()
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

/// Kill the root's cw-rails and wait until its port refuses.
fn kill_rails(lock: &Path, rails_port: u16, log: &Path) {
    let pids = holders(lock);
    assert_eq!(
        pids.len(),
        1,
        "one cw-rails holds {}: {pids:?}",
        lock.display()
    );
    kill(pids[0]);
    wait_until(
        "the killed cw-rails let go",
        Duration::from_secs(10),
        log,
        || holders(lock).is_empty() && status(rails_port, "/v1/mesh/status").is_none(),
    );
}

#[test]
fn a_meshless_node_brings_cw_rails_up_and_its_state_survives_a_restart() {
    let rails_bin = cw_rails_bin();
    let root = tempfile::tempdir().expect("tempdir");
    let (home, data, rails_dir) = (
        root.path().join("home"),
        root.path().join("data"),
        root.path().join("rails"),
    );
    for d in [&home, &data, &rails_dir] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (client, internal, rails_port) = (free_port(), free_port(), free_port());
    let base = format!("http://127.0.0.1:{rails_port}");
    // Meshless: rails.toml names the port and nothing else; no mesh.json.
    std::fs::write(
        rails_dir.join("rails.toml"),
        format!("listen = {rails_port}\n"),
    )
    .expect("rails.toml");
    let (lock, log) = (rails_dir.join("rails.lock"), rails_dir.join("rails.log"));
    let _reaper = Reaper(lock.clone());
    // This process's ensure_rails resolves the same root and binary.
    std::env::set_var("CW_RAILS_DIR", &rails_dir);
    std::env::set_var("CW_RAILS_BIN", &rails_bin);
    // The pump's append is the durability point the test waits on below.
    std::env::set_var("RUST_LOG", "info,rails=debug");

    // A terminal-class node (no models) — five-programs-62's fixture. A
    // local-only process would need a loadable model, so the 503 that profile
    // shows without cw-rails stays proven in-process (local_only_boot.rs);
    // here the bring-up is proven by the lock's holder and the store below.
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[node]\nentry_node = \"00000000000000000000000000000001\"\n\n\
             [daemon]\nclient_port = {client}\ninternal_port = {internal}\n\
             rails_base = \"{base}\"\n\n[data]\ndir = \"{}\"\n",
            data.display()
        ),
    )
    .expect("config");
    let stderr = root.path().join("daemon.stderr.log");
    let _daemon = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", root.path().join("svrnmesh"))
            .env("CW_RAILS_DIR", &rails_dir)
            .env("CW_RAILS_BIN", &rails_bin)
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&stderr).expect("stderr")))
            .spawn()
            .expect("spawn sovereign-daemon"),
    );

    // Boot brings cw-rails up solo, and the model list is its to serve.
    wait_until(
        "the daemon answered 200 on /v1/models",
        Duration::from_secs(60),
        &stderr,
        || status(client, "/v1/models") == Some(200),
    );
    assert_eq!(holders(&lock).len(), 1, "boot brought up ONE cw-rails");

    // A portfolio row, written the way `svrn portfolio` writes it.
    let rails_kv = || {
        ensure_rails(&base, false).expect("rails_kv() re-ensures cw-rails");
        RailsKv::new(base.clone())
    };
    let (app, key, value) = ("portfolio-private", "solo-e2e", Bytes::from_static(b"kept"));
    rails_kv()
        .set(app, key, value.clone(), NodeId::from_u128(7))
        .expect("the row is written");

    // A write is durable once cw-rails' pump appends it to the journal, on
    // its next tick (PUMP_INTERVAL, 2 s); a kill inside that window loses it.
    wait_until(
        "the pump appended the row",
        Duration::from_secs(10),
        &log,
        || {
            std::fs::read_to_string(&log)
                .is_ok_and(|l| l.contains("kv pump: appended a local write"))
        },
    );
    // cw-rails dies; the next user action re-ensures it and the row is there.
    kill_rails(&lock, rails_port, &log);
    let row = rails_kv()
        .get(app, key)
        .expect("the store answers after a re-ensure");
    assert_eq!(
        row.map(|r| r.value),
        Some(value),
        "the row survives a cw-rails restart"
    );

    // Two racing bring-ups: both Ok, one holder, the loser refused by name.
    kill_rails(&lock, rails_port, &log);
    let gate = std::sync::Barrier::new(2);
    let (a, b) = std::thread::scope(|s| {
        let race = || {
            gate.wait();
            ensure_rails(&base, false)
        };
        let (a, b) = (s.spawn(race), s.spawn(race));
        (a.join().expect("racer a"), b.join().expect("racer b"))
    });
    assert!(
        a.is_ok() && b.is_ok(),
        "both racers reach cw-rails: {a:?} / {b:?}"
    );
    wait_until(
        "the losing cw-rails exited",
        Duration::from_secs(10),
        &log,
        || holders(&lock).len() == 1,
    );
    let written = std::fs::read_to_string(&log).expect("rails.log");
    assert!(
        written.contains("is held by another cw-rails"),
        "the loser's refusal is in {}:\n{written}",
        log.display()
    );
}

/// One loopback GET's body, or `None` while nothing listens.
fn body(port: u16, path: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    raw.split_once("\r\n\r\n").map(|(_, b)| b.to_string())
}

/// A terminal-class daemon with `rails_base` pinned to `rails_port` and a
/// cw-rails root holding NO rails.toml, so nothing but the bring-up argv can
/// put cw-rails on that port (five-programs-66).
struct PinnedBoot {
    _reaper: Reaper,
    _daemon: Killed,
    _root: tempfile::TempDir,
    rails_port: u16,
    lock: PathBuf,
    log: PathBuf,
}

fn boot_pinned(local_only: bool) -> PinnedBoot {
    let rails_bin = cw_rails_bin();
    let root = tempfile::tempdir().expect("tempdir");
    let (home, data, rails_dir) = (
        root.path().join("home"),
        root.path().join("data"),
        root.path().join("rails"),
    );
    for d in [&home, &data, &rails_dir] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (client, internal, rails_port) = (free_port(), free_port(), free_port());
    let (lock, log) = (rails_dir.join("rails.lock"), rails_dir.join("rails.log"));
    let reaper = Reaper(lock.clone());
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[node]\nentry_node = \"00000000000000000000000000000001\"\n\n\
             [daemon]\nclient_port = {client}\ninternal_port = {internal}\n\
             rails_base = \"http://127.0.0.1:{rails_port}\"\n\n[data]\ndir = \"{}\"\n",
            data.display()
        ),
    )
    .expect("config");
    let stderr = root.path().join("daemon.stderr.log");
    let daemon = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", root.path().join("svrnmesh"))
            .env("CW_RAILS_DIR", &rails_dir)
            .env("CW_RAILS_BIN", &rails_bin)
            .env("RUST_LOG", "info")
            // The one decider reads the env over config (LocalOnlyProfile).
            .env("SOVEREIGN_LOCAL_ONLY", if local_only { "1" } else { "0" })
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&stderr).expect("stderr")))
            .spawn()
            .expect("spawn sovereign-daemon"),
    );
    wait_until(
        "cw-rails answered on the pinned rails_base port",
        Duration::from_secs(60),
        &stderr,
        || status(rails_port, "/v1/mesh/status") == Some(200),
    );
    PinnedBoot {
        _reaper: reaper,
        _daemon: daemon,
        _root: root,
        rails_port,
        lock,
        log,
    }
}

/// PROOF (2): with no rails.toml, the only thing naming the port is the
/// daemon's `rails_base` — cw-rails serves it because the bring-up passes it.
#[test]
fn a_pinned_rails_base_is_the_port_the_brought_up_cw_rails_serves() {
    let boot = boot_pinned(false);
    assert_eq!(holders(&boot.lock).len(), 1, "boot brought up ONE cw-rails");
    let written = std::fs::read_to_string(&boot.log).expect("rails.log");
    assert!(
        written.contains(&format!("127.0.0.1:{}", boot.rails_port)),
        "cw-rails names the pinned port in {}:\n{written}",
        boot.log.display()
    );
}

/// PROOF (3): a local-only node's cw-rails runs with n0 severed — its status
/// says so, and its log names no n0 relay.
#[test]
fn a_local_only_boot_brings_cw_rails_up_with_n0_services_off() {
    let boot = boot_pinned(true);
    let doc: serde_json::Value =
        serde_json::from_str(&body(boot.rails_port, "/v1/mesh/status").expect("status body"))
            .expect("status json");
    assert_eq!(
        doc["relay"]["n0_services"],
        serde_json::json!(false),
        "a local-only node's cw-rails reports n0 services off: {doc}"
    );
    let written = std::fs::read_to_string(&boot.log).expect("rails.log");
    assert!(
        !written.contains("iroh.link"),
        "a local-only cw-rails names no n0 relay, but {} does:\n{written}",
        boot.log.display()
    );
}
