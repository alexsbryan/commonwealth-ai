// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh up`, the one opt-in bring-up of cw-rails, against the REAL
//! cw-rails binary (pb-rails-untether, phase-b-31). svrn's boot brings
//! nothing up (sovereign-daemon tests/solo_rails_e2e.rs), so these are the
//! proofs that used to ride its boot, re-pointed at the verb:
//!
//! - an upgraded node's legacy `rings/` is handed over BEFORE cw-rails starts,
//!   so the cw-rails the verb brings up serves that history (phase-b-3);
//! - with a cw-rails already answering, nothing moves under its live store,
//!   and the warn names what waits;
//! - two racing runs yield ONE cw-rails on `rails.lock`;
//! - a local-only node's cw-rails serves the pinned `rails_base` port with n0
//!   services off (five-programs-66).
//!
//! Each run gets its own `SVRNMESH_DATA_DIR` (the config the verb reads),
//! `CW_RAILS_DIR` and `CW_RAILS_BIN`, never the developer's. Linux only: the
//! holder count reads `/proc/*/fd`.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use bytes::Bytes;
use sovereign_contracts::peer::ReplicatedKv;
use sovereign_contracts::principal::NodeId;
use sovereign_turn_client::rails_kv::RailsKv;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-mesh");

/// `CW_RAILS_BIN`, else `cw-rails` beside the binary this test was built
/// with. Absent is a FAILURE naming the build, never a skip (five-programs-62).
fn cw_rails_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("CW_RAILS_BIN") {
        return PathBuf::from(p);
    }
    let beside = Path::new(BIN).with_file_name("cw-rails");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p commonwealth-rails`, or set CW_RAILS_BIN",
        beside.display()
    );
    beside
}

/// Kills every cw-rails holding this root's lock on every exit path. The
/// verb detaches what it starts by design, so the lock names it.
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

/// One loopback GET's status and body, or `None` while nothing listens.
fn get(port: u16, path: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
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

/// Kill the root's cw-rails and wait until its port refuses.
fn kill_rails(lock: &Path, port: u16, log: &Path) {
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
        || holders(lock).is_empty() && get(port, "/v1/mesh/status").is_none(),
    );
}

/// One temp node: its svrn data dir, its config root, and its cw-rails root.
struct Node {
    _root: tempfile::TempDir,
    home: PathBuf,
    svrnmesh: PathBuf,
    data: PathBuf,
    rails_dir: PathBuf,
    rails_port: u16,
}

impl Node {
    /// A terminal-class node whose config pins `rails_base` to a free port
    /// and `[data] dir` to its own root.
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let p = |n: &str| root.path().join(n);
        let (home, svrnmesh, data, rails_dir) = (p("home"), p("svrnmesh"), p("data"), p("rails"));
        for d in [&home, &svrnmesh, &data, &rails_dir] {
            std::fs::create_dir_all(d).expect("dir");
        }
        let rails_port = free_port();
        std::fs::write(
            svrnmesh.join("config.toml"),
            format!(
                "[node]\nentry_node = \"00000000000000000000000000000001\"\n\n\
                 [daemon]\nrails_base = \"http://127.0.0.1:{rails_port}\"\n\n\
                 [data]\ndir = \"{}\"\n",
                data.display()
            ),
        )
        .expect("config");
        Self {
            _root: root,
            home,
            svrnmesh,
            data,
            rails_dir,
            rails_port,
        }
    }

    fn lock(&self) -> PathBuf {
        self.rails_dir.join("rails.lock")
    }

    fn log(&self) -> PathBuf {
        self.rails_dir.join("rails.log")
    }

    /// `svrn mesh up`, as the dispatcher execs it, with `env` added.
    fn mesh_up(&self, env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(["mesh", "up"])
            .env("HOME", &self.home)
            .env("SVRNMESH_DATA_DIR", &self.svrnmesh)
            .env("CW_RAILS_DIR", &self.rails_dir)
            .env("CW_RAILS_BIN", cw_rails_bin())
            .env("RUST_LOG", "info")
            .env_remove("SOVEREIGN_LOCAL_ONLY")
            .stdin(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("run sovereign-cli-mesh mesh up")
    }

    /// A cw-rails on this root at `port`, started by hand (no verb).
    fn spawn_rails(&self, port: u16, log: &Path) {
        Command::new(cw_rails_bin())
            .args(["run", "--listen"])
            .arg(port.to_string())
            .env("HOME", &self.home)
            .env("CW_RAILS_DIR", &self.rails_dir)
            .env("RUST_LOG", "info,rails=debug")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(log).expect("rails log")))
            .spawn()
            .expect("spawn cw-rails");
        wait_until("cw-rails answered", Duration::from_secs(30), log, || {
            get(port, "/v1/mesh/status").is_some_and(|(s, _)| s == 200)
        });
    }

    /// An upgraded node's layout (phase-b-3): two namespaces a real cw-rails
    /// wrote on this root (so they carry its key), then moved under svrn's
    /// `data/rings/`, where they sat before fp-54. Returns the namespaces.
    fn seed_legacy_rings(&self) -> Vec<String> {
        let port = free_port();
        let log = self.rails_dir.join("seed.log");
        self.spawn_rails(port, &log);
        let kv = RailsKv::new(format!("http://127.0.0.1:{port}"));
        for (app, key, value) in SEEDED {
            kv.set(app, key, Bytes::from_static(value), NodeId::from_u128(7))
                .expect("a seed row is written");
        }
        wait_until(
            "the pump appended both rows",
            Duration::from_secs(10),
            &log,
            || {
                std::fs::read_to_string(&log).is_ok_and(|l| {
                    l.matches("kv pump: appended a local write").count() >= SEEDED.len()
                })
            },
        );
        kill_rails(&self.lock(), port, &log);
        let (from, to) = (self.rails_dir.join("rings"), self.data.join("rings"));
        std::fs::create_dir_all(&to).expect("data/rings");
        let mut namespaces = Vec::new();
        for e in std::fs::read_dir(&from)
            .expect("the seed wrote rings/")
            .flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();
            std::fs::rename(e.path(), to.join(&name)).expect("move a namespace");
            namespaces.push(name);
        }
        assert_eq!(
            namespaces.len(),
            2,
            "the fixture is two namespaces: {namespaces:?}"
        );
        namespaces
    }
}

/// The rows the fixture seeds, one per namespace.
const SEEDED: [(&str, &str, &[u8]); 2] = [
    ("portfolio-private", "handover-e2e", b"kept"),
    ("notes", "handover-e2e", b"also kept"),
];

fn said(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// PROOF: on an upgraded node, `svrn mesh up` hands the legacy rings over
/// and THEN brings cw-rails up, so the cw-rails it starts serves the history
/// the node already had. PLANT: drop the handover from the verb and every
/// seeded row reads empty.
#[test]
fn mesh_up_hands_the_rings_over_then_brings_cw_rails_up() {
    let node = Node::new();
    let _reaper = Reaper(node.lock());
    let namespaces = node.seed_legacy_rings();

    let out = node.mesh_up(&[]);
    assert!(out.status.success(), "mesh up failed: {}", said(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("(started, pid"),
        "mesh up started cw-rails: {}",
        said(&out)
    );
    assert_eq!(
        holders(&node.lock()).len(),
        1,
        "mesh up brought up ONE cw-rails"
    );
    for ns in &namespaces {
        assert!(
            !node.data.join("rings").join(ns).exists(),
            "{ns} was handed over"
        );
    }
    let kv = RailsKv::new(format!("http://127.0.0.1:{}", node.rails_port));
    for (app, key, value) in SEEDED {
        let row = kv.get(app, key).expect("cw-rails answers the read");
        assert_eq!(
            row.map(|r| r.value),
            Some(Bytes::from_static(value)),
            "{app}/{key} reads back after mesh up: {}\ncw-rails log:\n{}",
            said(&out),
            std::fs::read_to_string(node.log()).unwrap_or_default()
        );
    }

    // A second run finds it serving and moves nothing.
    let again = node.mesh_up(&[]);
    assert!(
        again.status.success()
            && String::from_utf8_lossy(&again.stdout).contains("already running"),
        "a second mesh up reaches the running cw-rails: {}",
        said(&again)
    );
    assert_eq!(holders(&node.lock()).len(), 1, "still ONE cw-rails");
}

/// A cw-rails already answering: nothing moves under its live store. The
/// namespaces stay under svrn's data dir, and the verb's warn names them
/// (phase-b-3, principle 6).
#[test]
fn a_live_cw_rails_takes_nothing_and_mesh_up_names_what_waits() {
    let node = Node::new();
    let _reaper = Reaper(node.lock());
    let namespaces = node.seed_legacy_rings();
    node.spawn_rails(node.rails_port, &node.rails_dir.join("live.log"));

    let out = node.mesh_up(&[]);
    assert!(
        out.status.success(),
        "mesh up reaches the live cw-rails: {}",
        said(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.contains("cw-rails already answers, so nothing moved under it"))
        .unwrap_or_else(|| panic!("no warn names what waits: {}", said(&out)));
    for ns in &namespaces {
        assert!(line.contains(ns.as_str()), "the warn names {ns}: {line}");
        assert!(
            node.data.join("rings").join(ns).is_dir(),
            "{ns} stays at the source"
        );
        assert!(
            !node.rails_dir.join("rings").join(ns).exists(),
            "{ns} did not move under the live store"
        );
    }
}

/// Two racing `svrn mesh up` runs: both exit 0, ONE cw-rails holds the
/// lock, and the loser's refusal is in `rails.log`.
#[test]
fn two_racing_mesh_up_runs_yield_one_cw_rails() {
    let node = Node::new();
    let _reaper = Reaper(node.lock());
    let gate = std::sync::Barrier::new(2);
    let (a, b) = std::thread::scope(|s| {
        let race = || {
            gate.wait();
            node.mesh_up(&[])
        };
        let (a, b) = (s.spawn(race), s.spawn(race));
        (a.join().expect("racer a"), b.join().expect("racer b"))
    });
    assert!(
        a.status.success() && b.status.success(),
        "both racers reach cw-rails:\n{}\n---\n{}",
        said(&a),
        said(&b)
    );
    wait_until(
        "the losing cw-rails exited",
        Duration::from_secs(10),
        &node.log(),
        || holders(&node.lock()).len() == 1,
    );
    let log = std::fs::read_to_string(node.log()).expect("rails.log");
    assert!(
        log.contains("is held by another cw-rails"),
        "the loser's refusal is in rails.log:\n{log}"
    );
}

/// A local-only node (five-programs-66): with no rails.toml, only the
/// verb's `--listen` can put cw-rails on the pinned port, and only its
/// `--local-only` can turn n0 services off.
#[test]
fn a_local_only_nodes_mesh_up_serves_the_pinned_port_with_n0_services_off() {
    let node = Node::new();
    let _reaper = Reaper(node.lock());
    let out = node.mesh_up(&[("SOVEREIGN_LOCAL_ONLY", "1")]);
    assert!(out.status.success(), "mesh up failed: {}", said(&out));
    let (_, body) = get(node.rails_port, "/v1/mesh/status").expect("cw-rails on the pinned port");
    let doc: serde_json::Value = serde_json::from_str(&body).expect("status json");
    assert_eq!(
        doc["relay"]["n0_services"],
        serde_json::json!(false),
        "a local-only node's cw-rails reports n0 services off: {doc}"
    );
    let log = std::fs::read_to_string(node.log()).expect("rails.log");
    assert!(
        log.contains(&format!("127.0.0.1:{}", node.rails_port)),
        "cw-rails names the pinned port in rails.log:\n{log}"
    );
    assert!(
        !log.contains("iroh.link"),
        "a local-only cw-rails names no n0 relay:\n{log}"
    );
}

/// A local-only node's `svrn mesh up` finds a cw-rails with n0 services on
/// serving its `rails_base`: a named refusal, exit 1, and no second cw-rails
/// beside it (five-programs-66).
#[test]
fn a_local_only_nodes_mesh_up_refuses_an_n0_cw_rails() {
    let node = Node::new();
    let _reaper = Reaper(node.lock());
    node.spawn_rails(node.rails_port, &node.rails_dir.join("n0.log"));
    let out = node.mesh_up(&[("SOVEREIGN_LOCAL_ONLY", "1")]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "mesh up refuses: {}",
        said(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("uses n0 services"),
        "the refusal names the n0 posture: {}",
        said(&out)
    );
    assert_eq!(
        holders(&node.lock()).len(),
        1,
        "no second cw-rails beside it"
    );
}
