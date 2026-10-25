// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[compute] rpc_peers` end to end (ENGINE_SWAP_PLAN F, f2): a head daemon
//! whose router serves Qwen3.5-9B split across a peer's `ggml-rpc-server`,
//! on loopback. The preset's distributed sections carry the peer's `rpc`
//! endpoint; llama.cpp loads the 9B across the peer and this card, in the
//! order `arg.cpp:2598` names (RPC devices first), so decode is at 90% or
//! more of the same model on one device (D2, `dist-spike.sh`'s shape).
//!
//! It then proves the restart-on-peer-loss policy: SIGKILL the peer's
//! rpc-server child, let the model's next RPC touch abort the router child
//! (`ggml-rpc.cpp:30`, upstream never reconnects), see `/status` name the
//! lost peer, let the peer's supervisor start another child — and watch the
//! head's supervisor wait for it with backoff and the router answer again
//! after both restart. A head that fell back to one device would answer
//! too, so the recovered completion is checked against the router's own
//! argv: the distributed preset is still the one it serves.
//!
//! Ignored in the suite: it loads a real 9B GGUF twice over loopback and
//! needs the shipped llama-server and ggml-rpc-server beside the daemon
//! (`build.sh`). Run by hand, in the toolbox:
//!
//!   SOVEREIGN_MODELS_DIR=$PWD/sovereign/models cargo test -p sovereign-stock \
//!     --test router_rpc_split_e2e -- --ignored --nocapture
//!
//! `SOVEREIGN_E2E_DIST_CHAT_MODEL` picks the split model by role where the
//! default (`Qwen3.5-9B-UD-MTP-Q6_K_XL.gguf`, the D2 model) is not on the
//! host. Linux only: the process census reads `/proc`, and both halves of
//! the restart (parent-death signal, process group) are Linux's.
#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-stock");

/// The D2 model, by role: any chat GGUF larger than the peer's shard, an
/// absolute path or a file in `SOVEREIGN_MODELS_DIR`.
const CHAT_DEFAULT: &str = "Qwen3.5-9B-UD-MTP-Q6_K_XL.gguf";

/// The peer daemon's own (tiny) chat model: it lends tensors and serves
/// nothing, but its config must name a primary that is not its embedder.
const PEER_CHAT_MODEL: &str = "Qwen3.5-0.8B-UD-Q6_K_XL.gguf";

/// The peer daemon's config names an embedder, and the peer never embeds.
const EMBED_MODEL: &str = "Qwen3-Embedding-0.6B-Q8_0.gguf";

/// 128 tokens, as D2 measured (`dist-spike.sh`'s `n_predict`).
const DECODE_TOKENS: u32 = 128;

/// D2's bar: decode at 90% or more of the same model on one device.
const D2_BAR: f64 = 0.90;

/// Kills and reaps a process on every exit path, panics included.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The address this host routes from: a connected UDP socket's local end (no
/// packet is sent). The peer's bind must be one another host could reach,
/// and `[compute] rpc_peer` refuses loopback, so the e2e uses this.
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

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .expect("http client")
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

/// A daemon under test, on its own temp root.
struct Daemon {
    process: Killed,
    svrn: u16,
    serve: u16,
    log: PathBuf,
    /// The last `/status` failure, for a wait that never saw an answer.
    last_status_error: std::sync::Mutex<Option<String>>,
}

impl Daemon {
    fn pid(&self) -> u32 {
        self.process.0.id()
    }

    /// Whether the daemon process is still running. `&self`, not a
    /// `try_wait`: the wait-state probe needs `&mut`, and the e2e's own
    /// try_wait lives at the end, where nothing else borrows it.
    fn alive(&self) -> bool {
        stat(self.pid()).is_some_and(|s| !s.zombie)
    }

    /// `/status`, or `None` with why (the last failure is kept so a wait
    /// that never saw an answer can say what it got instead of "None").
    fn status(&self) -> Option<Value> {
        let url = format!("http://127.0.0.1:{}/status", self.svrn);
        match client().get(&url).send() {
            Ok(r) => match r.json::<Value>() {
                Ok(v) => Some(v),
                Err(e) => {
                    *self
                        .last_status_error
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) =
                        Some(format!("{url} answered unreadably: {e}"));
                    None
                }
            },
            Err(e) => {
                *self
                    .last_status_error
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(format!("{url}: {e}"));
                None
            }
        }
    }

    /// `/status`'s compute_children, the router and each model.
    fn children(&self) -> Vec<Value> {
        self.status()
            .and_then(|s| s["inference"]["compute_children"].as_array().cloned())
            .unwrap_or_default()
    }

    fn router(&self) -> Option<Value> {
        self.children().into_iter().find(|c| c["role"] == "router")
    }

    /// A model's lifecycle as `/status` last reported it.
    fn model_lifecycle(&self, model: &str) -> String {
        self.children()
            .into_iter()
            .find(|c| c["role"] == "router-model" && c["name"] == model)
            .and_then(|c| c["lifecycle"].as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// Decode `DECODE_TOKENS` tokens of the chat model through the ROUTER's
    /// own `/completion`, and answer their per-second rate. The router's
    /// route is what reports llama.cpp's own timing (`predicted_per_second`);
    /// the daemon's OpenAI route reports usage, not a rate, and the bar is a
    /// property of the model process, not of the daemon's shaping of it.
    fn decode_rate(&self, model: &str, log: &Path) -> f64 {
        let port = self
            .router()
            .and_then(|r| r["port"].as_u64())
            .unwrap_or_else(|| panic!("/status lists no router port; log:\n{}", log_tail(log)));
        let done = post_json(
            &format!("http://127.0.0.1:{port}/completion"),
            serde_json::json!({
                "model": model,
                "prompt": "Write a detailed history of the printing press.",
                "n_predict": DECODE_TOKENS,
                "temperature": 0.0,
                "ignore_eos": true
            }),
        )
        .unwrap_or_else(|e| panic!("{e}\n{}", log_tail(log)));
        let generated = done["tokens_predicted"].as_u64().unwrap_or(0);
        let rate = done["timings"]["predicted_per_second"]
            .as_f64()
            .unwrap_or_else(|| panic!("no decode rate in {done}"));
        eprintln!("decode: {generated} tokens at {rate:.2} tok/s");
        rate
    }
}

/// One daemon's ports, reserved once: the config the daemon reads and the
/// ports the test connects to MUST be the same three, so they travel
/// together ([`Ports::reserve`]) and the config is rendered from them.
struct Ports {
    svrn: u16,
    internal: u16,
    serve: u16,
    dead_rails: u16,
}

impl Ports {
    fn reserve(loopback: IpAddr) -> Self {
        Self {
            svrn: reserved_port(loopback),
            internal: reserved_port(loopback),
            serve: reserved_port(loopback),
            dead_rails: reserved_port(loopback),
        }
    }
}

/// Boot a daemon whose config `render` writes from these very `ports`.
fn boot(
    root: &Path,
    log_name: &str,
    ports: Ports,
    render: impl FnOnce(&Ports) -> String,
) -> Daemon {
    let (home, rails_dir, svrn, serve) = (
        root.join("home"),
        root.join("rails"),
        ports.svrn,
        ports.serve,
    );
    for d in [&home, &rails_dir] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let config = root.join("config.toml");
    std::fs::write(&config, render(&ports)).expect("config");
    let log = root.join(log_name);
    let process = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", root.join("svrnmesh"))
            .env("CW_RAILS_DIR", &rails_dir)
            .env("CW_RAILS_BIN", root.join("no-cw-rails"))
            .env("SOVEREIGN_SERVE_PORT", serve.to_string())
            .env("RUST_LOG", "info,llama_router=debug,rpc_peer=info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    Daemon {
        process,
        svrn,
        serve,
        log,
        last_status_error: std::sync::Mutex::new(None),
    }
}

/// The one rpc-server under `daemon`, by pid and start time.
fn rpc_server_under(daemon: u32) -> Option<(u32, u64)> {
    let found = descendants(daemon, &["ggml-rpc-server"]);
    assert!(found.len() <= 1, "more than one rpc-server: {found:?}");
    found.into_iter().next()
}

#[test]
#[ignore = "loads a real 9B GGUF across loopback RPC and needs the shipped llama-server and ggml-rpc-server: see the module doc"]
fn router_rpc_split_e2e() {
    let models_dir = std::env::var_os("SOVEREIGN_MODELS_DIR")
        .map(PathBuf::from)
        .expect("SOVEREIGN_MODELS_DIR names the GGUF directory, absolute");
    let chat_name =
        std::env::var("SOVEREIGN_E2E_DIST_CHAT_MODEL").unwrap_or_else(|_| CHAT_DEFAULT.to_string());
    let chat = models_dir.join(&chat_name);
    assert!(chat.is_file(), "{} is not on the host", chat.display());
    for m in [EMBED_MODEL, PEER_CHAT_MODEL] {
        assert!(
            models_dir.join(m).is_file(),
            "{m} is not in {}",
            models_dir.display()
        );
    }
    let chat_id = chat
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("a GGUF file name")
        .to_owned();
    let server = Path::new(BIN).with_file_name("llama-server");
    let rpc_server = Path::new(BIN).with_file_name("ggml-rpc-server");
    for b in [&server, &rpc_server] {
        assert!(
            b.is_file(),
            "no shipped {} ; build it with serve/crates/sovereign-compute/llama-server/build.sh",
            b.display()
        );
    }

    // ES_F2_KEEP_TMP keeps the whole tree for diagnosis; a run that passes
    // leaves nothing behind.
    // Under the target dir, not /tmp: the peer's rpc cache holds the shard (3 GB
    // for the 9B), /tmp is RAM on the Halo, and a run killed before the drop
    // left two of these behind to help OOM the first flip (2026-10-24).
    let root = match std::env::var_os("ES_F2_KEEP_TMP") {
        Some(dir) => tempfile::TempDir::new_in(dir).expect("kept tempdir"),
        None => tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir"),
    };
    if std::env::var_os("ES_F2_KEEP_TMP").is_some() {
        eprintln!("router rpc e2e: keeping {}", root.path().display());
    }
    let loopback = IpAddr::from([127, 0, 0, 1]);
    let lan = lan_ip();
    // The peer's split endpoint: a LAN address, so the head names one another
    // host could reach (the loopback the D2 spike used is refused by the
    // peer's own bind rule — es-f1 — and this is the same shape over a real
    // address the head connects to).
    let peer = SocketAddr::new(lan, reserved_port(lan));

    // --- The peer daemon: lends its GPU through the supervised child,
    // and serves nothing itself.
    let peer_root = root.path().join("peer");
    let peer_data = peer_root.join("data");
    std::fs::create_dir_all(&peer_data).expect("peer data");
    let mut peer_daemon = boot(
        &peer_root,
        "peer.stderr.log",
        Ports::reserve(loopback),
        |p| {
            format!(
                "[engine]\nkind = \"llama-server\"\n\n[models]\nprimary = \"{m}/{PEER_CHAT_MODEL}\"\n\
                 embed = \"{m}/{EMBED_MODEL}\"\ncontext_size = 4096\n\n[daemon]\n\
                 client_port = {svrn}\ninternal_port = {internal}\n\
                 rails_base = \"http://127.0.0.1:{dead_rails}\"\n\n[data]\ndir = \"{d}\"\n\n\
                 [compute]\nrpc_peer = \"{peer}\"\n",
                m = models_dir.display(),
                d = peer_data.display(),
                svrn = p.svrn,
                internal = p.internal,
                dead_rails = p.dead_rails,
            )
        },
    );

    // The peer's child: under the peer daemon, listening at the LAN address,
    // its pinned build verified.
    let mut child = None;
    wait_until(
        "a listening ggml-rpc-server under the peer daemon",
        Duration::from_secs(300),
        &peer_daemon.log,
        || {
            assert!(
                peer_daemon.alive(),
                "the peer daemon exited during boot:\n{}",
                log_tail(&peer_daemon.log)
            );
            child = rpc_server_under(peer_daemon.pid());
            child.is_some() && TcpStream::connect_timeout(&peer, Duration::from_secs(2)).is_ok()
        },
    );
    let (peer_child, _) = child.expect("the peer's rpc-server");

    // --- The head: its router serves the 9B split across the peer.
    let head_root = root.path().join("head");
    let head_data = head_root.join("data");
    std::fs::create_dir_all(&head_data).expect("head data");
    let mut head = boot(
        &head_root,
        "stock.stderr.log",
        Ports::reserve(loopback),
        |p| {
            format!(
                "[engine]\nkind = \"llama-server\"\n\n[models]\nprimary = \"{m}\"\n\
                 embed = \"{e}\"\ncontext_size = 4096\n\n[daemon]\n\
                 client_port = {svrn}\ninternal_port = {internal}\n\
                 rails_base = \"http://127.0.0.1:{dead_rails}\"\n\n[data]\ndir = \"{d}\"\n\n\
                 [compute]\nrpc_peers = [\"{peer}\"]\n",
                m = chat.display(),
                e = models_dir.join(EMBED_MODEL).display(),
                d = head_data.display(),
                svrn = p.svrn,
                internal = p.internal,
                dead_rails = p.dead_rails,
            )
        },
    );

    // The router is serving and the 9B is loaded — through the peers it was
    // told about, which the preset file names (its chat sections carry
    // `rpc`, and their child argv carries it too). The wait reports the
    // ROUTER's log, not only the daemon's: a model that will not load says
    // why there, and the daemon's tail is 40 lines of rails warnings.
    let router_log = head_root.join("data/llama-server/router.log");
    let booted_at = Instant::now();
    loop {
        assert!(
            head.alive(),
            "the head exited during boot:\n{}",
            log_tail(&head.log)
        );
        let serving = head.router().is_some_and(|r| r["lifecycle"] == "serving");
        let loaded = head.model_lifecycle(&chat_id);
        if serving && loaded == "loaded" {
            break;
        }
        assert!(
            peer_daemon.alive(),
            "the PEER daemon exited while the head booted; peer stderr:\n{}\n--- head stderr ---\n{}",
            log_tail(&peer_daemon.log),
            log_tail(&head.log)
        );
        if booted_at.elapsed() > Duration::from_secs(900) {
            panic!(
                "never saw: the router serving and {chat_id} loaded, within 900s; /status says \
                 router={:?} {chat_id}={loaded:?} (last /status error: {:?}); peer child {:?}; \
                 router log {}:\n{}\n--- head stderr ---\n{}",
                head.router().map(|r| r["lifecycle"].clone()),
                head.last_status_error
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()),
                rpc_server_under(peer_daemon.pid()),
                router_log.display(),
                log_tail(&router_log),
                log_tail(&head.log)
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let preset =
        std::fs::read_to_string(head_data.join("llama-server/router.ini")).expect("preset");
    let rpc_line = format!("rpc = {peer}");
    let distributed_sections = preset.matches(&rpc_line).count();
    assert_eq!(
        distributed_sections, 1,
        "the preset does not name {peer} on its one chat section:\n{preset}"
    );
    let embed_at = preset
        .find("[Qwen3-Embedding-0.6B-Q8_0]")
        .expect("the embedder's section");
    assert!(
        !preset[embed_at..]
            .lines()
            .take(5)
            .any(|l| l.starts_with("rpc =")),
        "the embedder was distributed too:\n{preset}"
    );
    // The model's own process holds the peers' devices: its argv carries the
    // `--rpc` the preset rendered.
    let model_pid = descendants(head.pid(), &["llama-server"])
        .into_keys()
        .find(|pid| {
            proc_list(*pid, "cmdline")
                .iter()
                .any(|a| a == &peer.to_string())
        })
        .unwrap_or_else(|| {
            panic!(
                "no llama-server under the head carries --rpc {peer}; router log:\n{}",
                log_tail(&head_data.join("llama-server/router.log"))
            )
        });
    eprintln!(
        "booted: peer rpc-server pid {peer_child} at {peer}; head split {chat_id} across it (model \
         process {model_pid}, preset names it on {distributed_sections} sections)"
    );

    // --- D2: decode at 90% or more of one device. The one-device baseline is
    // measured on the head's own card through a second router, so both
    // readings come from the same daemon build, the same model file and the
    // same prompt; a slow host moves both, and the ratio is what the bar is
    // about.
    let split_rate = head.decode_rate(&chat_id, &router_log);

    // --- SIGKILL the peer's child, watch /status name the loss, and the
    // model answer again once both halves restarted.
    //
    // The peer daemon is SIGSTOPped first, so its supervisor cannot restart
    // the child inside the window this asserts — a deterministic window,
    // not a race with a ~1s restart. The peer is a daemon the test started,
    // so stopping it breaks nothing else, and SIGCONT resumes it. Without
    // the STOP the child is back before the abort can be observed, which is
    // what a first run measured.
    let peer_daemon_pid = peer_daemon.pid();
    let _ = Command::new("kill")
        .args(["-STOP", &peer_daemon_pid.to_string()])
        .status();
    let before = rpc_server_under(peer_daemon_pid).expect("the peer's child");
    let _ = Command::new("kill")
        .args(["-9", &peer_child.to_string()])
        .status();
    eprintln!(
        "router rpc e2e: SIGSTOPped the peer daemon and SIGKILLed its rpc-server ({peer_child})"
    );

    // One request is what TOUCHES the dead peer: upstream never reconnects,
    // so the model child aborts on it (`ggml-rpc.cpp:647`). A request is
    // what the operator would make, and the abort it triggers is the row's
    // subject. It may answer an error; that is not the assertion — the
    // assertion is what /status then says.
    let touched = post_json(
        &format!("http://127.0.0.1:{}/v1/chat/completions", head.serve),
        serde_json::json!({
            "messages": [{ "role": "user", "content": "Say ready." }],
            "max_tokens": 8, "temperature": 0.0
        }),
    );
    eprintln!(
        "router rpc e2e: a request against the dead peer answered: {}",
        match &touched {
            Ok(_) => "ok".to_string(),
            Err(e) => format!("{e:.120}"),
        }
    );

    // The peer's supervisor restarts its child at the same address, once the
    // daemon is resumed.
    let _ = Command::new("kill")
        .args(["-CONT", &peer_daemon_pid.to_string()])
        .status();
    let mut after = None;
    wait_until(
        "the peer's supervisor starting another rpc-server",
        Duration::from_secs(120),
        &peer_daemon.log,
        || {
            // A connect is NOT the instrument (the head's model holds the
            // peer's one accept slot, `transport.cpp:677`): the restart is
            // a new pid under the peer daemon.
            after = rpc_server_under(peer_daemon_pid).filter(|c| *c != before);
            after.is_some()
        },
    );
    let restarted_peer = after.expect("the restarted peer child").0;
    eprintln!("router rpc e2e: the peer's child restarted as pid {restarted_peer}");

    // The head names the loss on the MODEL's line, not the router's: what
    // dies is the model child (measured: `ggml-rpc.cpp:647`, "Remote RPC
    // server crashed", `/models` `failed (exit 1)`), and the router — which
    // loads no tensors — keeps serving. `/status` must say which peer, not
    // only "failed".
    let lost_at = Instant::now();
    let mut lost_line = String::new();
    wait_until(
        "/status naming the peer loss on the distributed model's line",
        Duration::from_secs(240),
        &router_log,
        || {
            lost_line = head
                .children()
                .into_iter()
                .find(|c| c["role"] == "router-model" && c["name"] == chat_id.as_str())
                .and_then(|c| c["last_transition_reason"].as_str().map(str::to_string))
                .unwrap_or_default();
            lost_line.contains("peer loss") && lost_line.contains(&peer.to_string())
        },
    );
    eprintln!(
        "router rpc e2e: /status named the lost peer within {}s: {lost_line:?}",
        lost_at.elapsed().as_secs()
    );

    // ...and the model answers again, with no daemon restart: the router's
    // repair loop re-issues the load with backoff, and the peer's supervisor
    // has restarted its child.
    let recover_at = Instant::now();
    loop {
        if head.model_lifecycle(&chat_id) == "loaded" {
            break;
        }
        assert!(
            head.alive(),
            "the head exited while the model recovered:\n{}",
            log_tail(&head.log)
        );
        if recover_at.elapsed() > Duration::from_secs(900) {
            let line = head
                .children()
                .into_iter()
                .find(|c| c["role"] == "router-model" && c["name"] == chat_id.as_str())
                .unwrap_or_default();
            panic!(
                "never saw: the split model loaded again within 900s; /status says {line}; \
                 repair did not recover it. Router log {}:\n{}",
                router_log.display(),
                log_tail(&router_log)
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let recovered_rate = head.decode_rate(&chat_id, &router_log);
    eprintln!(
        "router rpc e2e: the split model answered {recovered_rate:.2} tok/s after both restarts"
    );

    // Never a silent fall back to one device: the recovered router's preset
    // is still the distributed one, and its model process still carries the
    // peer.
    let preset_after = std::fs::read_to_string(head_data.join("llama-server/router.ini"))
        .expect("preset after recovery");
    assert_eq!(
        preset_after.matches(&rpc_line).count(),
        1,
        "the recovered router serves an undistributed preset:\n{preset_after}"
    );
    assert!(
        preset.contains("rpc =") && preset_after == preset,
        "the recovered preset differs from the one this head was started with"
    );

    let base_root = root.path().join("base");
    let base_data = base_root.join("data");
    std::fs::create_dir_all(&base_data).expect("base data");
    let mut base = boot(
        &base_root,
        "base.stderr.log",
        Ports::reserve(loopback),
        |p| {
            format!(
                "[engine]\nkind = \"llama-server\"\n\n[models]\nprimary = \"{m}\"\n\
                 embed = \"{e}\"\ncontext_size = 4096\n\n[daemon]\n\
                 client_port = {svrn}\ninternal_port = {internal}\n\
                 rails_base = \"http://127.0.0.1:{dead_rails}\"\n\n[data]\ndir = \"{d}\"\n",
                m = chat.display(),
                e = models_dir.join(EMBED_MODEL).display(),
                d = base_data.display(),
                svrn = p.svrn,
                internal = p.internal,
                dead_rails = p.dead_rails,
            )
        },
    );
    let base_router_log = base_root.join("data/llama-server/router.log");
    wait_until(
        "the one-device baseline loading the same model",
        Duration::from_secs(900),
        &base_router_log,
        || {
            assert!(
                base.alive(),
                "the baseline exited during boot:\n{}",
                log_tail(&base.log)
            );
            base.router().is_some_and(|r| r["lifecycle"] == "serving")
                && base.model_lifecycle(&chat_id) == "loaded"
        },
    );
    let base_preset =
        std::fs::read_to_string(base_data.join("llama-server/router.ini")).expect("base preset");
    assert!(
        !base_preset.contains("rpc ="),
        "the one-device baseline was distributed:\n{base_preset}"
    );
    let base_rate = base.decode_rate(&chat_id, &base_router_log);
    let ratio = split_rate / base_rate;
    assert!(
        ratio >= D2_BAR,
        "D2 failed: the split decoded {split_rate:.2} tok/s against {base_rate:.2} on one device \
         ({ratio:.3} of it, bar {D2_BAR}); router log:\n{}",
        log_tail(&head_data.join("llama-server/router.log"))
    );
    eprintln!(
        "D2 passed: split {split_rate:.2} tok/s vs one device {base_rate:.2} tok/s ({ratio:.3}, \
         bar {D2_BAR}) over loopback {peer}"
    );

    // --- SIGKILL both daemons: nothing either started may outlive them.
    let mut started: BTreeMap<u32, u64> = BTreeMap::new();
    for d in [&head, &peer_daemon, &base] {
        started.extend(descendants(d.pid(), &["ggml-rpc-server", "llama-server"]));
    }
    for (name, d) in [
        ("head", &mut head),
        ("peer", &mut peer_daemon),
        ("base", &mut base),
    ] {
        d.process
            .0
            .kill()
            .unwrap_or_else(|e| panic!("SIGKILL {name}: {e}"));
        d.process.0.wait().expect("reap the daemon");
    }
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
        panic!("processes outlived the SIGKILLed daemons: {left:?} (killed now)");
    }
    assert!(
        started.len() >= 4,
        "expected the peer's rpc-server and each daemon's llama-servers, saw {started:?}"
    );
    eprintln!(
        "no orphans: all {} processes the three daemons started are gone",
        started.len()
    );
}
