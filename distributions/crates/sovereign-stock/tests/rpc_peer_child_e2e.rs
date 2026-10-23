// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[compute] rpc_peer` end to end (ENGINE_SWAP_PLAN F, f1): the stock daemon
//! starts the shipped `ggml-rpc-server -c` as a supervised child bound to
//! this host's LAN address, with `LLAMA_CACHE` naming a cache under its data
//! dir; a llama-server `--rpc` at it loads a model twice, the second load
//! writing nothing to that cache (every cached tensor a hit, the
//! `bench/lanes/engine-swap/derisk-20261012/dist-spike.sh` shape); a SIGKILLed
//! child is restarted by its supervisor; and a SIGKILLed daemon leaves no
//! rpc-server or llama-server behind.
//!
//! Boots `sovereign-stock` as `llama_server_router_e2e` does: a temp root
//! with its own ports, data dir and HOME, cw-rails pinned closed. The peer
//! binds the address this host routes from, never loopback (refused) and
//! never every interface (refused), so the host needs a LAN or tunnel.
//!
//! Ignored in the suite: it loads real GGUFs and needs the shipped server
//! and rpc-server beside the daemon (`build.sh`). Run by hand, in the toolbox:
//!
//!   SOVEREIGN_MODELS_DIR=$PWD/sovereign/models cargo test -p sovereign-stock \
//!     --test rpc_peer_child_e2e -- --ignored --nocapture
//!
//! Linux only: the process census reads `/proc`, and the supervisor's
//! parent-death signal, which the last step proves, is Linux's.
#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-stock");

/// The router e2e's models: the peer daemon needs a config that boots, and
/// the head loads the chat model through the peer.
const CHAT_MODEL: &str = "Qwen3.5-0.8B-UD-Q6_K_XL.gguf";
const EMBED_MODEL: &str = "Qwen3-Embedding-0.6B-Q8_0.gguf";

/// Kills and reaps a process on every exit path, panics included.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The address this host routes from: a connected UDP socket's local end
/// (no packet is sent).
fn lan_ip() -> IpAddr {
    let s = UdpSocket::bind("0.0.0.0:0").expect("udp socket");
    s.connect("192.0.2.1:9")
        .expect("this host has no route off loopback, so no LAN or tunnel address to bind");
    let ip = s.local_addr().expect("local address").ip();
    assert!(
        !ip.is_loopback() && !ip.is_unspecified(),
        "no LAN address: {ip}"
    );
    ip
}

/// A port reserved by parking it in TIME_WAIT (one_process_e2e, note 3f7d5a40).
fn reserved_port(ip: IpAddr) -> u16 {
    let listener = TcpListener::bind((ip, 0)).expect("ephemeral port");
    let addr = listener.local_addr().expect("ephemeral port");
    let client = TcpStream::connect(addr).expect("connect to the reserved port");
    let (accepted, _) = listener.accept().expect("accept on the reserved port");
    drop(accepted);
    drop(client);
    addr.port()
}

fn models_dir() -> PathBuf {
    let dir = std::env::var_os("SOVEREIGN_MODELS_DIR")
        .map(PathBuf::from)
        .expect("SOVEREIGN_MODELS_DIR names the GGUF directory, absolute");
    for m in [CHAT_MODEL, EMBED_MODEL] {
        assert!(dir.join(m).is_file(), "{} is not in {}", m, dir.display());
    }
    dir
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
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `/proc/<pid>/stat`: parent, start time, zombie.
struct Stat {
    ppid: u32,
    start: u64,
    zombie: bool,
}

fn stat(pid: u32) -> Option<Stat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = text.rsplit_once(')')?.1.split_whitespace().collect();
    Some(Stat {
        zombie: fields.first()? == &"Z",
        ppid: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

fn comm(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// A NUL-separated `/proc/<pid>/` file as its entries.
fn proc_list(pid: u32, file: &str) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/{file}"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Every process named one of `names` descended from `root`, by pid and
/// start time.
fn descendants(root: u32, names: &[&str]) -> BTreeMap<u32, u64> {
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut starts = BTreeMap::new();
    for e in std::fs::read_dir("/proc")
        .expect("/proc")
        .filter_map(Result::ok)
    {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Some(s) = stat(pid).filter(|s| !s.zombie) {
            children.entry(s.ppid).or_default().push(pid);
            starts.insert(pid, s.start);
        }
    }
    let (mut found, mut queue) = (BTreeMap::new(), vec![root]);
    while let Some(p) = queue.pop() {
        for &c in children.get(&p).into_iter().flatten() {
            if names.contains(&comm(c).as_str()) {
                found.insert(c, starts[&c]);
            }
            queue.push(c);
        }
    }
    found
}

fn still_running(procs: &BTreeMap<u32, u64>) -> BTreeSet<u32> {
    procs
        .iter()
        .filter(|(&pid, &start)| stat(pid).is_some_and(|s| s.start == start && !s.zombie))
        .map(|(&pid, _)| pid)
        .collect()
}

/// The one rpc-server under the daemon, by pid and start time.
fn rpc_server_under(daemon: u32) -> Option<(u32, u64)> {
    let found = descendants(daemon, &["ggml-rpc-server"]);
    assert!(found.len() <= 1, "more than one rpc-server: {found:?}");
    found.into_iter().next()
}

/// The cache rpc-server keeps under `$LLAMA_CACHE/rpc`: each file's name,
/// size and modification time.
fn cache_snapshot(dir: &Path) -> BTreeMap<String, (u64, SystemTime)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((
                e.file_name().to_string_lossy().into_owned(),
                (m.len(), m.modified().ok()?),
            ))
        })
        .collect()
}

/// Load `model` through a llama-server whose only device is the peer, ask
/// it for a few tokens, and stop it. Returns the load time.
fn load_through_peer(server: &Path, model: &Path, peer: SocketAddr, root: &Path, n: u32) -> f64 {
    let port = reserved_port(IpAddr::from([127, 0, 0, 1]));
    let log = root.join(format!("head-{n}.log"));
    let started = Instant::now();
    let mut head = Killed(
        Command::new(server)
            .arg("-m")
            .arg(model)
            .args(["--rpc", &peer.to_string(), "-dev", "RPC0", "-ngl", "99"])
            .args(["-c", "2048", "-np", "1", "--host", "127.0.0.1"])
            .args(["--port", &port.to_string()])
            .env("HOME", root.join("home"))
            .env("LLAMA_CACHE", root.join("head-cache"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn the head llama-server"),
    );
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("http client");
    wait_until(
        &format!("load {n} through the peer answering /health"),
        Duration::from_secs(300),
        &log,
        || {
            assert!(
                head.0.try_wait().expect("try_wait").is_none(),
                "the head exited during load {n}:\n{}",
                log_tail(&log)
            );
            http.get(format!("http://127.0.0.1:{port}/health"))
                .send()
                .is_ok_and(|r| r.status().is_success())
        },
    );
    let load = started.elapsed().as_secs_f64();
    let done: Value = http
        .post(format!("http://127.0.0.1:{port}/completion"))
        .json(&serde_json::json!({ "prompt": "The capital of France is", "n_predict": 8 }))
        .send()
        .and_then(|r| r.json())
        .unwrap_or_else(|e| panic!("load {n}: no completion: {e}\n{}", log_tail(&log)));
    let predicted = done["tokens_predicted"].as_u64().unwrap_or(0);
    assert!(predicted > 0, "load {n} generated nothing: {done}");
    // SIGTERM and wait, so the peer sees its one client leave before the next.
    let _ = Command::new("kill").arg(head.0.id().to_string()).status();
    let _ = head.0.wait();
    load
}

#[test]
#[ignore = "loads real GGUFs and needs the shipped llama-server and ggml-rpc-server: see the module doc"]
fn rpc_peer_child_e2e() {
    let models = models_dir();
    let server = Path::new(BIN).with_file_name("llama-server");
    let rpc_server = Path::new(BIN).with_file_name("ggml-rpc-server");
    for b in [&server, &rpc_server] {
        assert!(
            b.is_file(),
            "no shipped {} ; build it with serve/crates/sovereign-compute/llama-server/build.sh",
            b.display()
        );
    }
    let lan = lan_ip();
    let peer = SocketAddr::new(lan, reserved_port(lan));

    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir, data) = (
        root.path().join("home"),
        root.path().join("rails"),
        root.path().join("data"),
    );
    for d in [&home, &rails_dir, &data] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let loopback = IpAddr::from([127, 0, 0, 1]);
    let (svrn, internal, serve, dead_rails) = (
        reserved_port(loopback),
        reserved_port(loopback),
        reserved_port(loopback),
        reserved_port(loopback),
    );
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[engine]\nkind = \"llama-server\"\n\n[models]\nprimary = \"{m}/{CHAT_MODEL}\"\n\
             embed = \"{m}/{EMBED_MODEL}\"\ncontext_size = 4096\n\n[daemon]\n\
             client_port = {svrn}\ninternal_port = {internal}\n\
             rails_base = \"http://127.0.0.1:{dead_rails}\"\n\n[data]\ndir = \"{d}\"\n\n\
             [compute]\nrpc_peer = \"{peer}\"\n",
            m = models.display(),
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
            .env("RUST_LOG", "info,rpc_peer=debug")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    let daemon = stock.0.id();

    // The child: under the daemon, at this host's LAN address, with the cache
    // the daemon names, and listening.
    let mut child = None;
    wait_until(
        "a listening ggml-rpc-server under the daemon",
        Duration::from_secs(300),
        &log,
        || {
            assert!(
                stock.0.try_wait().expect("try_wait").is_none(),
                "the daemon exited during boot:\n{}",
                log_tail(&log)
            );
            child = rpc_server_under(daemon);
            child.is_some() && TcpStream::connect_timeout(&peer, Duration::from_secs(2)).is_ok()
        },
    );
    let (pid, _) = child.expect("the child");
    let argv = proc_list(pid, "cmdline");
    let wanted = ["-H", &lan.to_string(), "-p", &peer.port().to_string(), "-c"].map(String::from);
    assert!(
        argv.windows(5).any(|w| w == wanted),
        "the child is not `-H {lan} -p {} -c`: {argv:?}",
        peer.port()
    );
    let cache_root = data.join("rpc-peer");
    let llama_cache = format!("LLAMA_CACHE={}", cache_root.display());
    assert!(
        proc_list(pid, "environ").contains(&llama_cache),
        "the child's environment does not set {llama_cache}"
    );

    // Two loads through the peer: the first fills its cache, the second
    // writes nothing to it.
    let model = models.join(CHAT_MODEL);
    let cache = cache_root.join("rpc");
    let cold = load_through_peer(&server, &model, peer, root.path(), 1);
    let first = cache_snapshot(&cache);
    assert!(
        !first.is_empty(),
        "the first load cached nothing in {}",
        cache.display()
    );
    let warm = load_through_peer(&server, &model, peer, root.path(), 2);
    let second = cache_snapshot(&cache);
    assert_eq!(
        first, second,
        "the second load wrote to the cache, so not every cached tensor was a hit"
    );
    let bytes: u64 = first.values().map(|(len, _)| len).sum();

    // SIGKILL the child: its supervisor starts another at the same address.
    let before = rpc_server_under(daemon).expect("the child");
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    let mut after = None;
    wait_until(
        "the supervisor restarting the SIGKILLed rpc-server",
        Duration::from_secs(60),
        &log,
        || {
            after = rpc_server_under(daemon).filter(|c| *c != before);
            after.is_some() && TcpStream::connect_timeout(&peer, Duration::from_secs(2)).is_ok()
        },
    );
    let restarted = after.expect("the restarted child").0;

    eprintln!(
        "booted: rpc-server pid {pid} at {peer} (-c, {llama_cache}); load 1 {cold:.1}s cached \
         {} tensors ({bytes} bytes); load 2 {warm:.1}s wrote none; SIGKILLed child restarted as \
         pid {restarted}",
        first.len()
    );

    // SIGKILL the daemon: nothing it started may outlive it.
    let started = descendants(daemon, &["ggml-rpc-server", "llama-server"]);
    stock.0.kill().expect("SIGKILL the daemon");
    stock.0.wait().expect("reap the daemon");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut left = still_running(&started);
    while !left.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        left = still_running(&started);
    }
    if !left.is_empty() {
        for pid in &left {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        panic!("processes outlived the SIGKILLed daemon: {left:?} (killed now)");
    }
}
