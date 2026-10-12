// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[engine] kind = "llama-server"` end to end (ENGINE_SWAP_PLAN step 1,
//! slice e): the stock daemon boots on the shipped llama-server, verifies its
//! build, serves a completion and an embedding through the router, names it
//! in `/status`, and when the daemon is SIGKILLed no llama-server outlives it.
//!
//! Boots `sovereign-stock` with the argv the sovereign-daemon binary takes
//! (`run --config <path>`) on a temp root: its own ports, data dir and HOME,
//! cw-rails pinned to a closed `rails_base` with no binary to bring up. The
//! production daemon and `~/.sovereign` are never touched, and the router
//! binds a free port of its own.
//!
//! Ignored in the suite: it loads real GGUFs and needs the shipped server
//! beside the daemon (`serve/crates/sovereign-compute/llama-server/build.sh`
//! installs it and its manifest into target/debug). Run by hand, in the
//! toolbox:
//!
//!   SOVEREIGN_MODELS_DIR=$PWD/sovereign/models cargo test -p sovereign-stock \
//!     --test llama_server_router_e2e -- --ignored --nocapture
//!
//! Linux only: the process census reads `/proc`.
#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-stock");

/// The smallest chat and embedding models in `sovereign/models/`: the test
/// is about the router's lifecycle, not about what the model says.
const CHAT_MODEL: &str = "Qwen3.5-0.8B-UD-Q6_K_XL.gguf";
const EMBED_MODEL: &str = "Qwen3-Embedding-0.6B-Q8_0.gguf";

/// Kills and reaps the daemon on every exit path, panics included.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A loopback port reserved by parking it in TIME_WAIT (one_process_e2e,
/// note 3f7d5a40).
fn reserved_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
    let addr = listener.local_addr().expect("ephemeral port");
    let client = std::net::TcpStream::connect(addr).expect("connect to the reserved port");
    let (accepted, _) = listener.accept().expect("accept on the reserved port");
    drop(accepted);
    drop(client);
    addr.port()
}

/// The GGUF directory, from `SOVEREIGN_MODELS_DIR` (quality/env-flags.toml):
/// never from the crate's own path, which escapes the package root.
fn models_dir() -> PathBuf {
    let dir = std::env::var_os("SOVEREIGN_MODELS_DIR")
        .map(PathBuf::from)
        .expect("SOVEREIGN_MODELS_DIR names the GGUF directory, absolute");
    for m in [CHAT_MODEL, EMBED_MODEL] {
        assert!(dir.join(m).is_file(), "{} is not in {}", m, dir.display());
    }
    dir
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
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
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn post_json(url: &str, body: Value) -> Result<Value, String> {
    let resp = client()
        .post(url)
        .json(&body)
        .send()
        .map_err(|e| format!("no answer from {url}: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .map_err(|e| format!("{url} answered HTTP {status} unreadably: {e}"))?;
    match status.is_success() {
        true => Ok(v),
        false => Err(format!("{url} answered HTTP {status}: {v}")),
    }
}

/// A process as `/proc/<pid>/stat` names it: its parent, its start time (so
/// a reused pid is not mistaken for the original) and whether it is a zombie.
struct Stat {
    ppid: u32,
    start: u64,
    zombie: bool,
}

fn stat(pid: u32) -> Option<Stat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesised and may hold spaces: split after it.
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

/// Every llama-server descended from `root`, by pid and start time.
fn llama_servers_under(root: u32) -> BTreeMap<u32, u64> {
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut starts = BTreeMap::new();
    for e in std::fs::read_dir("/proc")
        .expect("/proc")
        .filter_map(Result::ok)
    {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Some(s) = stat(pid) {
            children.entry(s.ppid).or_default().push(pid);
            starts.insert(pid, s.start);
        }
    }
    let (mut found, mut queue) = (BTreeMap::new(), vec![root]);
    while let Some(p) = queue.pop() {
        for &c in children.get(&p).into_iter().flatten() {
            if comm(c) == "llama-server" {
                found.insert(c, starts[&c]);
            }
            queue.push(c);
        }
    }
    found
}

/// The ones of `procs` still running as the same process.
fn still_running(procs: &BTreeMap<u32, u64>) -> BTreeSet<u32> {
    procs
        .iter()
        .filter(|(&pid, &start)| stat(pid).is_some_and(|s| s.start == start && !s.zombie))
        .map(|(&pid, _)| pid)
        .collect()
}

#[test]
#[ignore = "loads real GGUFs and needs the shipped llama-server: see the module doc"]
fn the_daemon_serves_through_its_router_and_takes_it_down_when_killed() {
    let models = models_dir();
    let shipped = Path::new(BIN).with_file_name("llama-server");
    assert!(
        shipped.is_file(),
        "no shipped llama-server at {}; build it with \
         serve/crates/sovereign-compute/llama-server/build.sh",
        shipped.display()
    );

    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir, data) = (
        root.path().join("home"),
        root.path().join("rails"),
        root.path().join("data"),
    );
    for d in [&home, &rails_dir, &data] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (svrn, internal, serve, dead_rails) = (
        reserved_port(),
        reserved_port(),
        reserved_port(),
        reserved_port(),
    );
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[engine]\nkind = \"llama-server\"\n\n[models]\nprimary = \"{m}/{CHAT_MODEL}\"\n\
             embed = \"{m}/{EMBED_MODEL}\"\ncontext_size = 4096\n\n[daemon]\n\
             client_port = {svrn}\ninternal_port = {internal}\n\
             rails_base = \"http://127.0.0.1:{dead_rails}\"\n\n[data]\ndir = \"{d}\"\n",
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
            .env_remove("RUST_LOG")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    let daemon = stock.0.id();

    // Boot: the router and both models report through /status.
    let mut children = Vec::new();
    wait_until(
        "/status listing the router and both models loaded",
        Duration::from_secs(300),
        &log,
        || {
            assert!(
                stock.0.try_wait().expect("try_wait").is_none(),
                "the daemon exited during boot:\n{}",
                log_tail(&log)
            );
            let Some(status) = client()
                .get(format!("http://127.0.0.1:{svrn}/status"))
                .send()
                .ok()
                .and_then(|r| r.json::<Value>().ok())
            else {
                return false;
            };
            children = status["inference"]["compute_children"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let loaded = children
                .iter()
                .filter(|c| c["role"] == "router-model" && c["lifecycle"] == "loaded")
                .count();
            loaded == 2
        },
    );
    let router = children
        .iter()
        .find(|c| c["role"] == "router")
        .unwrap_or_else(|| panic!("/status lists no router: {children:?}"));
    assert_eq!(router["lifecycle"], "serving", "{router}");
    let named = router["model_id"].as_str().unwrap_or_default();
    assert!(
        named.contains("shipped, build verified") && named.contains(&*shipped.to_string_lossy()),
        "/status does not name the verified shipped binary: {named}"
    );

    // Serving: a completion and an embedding, through serve's OpenAI routes.
    let done = post_json(
        &format!("http://127.0.0.1:{serve}/v1/chat/completions"),
        serde_json::json!({
            "messages": [{ "role": "user", "content": "Say the word ready." }],
            "max_tokens": 64,
            "temperature": 0.0
        }),
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
    let generated = done["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    assert!(generated > 0, "the completion generated nothing: {done}");
    let embedded = post_json(
        &format!("http://127.0.0.1:{serve}/v1/embeddings"),
        serde_json::json!({
            "model": EMBED_MODEL.trim_end_matches(".gguf"),
            "input": "hello router"
        }),
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
    let dims = embedded["data"][0]["embedding"]
        .as_array()
        .map_or(0, Vec::len);
    assert!(dims > 0, "the embedding is empty: {embedded}");

    // The processes: the router and one child per loaded model, all under
    // the daemon.
    let servers = llama_servers_under(daemon);
    assert_eq!(
        servers.len(),
        3,
        "expected the router and two model processes under the daemon, found {servers:?}"
    );
    eprintln!(
        "booted: {generated} tokens generated, {dims}-dim embedding, llama-servers {:?}",
        servers.keys()
    );

    // SIGKILL the daemon: nothing it started may outlive it.
    stock.0.kill().expect("SIGKILL the daemon");
    stock.0.wait().expect("reap the daemon");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut left = still_running(&servers);
    while !left.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        left = still_running(&servers);
    }
    if !left.is_empty() {
        for pid in &left {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        panic!(
            "llama-server outlived the SIGKILLed daemon: {left:?} (killed now); router log:\n{}",
            log_tail(&data.join("llama-server/router.log"))
        );
    }
}
