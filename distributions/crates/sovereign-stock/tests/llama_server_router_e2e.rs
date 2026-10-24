// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[engine] kind = "llama-server"` end to end (ENGINE_SWAP_PLAN step 1,
//! slice e): the stock daemon boots on the shipped llama-server, verifies its
//! build, serves a completion and an embedding through the router, names it
//! in `/status`, reports the router's facts as its own (context size against
//! the server's slot, the code model, embed among the resident, an exact token
//! count from `/tokenize`), serves a FIM completion from its `[models.edit]`
//! section, reranks through `[models.kinds] rerank`'s section (the route
//! named the rerank model, the scores returned as margins), lets the embedder
//! sleep on its idle window and wakes it with a request, and when the daemon
//! is SIGKILLed no llama-server outlives it.
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
//! `SOVEREIGN_E2E_CHAT_MODEL`, `SOVEREIGN_E2E_EMBED_MODEL` and
//! `SOVEREIGN_E2E_RERANK_MODEL` pick the three models by role where the
//! defaults are not on the host (`ROLES`).
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

/// The chat and embedding models, by ROLE: any small GGUF that serves it
/// (quality/env-flags.toml `SOVEREIGN_E2E_CHAT_MODEL`,
/// `SOVEREIGN_E2E_EMBED_MODEL`), never a file name or digest the test
/// insists on, because the test is about the router's lifecycle, not about
/// what the model says. Unset, each is the smallest of its role in the
/// Halo's `sovereign/models/`.
const ROLES: [(&str, &str); 3] = [
    ("SOVEREIGN_E2E_CHAT_MODEL", "Qwen3.5-0.8B-UD-Q6_K_XL.gguf"),
    (
        "SOVEREIGN_E2E_EMBED_MODEL",
        "Qwen3-Embedding-0.6B-Q8_0.gguf",
    ),
    (
        "SOVEREIGN_E2E_RERANK_MODEL",
        "qwen3-reranker-0.6b-q8_0.gguf",
    ),
];

/// `[daemon] embed_idle_secs`: short enough to watch the embedder sleep,
/// long enough that it does not between the embedding and the residency
/// check that follows it.
const EMBED_IDLE_SECS: u64 = 20;

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

/// A GGUF the test loads, and the id the router serves it under (its file
/// stem).
struct Model {
    path: PathBuf,
    id: String,
}

/// The chat, embedding and rerank models (`ROLES`), each an absolute path or
/// a file in `SOVEREIGN_MODELS_DIR` (quality/env-flags.toml): never resolved
/// from the crate's own path, which escapes the package root.
fn models() -> (Model, Model, Model) {
    let dir = std::env::var_os("SOVEREIGN_MODELS_DIR")
        .map(PathBuf::from)
        .expect("SOVEREIGN_MODELS_DIR names the GGUF directory, absolute");
    let [chat, embed, rerank] = ROLES.map(|(var, default)| {
        let named = std::env::var_os(var);
        let path = dir.join(named.as_deref().unwrap_or(default.as_ref()));
        let source = named.as_ref().map_or("its default", |_| var);
        assert!(
            path.is_file(),
            "{} (from {source}) is not a file in {}",
            path.display(),
            dir.display()
        );
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a GGUF file name")
            .to_owned();
        eprintln!("router e2e: {var} = {} (from {source})", path.display());
        Model { path, id }
    });
    (chat, embed, rerank)
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
    let (chat, embed, rerank) = models();
    let shipped = Path::new(BIN).with_file_name("llama-server");
    assert!(
        shipped.is_file(),
        "no shipped llama-server at {}; build it with \
         serve/crates/sovereign-compute/llama-server/build.sh",
        shipped.display()
    );

    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir, data, mesh) = (
        root.path().join("home"),
        root.path().join("rails"),
        root.path().join("data"),
        root.path().join("svrnmesh"),
    );
    for d in [&home, &rails_dir, &data, &mesh] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (svrn, internal, serve, dead_rails) = (
        reserved_port(),
        reserved_port(),
        reserved_port(),
        reserved_port(),
    );
    // At the default path as well as `--config`: the notes rail builds its
    // KV from `SetupConfig::load()` (sovereign-code face/atlas.rs
    // `atlas_kv`), which without this file falls back to the host's
    // cw-rails on :9747 and embeds every note the mesh holds, keeping the
    // embedder awake through the idle check.
    let config = mesh.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[engine]\nkind = \"llama-server\"\n\n[models]\nprimary = \"{c}\"\n\
             code = \"{c}\"\nembed = \"{e}\"\ncontext_size = 4096\n\n\
             [models.edit]\npath = \"{c}\"\ntemperature = 0.0\n\n\
             [models.kinds]\nrerank = \"{r}\"\n\n[daemon]\n\
             client_port = {svrn}\ninternal_port = {internal}\nembed_idle_secs = {EMBED_IDLE_SECS}\n\
             rails_base = \"http://127.0.0.1:{dead_rails}\"\n\n[data]\ndir = \"{d}\"\n",
            c = chat.path.display(),
            e = embed.path.display(),
            r = rerank.path.display(),
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
            .env("SVRNMESH_DATA_DIR", &mesh)
            .env("CW_RAILS_DIR", &rails_dir)
            .env("CW_RAILS_BIN", root.path().join("no-cw-rails"))
            .env("SOVEREIGN_SERVE_PORT", serve.to_string())
            // Added to the daemon's allowlist (process.rs `compose_filter`),
            // which does not name the target of atlas_kv's fallback warn.
            .env("RUST_LOG", "sovereign_code=warn")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    let daemon = stock.0.id();

    // Boot: the router and both models report through /status. The embedder
    // may already have slept on its short window.
    let mut children = Vec::new();
    wait_until(
        "/status listing the router and all three models loaded (or the embedder asleep)",
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
                .filter(|c| {
                    c["role"] == "router-model"
                        && (c["lifecycle"] == "loaded" || c["lifecycle"] == "sleeping")
                })
                .count();
            loaded == 3
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
            "model": embed.id,
            "input": "hello router"
        }),
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
    let dims = embedded["data"][0]["embedding"]
        .as_array()
        .map_or(0, Vec::len);
    assert!(dims > 0, "the embedding is empty: {embedded}");

    // Rerank (ENGINE_SWAP_PLAN A4): `[models.kinds] rerank` is a section on
    // the router, the daemon's /v1/rerank answers it under the RERANK model's
    // id (not the chat model's), and the scores it hands back are margins
    // `ln(p/(1-p))` on the embedded engine's ruler: a document that answers
    // the query above 0, one that does not below it. Failing input: name the
    // chat id (the route does not know it) or return the probabilities
    // untouched (0.5 is not above the relevance par).
    let reranked = post_json(
        &format!("http://127.0.0.1:{serve}/v1/rerank"),
        serde_json::json!({
            "model": rerank.id,
            "query": "What is the capital of France?",
            "documents": [
                "Paris is the capital and largest city of France.",
                "The mitochondria is the powerhouse of the cell."
            ]
        }),
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
    let mut scores: Vec<f32> = reranked["results"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .map(|r| r["relevance_score"].as_f64().unwrap_or(f64::NAN) as f32)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(scores.len(), 2, "one score per document: {reranked}");
    assert!(
        scores.iter().all(|s| s.is_finite()),
        "a rerank score is not finite: {scores:?}"
    );

    // FIM (ENGINE_SWAP_PLAN A3): `[models.edit]` on the chat GGUF is the
    // edit slot, its style asked of the router's tokenizer, and a completion
    // through the daemon's /v1/completions is a raw prompt on the router's
    // /completion, ended by the family's stop strings.
    let status: Value = client()
        .get(format!("http://127.0.0.1:{svrn}/status"))
        .send()
        .and_then(|r| r.json())
        .expect("/status");
    let edit = &status["inference"]["edit"];
    let fim_style = edit["fim_style"]
        .as_str()
        .unwrap_or_else(|| panic!("the edit slot reports no FIM style: {edit}"))
        .to_string();
    assert_eq!(edit["model_id"], chat.id.as_str(), "{edit}");
    let fim = post_json(
        &format!("http://127.0.0.1:{svrn}/v1/completions"),
        serde_json::json!({
            // The F2 spike's prompt: a block opener, so multi-line mode.
            // After `return ` the 0.8B model's first token is a newline,
            // which single-line mode stops on with nothing emitted.
            "prefix": "def add(a, b):",
            "suffix": "\n\nprint(add(1, 2))\n",
            "debug": true
        }),
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
    let infill = fim["choices"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !infill.trim().is_empty(),
        "the FIM completion is empty: {fim}"
    );
    assert!(
        !infill.contains("<|") && !infill.contains("<think>"),
        "the FIM prompt was templated, or a marker leaked: {infill:?}"
    );

    // Host truth (ENGINE_SWAP_PLAN A1, A5): what the daemon reports about
    // its engine is what the router says, asked through the replay route the
    // conformance battery uses.
    let replay = |method: &str, input: Value| -> Value {
        let r = post_json(
            &format!("http://127.0.0.1:{svrn}/internal/engine/replay"),
            serde_json::json!({ "method": method, "input": input }),
        )
        .unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
        assert_eq!(r["outcome"]["kind"], "ok", "{method}: {r}");
        r["answer"].clone()
    };
    let (chat, embed) = (chat.id.as_str(), embed.id.as_str());
    let router_port = router["port"].as_u64().expect("the router's port");
    let host = replay("host", serde_json::json!({}));
    let props: Value = client()
        .get(format!("http://127.0.0.1:{router_port}/props?model={chat}"))
        .send()
        .and_then(|r| r.json())
        .expect("the router's /props for the chat model");
    let slot_ctx = props["default_generation_settings"]["n_ctx"].clone();
    assert_eq!(
        host["effective_context_size"], slot_ctx,
        "the context the daemon budgets against is not the server's slot: {host} / {props}"
    );
    // Host truth asks the provider the same, through the replay route: the
    // id the daemon would name is the rerank section's, and it is not the
    // chat model's.
    let named_model = replay("rerank_model_id", serde_json::json!({}));
    assert_eq!(
        named_model.as_str(),
        Some(rerank.id.as_str()),
        "the rerank model the daemon names: {named_model}"
    );
    let replay_scores = replay(
        "rerank_batch",
        serde_json::json!({
            "model": rerank.id,
            "query": "What is the capital of France?",
            "docs": [
                "Paris is the capital and largest city of France.",
                "The mitochondria is the powerhouse of the cell."
            ]
        }),
    );
    let replay_scores: Vec<f32> = serde_json::from_value(replay_scores).unwrap_or_default();
    assert_eq!(
        replay_scores.len(),
        2,
        "the replay route's rerank: {replay_scores:?}"
    );
    scores.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    assert!(
        scores[0] < 0.0 && scores[1] > 0.0,
        "the answering document is not above the relevance par (0) and the other below it: {scores:?}"
    );

    let trained = host["n_ctx_train"].as_u64().unwrap_or(0);
    assert!(
        trained >= slot_ctx.as_u64().unwrap_or(u64::MAX),
        "n_ctx_train {trained} is unread, or below the slot the server opened: {host}"
    );
    assert_eq!(
        host["code_model"], chat,
        "the configured code model: {host}"
    );
    // The embedding woke the embedder if it had slept; residency is the
    // router's `/models`, which the daemon re-reads every 5s.
    let resident_of = |host: &Value| -> Vec<String> {
        host["resident_models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let mut resident = resident_of(&host);
    wait_until(
        "residency listing both loaded models",
        Duration::from_secs(10),
        &log,
        || {
            resident = resident_of(&replay("host", serde_json::json!({})));
            resident.iter().any(|m| m == chat) && resident.iter().any(|m| m == embed)
        },
    );
    // A text whose chars/4 estimate is not its count, so an estimate passed
    // off as exact fails here.
    let text = "tokenizers split words, numbers like 1234567 and punctuation!?";
    let exact = replay("count_tokens_exact", serde_json::json!({ "text": text }));
    let tokenized = post_json(
        &format!("http://127.0.0.1:{router_port}/tokenize"),
        serde_json::json!({ "model": chat, "content": text }),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let truth = tokenized["tokens"].as_array().map_or(0, Vec::len) as u64;
    let estimate = replay("count_tokens", serde_json::json!({ "text": text }));
    assert_eq!(
        exact.as_u64(),
        Some(truth),
        "count_tokens_exact against /tokenize"
    );
    assert_ne!(
        estimate.as_u64(),
        Some(truth),
        "the text no longer tells the estimate apart"
    );

    // No reader of the config fell back to the host's cw-rails.
    let boot_log = std::fs::read_to_string(&log).expect("the daemon's log");
    assert!(
        !boot_log.contains("the rails base is the default"),
        "a component read no setup config and reached the host's cw-rails:\n{}",
        log_tail(&log)
    );

    // Idle (ENGINE_SWAP_PLAN A2): left past `embed_idle_secs`, the embedder
    // sleeps and /status says so; the next embedding wakes it.
    let lifecycle_of = |model: &str| -> String {
        client()
            .get(format!("http://127.0.0.1:{svrn}/status"))
            .send()
            .and_then(|r| r.json::<Value>())
            .ok()
            .and_then(|s| {
                s["inference"]["compute_children"]
                    .as_array()?
                    .iter()
                    .find(|c| c["role"] == "router-model" && c["name"] == model)
                    .map(|c| c["lifecycle"].as_str().unwrap_or_default().to_string())
            })
            .unwrap_or_default()
    };
    let idle_from = Instant::now();
    // The router logs every request it proxies to a model, so on a timeout
    // its log names what kept the embedder awake.
    wait_until(
        "/status reporting the idle embedder asleep",
        Duration::from_secs(EMBED_IDLE_SECS * 3 + 10),
        &data.join("llama-server/router.log"),
        || lifecycle_of(embed) == "sleeping",
    );
    let slept_after = idle_from.elapsed().as_secs();
    let woken = post_json(
        &format!("http://127.0.0.1:{serve}/v1/embeddings"),
        serde_json::json!({ "model": embed, "input": "wake up" }),
    )
    .unwrap_or_else(|e| {
        panic!(
            "the sleeping embedder did not answer: {e}\n{}",
            log_tail(&log)
        )
    });
    assert_eq!(
        woken["data"][0]["embedding"].as_array().map_or(0, Vec::len),
        dims,
        "the woken embedder's embedding: {woken}"
    );
    wait_until(
        "/status reporting the woken embedder loaded",
        Duration::from_secs(15),
        &log,
        || lifecycle_of(embed) == "loaded",
    );

    // The processes: the router and one child per loaded model (chat,
    // embedding, rerank), all under the daemon.
    let servers = llama_servers_under(daemon);
    assert_eq!(
        servers.len(),
        4,
        "expected the router and three model processes under the daemon, found {servers:?}"
    );
    eprintln!(
        "booted: {generated} tokens generated, {dims}-dim embedding, rerank {rerank_scores:?} \
         on {rerank}, ctx {slot_ctx} of \
         n_ctx_train {trained}, code {chat}, FIM {fim_style} {infill:?}, resident {resident:?}, \
         {truth} tokens exact \
         (estimate {estimate}), embedder slept within {slept_after}s of {EMBED_IDLE_SECS}s idle \
         and answered again, llama-servers {:?}",
        servers.keys(),
        rerank_scores = scores,
        rerank = rerank.id,
    );

    // One model process dies under the router (ENGINE_SWAP_PLAN A7): the
    // router OWNS the generate and embed children now, so a SIGKILLed model
    // is the router's to report and reload. The daemon must stay up, /status
    // must say the model is not serving, and the model must answer again
    // without a daemon restart.
    // The chat model's status through /status, the same view the idle check
    // reads (this one has to be declared before the kill below).
    let model_lifecycle = |model: &str| -> String {
        client()
            .get(format!("http://127.0.0.1:{svrn}/status"))
            .send()
            .and_then(|r| r.json::<Value>())
            .ok()
            .and_then(|s| {
                s["inference"]["compute_children"]
                    .as_array()?
                    .iter()
                    .find(|c| c["role"] == "router-model" && c["name"] == model)
                    .map(|c| c["lifecycle"].as_str().unwrap_or_default().to_string())
            })
            .unwrap_or_default()
    };
    let before = llama_servers_under(daemon);
    let chat_server = *before
        .keys()
        .find(|pid| {
            // The model process whose command line names the chat GGUF.
            std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|cmd| cmd.contains(&chat))
        })
        .unwrap_or_else(|| panic!("no llama-server process is serving {}: {before:?}", chat));
    let _ = Command::new("kill")
        .args(["-9", &chat_server.to_string()])
        .status();
    eprintln!("router e2e: SIGKILLed the chat model's process ({chat_server})");

    // Failing input is this very check: before the repair loop, llama.cpp
    // left a killed model `failed (exit -9)` for good and /status kept
    // reporting it. The status must name the death...
    wait_until(
        "/status and the router reporting the killed model as failed",
        Duration::from_secs(30),
        &data.join("llama-server/router.log"),
        || {
            let status = client()
                .get(format!("http://127.0.0.1:{svrn}/status"))
                .send()
                .and_then(|r| r.json::<Value>())
                .ok();
            let listed = status
                .as_ref()
                .and_then(|s| s["inference"]["compute_children"].as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .find(|c| c["role"] == "router-model" && c["name"] == chat)
                .unwrap_or_default();
            let router_failed = listed["lifecycle"]
                .as_str()
                .is_some_and(|l| l.starts_with("failed"));
            // The router's own `/models` is the source of truth for a killed
            // model; /status polls it every 5s.
            let direct: Value = client()
                .get(format!("http://127.0.0.1:{router_port}/models"))
                .send()
                .and_then(|r| r.json())
                .unwrap_or(Value::Null);
            let failed = direct["data"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|m| m["id"] == chat && m["status"]["failed"] == true);
            eprintln!(
                "router e2e: after the kill — /status {:?}, /models failed {failed}",
                listed["lifecycle"]
            );
            failed || router_failed
        },
    );

    // ...and then the repair: the router loads it again, and a completion
    // through the daemon answers, with no daemon restart.
    wait_until(
        "/status listing the reloaded chat model",
        Duration::from_secs(120),
        &data.join("llama-server/router.log"),
        || model_lifecycle(chat) == "loaded",
    );
    let after = post_json(
        &format!("http://127.0.0.1:{serve}/v1/chat/completions"),
        serde_json::json!({
            "messages": [{ "role": "user", "content": "Say the word ready." }],
            "max_tokens": 32,
            "temperature": 0.0
        }),
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
    let recovered = after["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    assert!(
        recovered > 0,
        "the reloaded chat model generated nothing: {after}"
    );
    eprintln!(
        "router e2e: the killed chat model was reloaded by the router and answered {recovered} tokens"
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
