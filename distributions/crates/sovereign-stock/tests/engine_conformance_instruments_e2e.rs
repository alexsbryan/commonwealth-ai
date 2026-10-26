// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine conformance battery's new instruments, proved end to end: the
//! four I3 rows and `host.lifecycle` (L I3, I4) each reach a real verdict —
//! passed or failed, never could-not-judge or never-ran — on a battery run
//! against daemons and a router THIS TEST BOOTS ITSELF, on small GGUFs as
//! `llama_server_router_e2e` does. It never touches the deployed daemon.
//!
//! Three targets, each its own daemon on a temp root:
//! - `embedded`: `kind = "llama"`, the reference the embed row compares to;
//! - `llama-server`: `kind = "llama-server"`, the daemon's supervised router,
//!   `embed_inputs = "client"` — where the stream, admission and lifecycle
//!   rows are judged;
//! - `llama-server-default-embed`: the same router with `embed_inputs` left at
//!   its default ("server"), `compute.embeddings-default-inputs`' target.
//!
//! The driver is the shipped binary `bench/lanes/engine-swap/battery.sh`
//! execs (`sovereign-cli bench engine-conformance run`), so the verdict comes
//! from the code a seat's run will use.
//!
//! Ignored in the suite: it loads real GGUFs and boots servers.
//!
//! ```text
//! SOVEREIGN_MODELS_DIR=$PWD/sovereign/models cargo test -p sovereign-stock \
//!   --test engine_conformance_instruments_e2e -- --ignored --nocapture
//! ```
//!
//! Linux only: the process census reads `/proc`.
#![cfg(target_os = "linux")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-stock");

const CHAT_DEFAULT: &str = "Qwen3.5-0.8B-UD-Q6_K_XL.gguf";
const EMBED_DEFAULT: &str = "Qwen3-Embedding-0.6B-Q8_0.gguf";

/// The rows this test proves reached a verdict (L I3, I4).
const ROWS: [&str; 5] = [
    "projection.stream-error",
    "projection.stream-split-line",
    "cost.admission",
    "compute.embeddings-default-inputs",
    "host.lifecycle",
];

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn reserved_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
    let addr = listener.local_addr().expect("ephemeral port");
    let client = std::net::TcpStream::connect(addr).expect("connect to the reserved port");
    let (accepted, _) = listener.accept().expect("accept on the reserved port");
    drop(accepted);
    drop(client);
    addr.port()
}

fn model(var: &str, default: &str) -> (PathBuf, String) {
    let dir = std::env::var_os("SOVEREIGN_MODELS_DIR")
        .map(PathBuf::from)
        .expect("SOVEREIGN_MODELS_DIR names the GGUF directory, absolute");
    let named = std::env::var_os(var);
    let path = dir.join(named.as_deref().unwrap_or(default.as_ref()));
    assert!(path.is_file(), "{} is missing", path.display());
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("a GGUF file name")
        .to_owned();
    (path, id)
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

fn get(url: &str) -> Option<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    client.get(url).send().ok()?.json().ok()
}

/// One booted daemon: its client port, its log, and — for a router target —
/// the port its supervised router listens on (from `/status`).
struct Booted {
    _stock: Killed,
    daemon_url: String,
    /// The router's base URL, when the daemon supervises one.
    router: Option<String>,
}

/// Boot `sovereign-stock` on `engine_block` (the `[engine]` table, written
/// verbatim) and wait for it to serve; a router target also waits for the
/// router's port in `/status`.
fn boot(root: &Path, engine_block: &str) -> Booted {
    let (chat, embed) = {
        let (c, _) = model("SOVEREIGN_E2E_CHAT_MODEL", CHAT_DEFAULT);
        let (e, _) = model("SOVEREIGN_E2E_EMBED_MODEL", EMBED_DEFAULT);
        (c, e)
    };
    let (home, rails, data, mesh) = (
        root.join("home"),
        root.join("rails"),
        root.join("data"),
        root.join("svrnmesh"),
    );
    for d in [&home, &rails, &data, &mesh] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (svrn, internal, serve, dead_rails) = (
        reserved_port(),
        reserved_port(),
        reserved_port(),
        reserved_port(),
    );
    let config = mesh.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "{engine_block}\n\
             [models]\nprimary = \"{c}\"\nfast = \"{c}\"\nembed = \"{e}\"\ncontext_size = 4096\n\n\
             [daemon]\nclient_port = {svrn}\ninternal_port = {internal}\n\
             embed_idle_secs = 3600\narc_base = \"http://127.0.0.1:{dead_rails}\"\n\
             rails_base = \"http://127.0.0.1:{dead_rails}\"\n\n[data]\ndir = \"{d}\"\n",
            c = chat.display(),
            e = embed.display(),
            d = data.display()
        ),
    )
    .expect("config");
    let log = root.join("stock.stderr.log");
    let mut stock = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", &mesh)
            .env("CW_RAILS_DIR", &rails)
            .env("CW_RAILS_BIN", root.join("no-cw-rails"))
            .env("SOVEREIGN_SERVE_PORT", serve.to_string())
            .env("RUST_LOG", "sovereign_code=warn")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    let daemon_url = format!("http://127.0.0.1:{svrn}");
    let status_url = format!("{daemon_url}/status");
    let router_target = engine_block.contains("llama-server");
    let mut router = None;
    wait_until(
        "the daemon serving (and, for a router target, the router's port)",
        Duration::from_secs(300),
        &log,
        || {
            let alive = stock.0.try_wait().expect("try_wait").is_none();
            if !alive {
                return false;
            }
            let Some(status) = get(&status_url) else {
                return false;
            };
            let loaded = status["inference"]["resident"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|s| s["resident"] == true)
                .count();
            if !router_target {
                return loaded >= 1;
            }
            let port = status["inference"]["compute_children"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|c| c["role"] == "router")
                .and_then(|c| c["port"].as_u64());
            let models = status["inference"]["compute_children"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["role"] == "router-model" && c["lifecycle"] == "loaded")
                .count();
            if let Some(port) = port.filter(|_| models >= 2) {
                router = Some(format!("http://127.0.0.1:{port}"));
                return true;
            }
            false
        },
    );
    assert!(
        stock.0.try_wait().expect("try_wait").is_none(),
        "the daemon exited during boot:\n{}",
        log_tail(&log)
    );
    let pid = stock.0.id();
    eprintln!("booted: daemon {pid} at {} router {:?}", daemon_url, router);
    Booted {
        _stock: stock,
        daemon_url,
        router,
    }
}

/// Run the shipped driver for one target over the cases that carry one of
/// `ROWS`, writing `<out>.jsonl`. Returns the records path.
fn run_rows(cli: &Path, booted: &Booted, target: &str, bank: &Path, only: &[String], out: &Path) {
    let mut cmd = Command::new(cli);
    cmd.args(["bench", "engine-conformance", "run"])
        .args(["--target", target])
        .args(["--daemon", &booted.daemon_url])
        .args(["--cases", bank.to_str().unwrap()])
        .args(["--out", out.to_str().unwrap()])
        .args(only)
        .env("SOVEREIGN_NO_STALE_WARN", "1");
    if let Some(router) = &booted.router {
        cmd.args(["--server", router]);
    }
    let ran = cmd.output().expect("the driver runs");
    assert!(
        ran.status.success() || ran.status.code() == Some(1),
        "the driver failed for {target}:\n{}\n{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let records: Vec<Value> = std::fs::read_to_string(out)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a record line"))
        .collect();
    assert!(
        !records.is_empty(),
        "the driver recorded nothing for {target}:\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    eprintln!("{target}: {} records", records.len());
}

#[test]
#[ignore = "loads real GGUFs and boots routers: see the module doc"]
fn the_new_instruments_reach_a_verdict_on_a_daemon_and_router_the_test_boots() {
    let shipped = Path::new(BIN).with_file_name("llama-server");
    assert!(
        shipped.is_file(),
        "no shipped llama-server at {}; build it with \
         serve/crates/sovereign-compute/llama-server/build.sh",
        shipped.display()
    );
    // The driver binary `battery.sh` execs. Build it with the row's own gate.
    let cli = Path::new(BIN).with_file_name("sovereign-cli");
    assert!(
        cli.is_file(),
        "no sovereign-cli at {}; build it with cargo build --bins -p sovereign-cli \
         --features sovereign-cli/dev-tools",
        cli.display()
    );

    // The case bank, built by `cases.py` — the ONE place a case id is
    // computed — then filtered to the ids this test's rows carry (ARCH 8).
    let repo = Path::new(BIN)
        .ancestors()
        .nth(3)
        .expect("the repo root from target/debug")
        .to_path_buf();
    let root = tempfile::tempdir().expect("tempdir");
    let full = root.path().join("bank.jsonl");
    let built = Command::new("python3")
        .arg(repo.join("bench/lanes/engine-swap/cases.py"))
        .arg("build")
        .arg("--synthetic")
        .arg(repo.join("bench/lanes/engine-swap/cases/synthetic.jsonl"))
        .arg("--out")
        .arg(&full)
        .output()
        .expect("cases.py runs");
    assert!(
        built.status.success(),
        "cases.py failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let bank: Vec<Value> = std::fs::read_to_string(&full)
        .expect("the built bank")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a bank line"))
        .collect();
    let carries = |case: &Value, rows: &[&str]| {
        case["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r.as_str().is_some_and(|r| rows.contains(&r)))
    };
    let ids = |rows: &[&str]| -> Vec<String> {
        let mut only = Vec::new();
        for case in &bank {
            if carries(case, rows) {
                only.push("--only".to_string());
                only.push(case["case_id"].as_str().expect("a case id").to_string());
            }
        }
        only
    };
    // Every row this test proves has a case, or the filter proves nothing.
    for row in ROWS {
        assert!(
            bank.iter().any(|c| carries(c, &[row])),
            "no case in the bank exercises {row}"
        );
    }

    // 1. The router with client-prepared embed input: the stream, admission
    //    and lifecycle rows.
    let router_client = boot(
        &root.path().join("router-client"),
        "[engine]\nkind = \"llama-server\"\nembed_inputs = \"client\"\ngrammar = \"llguidance\"\ncontext_size = 4096",
    );
    let router_rows: [&str; 4] = [
        "projection.stream-error",
        "projection.stream-split-line",
        "cost.admission",
        "host.lifecycle",
    ];
    let router_records = root.path().join("router.jsonl");
    run_rows(
        &cli,
        &router_client,
        "llama-server",
        &full,
        &ids(&router_rows),
        &router_records,
    );
    drop(router_client);

    // 2. The embedded engine: the reference for the embed row.
    let embedded = boot(&root.path().join("embedded"), "[engine]\nkind = \"llama\"");
    let embed_rows: [&str; 1] = ["compute.embeddings-default-inputs"];
    let embedded_records = root.path().join("embedded.jsonl");
    run_rows(
        &cli,
        &embedded,
        "embedded",
        &full,
        &ids(&embed_rows),
        &embedded_records,
    );
    drop(embedded);

    // 3. The router with the embed_inputs default the row names.
    let default_embed = boot(
        &root.path().join("router-default-embed"),
        "[engine]\nkind = \"llama-server\"\nembed_inputs = \"server\"\ngrammar = \"llguidance\"\ncontext_size = 4096",
    );
    let default_embed_records = root.path().join("default-embed.jsonl");
    run_rows(
        &cli,
        &default_embed,
        "llama-server-default-embed",
        &full,
        &ids(&embed_rows),
        &default_embed_records,
    );
    drop(default_embed);

    // Judge all three record sets against the shipped inventory.
    let verdicts = root.path().join("verdicts");
    let judged = Command::new(&cli)
        .args(["bench", "engine-conformance", "judge"])
        .args([
            "--inventory",
            repo.join("bench/lanes/engine-swap/conformance.toml")
                .to_str()
                .unwrap(),
        ])
        .args(["--records", router_records.to_str().unwrap()])
        .args(["--records", embedded_records.to_str().unwrap()])
        .args(["--records", default_embed_records.to_str().unwrap()])
        .args(["--out", verdicts.to_str().unwrap()])
        .env("SOVEREIGN_NO_STALE_WARN", "1")
        .output()
        .expect("the judge runs");
    let stdout = String::from_utf8_lossy(&judged.stdout).to_string();
    let stderr = String::from_utf8_lossy(&judged.stderr).to_string();
    eprintln!("judge:\n{stdout}\n{stderr}");
    let table = std::fs::read_to_string(verdicts.join("verdicts.jsonl")).unwrap_or_default();
    let verdict_lines: Vec<Value> = table
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a verdict line"))
        .collect();
    for row in ROWS {
        let found: Vec<&Value> = verdict_lines.iter().filter(|v| v["row"] == row).collect();
        assert!(
            !found.is_empty(),
            "row {row} has no verdict — its instrument did not run:\n{table}"
        );
        // Every pair of the row must reach a verdict; the row's OWN target is
        // the one this test proves.
        let verdict = found
            .iter()
            .find(|v| {
                v["target"]
                    .as_str()
                    .is_some_and(|t| t != "embedded" || row.starts_with("compute"))
            })
            .map(|v| v["verdict"].as_str().unwrap_or_default())
            .unwrap_or_default();
        assert!(
            verdict == "passed" || verdict == "failed",
            "row {row} is `{verdict}`, which claims nothing:\n{table}"
        );
    }

    // The embed instrument must be able to see what the row is about. On
    // this host both sides prepare the input, so the pair's lowest cosine is
    // reported for the seat's table; what the test asserts is that the
    // instrument is live — each side embeds the same text two ways (embed,
    // embed_query) and the vectors move.
    {
        let read = |p: &Path| -> Vec<Value> {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).expect("a record line"))
                .collect()
        };
        let cos = |x: &[f64], y: &[f64]| -> f64 {
            let dot: f64 = x.iter().zip(y).map(|(a, b)| a * b).sum();
            let n = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
            dot / (n(x) * n(y))
        };
        let vectors = |r: &Value| -> Option<Vec<f64>> {
            Some(
                r["facets"]["embeddings"]
                    .as_array()?
                    .first()?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_f64)
                    .collect(),
            )
        };
        let embedded = read(&embedded_records);
        let default_embed = read(&default_embed_records);
        for (name, records) in [("embedded", &embedded), ("default-embed", &default_embed)] {
            let vs: Vec<Vec<f64>> = records.iter().filter_map(vectors).collect();
            if vs.len() == 2 {
                let c = cos(&vs[0], &vs[1]);
                eprintln!("embed instrument: {name} embed vs embed_query cosine {c:.4}");
                assert!(
                    (c - 1.0).abs() > 1e-3,
                    "{name} embeds its two texts identically — the embed path is not live"
                );
            }
        }
        if let (Some(a), Some(b)) = (
            embedded.first().and_then(vectors),
            default_embed.first().and_then(vectors),
        ) {
            eprintln!(
                "embed instrument: embedded vs default-embed (first case) cosine {:.6}",
                cos(&a, &b)
            );
        }
    }
}
