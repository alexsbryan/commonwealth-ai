// SPDX-License-Identifier: AGPL-3.0-or-later
//! The stock install is ONE process (pb-stock-binary; phase-b-29 Q1, Q2).
//!
//! Boots the `sovereign-stock` binary the way `svrn daemon run` execs it (the
//! argv the sovereign-daemon binary takes: `run --config <path>`), on a temp
//! root with the model-free mock engine, cw-rails pinned to a closed port with
//! no binary to bring up, and a reserved `SOVEREIGN_SERVE_PORT`. Then:
//!
//! - `/status` says `serve (this process)`, and the serving decision reached
//!   the log (svrn's and serve's allowlists, unioned);
//! - a chat turn answers on svrn's port AND on serve's port, and serve's NER
//!   route answers within a bound (no `RemoteNer` dialing its own process);
//! - both `/v1/models` name the same local ids;
//! - no `sovereign-serve` process exists for this root;
//! - a reload through svrn with a changed config rebuilds ONE engine that both
//!   ports answer from. The mock engine names one model id whatever the config
//!   says, so the rebuild is counted in the log (one engine build at boot, one
//!   at reload), not read off a new id;
//! - SIGTERM leaves neither port answering.
//!
//! And, in its own boot, that code is composed on svrn's one `/mcp`
//! (pb-code-daemon-exit): see [`the_stock_install_serves_code_on_its_one_mcp`].
//!
//! Linux only: the process census reads `/proc`.
#![cfg(target_os = "linux")]

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-stock");

/// The mock engine's one build line (sovereign_compute::mock).
const ENGINE_BUILT: &str = "building the model-free mock engine";

/// Kills and reaps the process on every exit path, panics included.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A loopback port the child will bind, reserved rather than merely seen free.
/// Binding `:0` and dropping the listener left the port free for the seconds
/// until the child bound it, and a parallel test's `bind(:0)` took it in a
/// full-workspace run (note 3f7d5a40). Closing an accepted connection from the
/// listening side first parks the port in TIME_WAIT for 60s: the kernel hands
/// it to no other `bind(:0)` or `connect()` meanwhile, and the child's tokio
/// bind, which sets SO_REUSEADDR, still takes it. A port nothing binds (the
/// dead rails) refuses connections for that window too.
fn reserved_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
    let addr = listener.local_addr().expect("ephemeral port");
    let client = std::net::TcpStream::connect(addr).expect("connect to the reserved port");
    let (accepted, _) = listener.accept().expect("accept on the reserved port");
    drop(accepted);
    drop(client);
    addr.port()
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("http client")
}

fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(40).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Poll `done`; on timeout, panic with the log's tail — the tempdir is gone
/// by the time anyone reads the failure.
fn wait_until(what: &str, within: Duration, log: &Path, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(
            Instant::now() < deadline,
            "never saw: {what}, within {within:?}; last lines of {}:\n{}",
            log.display(),
            log_tail(log)
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn get_json(url: &str) -> Option<Value> {
    client().get(url).send().ok()?.json().ok()
}

/// A chat turn's answer text, or the failure named.
fn chat(port: u16) -> Result<String, String> {
    let body = serde_json::json!({ "messages": [{ "role": "user", "content": "hello" }] });
    let resp = client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .map_err(|e| format!("no answer on :{port}: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .map_err(|e| format!(":{port} answered HTTP {status} unreadably: {e}"))?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!(":{port} answered HTTP {status} with no content: {v}"))
}

/// The ids `/v1/models` says this node advertises (`advertised_by: local`).
/// `owned_by` differs by port by design: svrn's router says `mesh`.
fn local_models(port: u16) -> Vec<String> {
    let v = get_json(&format!("http://127.0.0.1:{port}/v1/models"))
        .unwrap_or_else(|| panic!("/v1/models on :{port} did not answer"));
    let mut ids: Vec<String> = v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("/v1/models on :{port} has no data: {v}"))
        .iter()
        .filter(|m| {
            m["advertised_by"]
                .as_array()
                .is_some_and(|by| by.iter().any(|b| b == "local"))
        })
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Pids whose command line runs a `sovereign-serve` binary for THIS root:
/// its environment carries the `SVRNMESH_DATA_DIR` the test handed the stock
/// process, which any child inherits. Host-wide, a full workspace run's other
/// tests' serves appeared here and failed a one-process install (auto-11).
fn serve_processes(data_dir: &Path) -> Vec<String> {
    let mut marker = b"SVRNMESH_DATA_DIR=".to_vec();
    marker.extend_from_slice(data_dir.as_os_str().as_encoded_bytes());
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().parse::<u32>().is_ok())
        .filter_map(|e| {
            let cmd = std::fs::read(e.path().join("cmdline")).ok()?;
            let argv0 = cmd.split(|b| *b == 0).next()?;
            let name = Path::new(std::str::from_utf8(argv0).ok()?).file_name()?;
            let env = std::fs::read(e.path().join("environ")).ok()?;
            (name == "sovereign-serve" && env.split(|b| *b == 0).any(|v| v == marker.as_slice()))
                .then(|| e.file_name().to_string_lossy().into_owned())
        })
        .collect()
}

fn config_text(root: &Path, client: u16, internal: u16, dead_rails: u16, primary: &str) -> String {
    format!(
        "[engine]\nkind = \"mock\"\n\n[models]\nprimary = \"{r}/{primary}\"\n\
         embed = \"{r}/mock-embed.gguf\"\n\n[daemon]\nclient_port = {client}\n\
         internal_port = {internal}\nrails_base = \"http://127.0.0.1:{dead_rails}\"\n\n\
         [data]\ndir = \"{r}/data\"\n",
        r = root.display()
    )
}

#[test]
fn the_stock_install_serves_both_ports_from_one_process_and_one_engine() {
    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir) = (root.path().join("home"), root.path().join("rails"));
    for d in [&home, &rails_dir, &root.path().join("data")] {
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
        config_text(root.path(), svrn, internal, dead_rails, "mock.gguf"),
    )
    .expect("config");
    let serve_before = serve_processes(&root.path().join("svrnmesh"));
    let log = root.path().join("stock.stderr.log");
    let mut stock = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", root.path().join("svrnmesh"))
            // cw-rails absent by construction: `rails_base` above names a
            // closed port, and there is no binary to bring up there.
            .env("CW_RAILS_DIR", &rails_dir)
            .env("CW_RAILS_BIN", root.path().join("no-cw-rails"))
            .env("SOVEREIGN_SERVE_PORT", serve.to_string())
            .env_remove("RUST_LOG")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );

    let mut serving = String::new();
    wait_until(
        "svrn's /status named its serving path",
        Duration::from_secs(120),
        &log,
        || {
            assert!(
                stock.0.try_wait().expect("try_wait").is_none(),
                "the stock process exited during boot:\n{}",
                log_tail(&log)
            );
            match get_json(&format!("http://127.0.0.1:{svrn}/status")) {
                Some(v) => {
                    serving = v["serving"].as_str().unwrap_or_default().to_string();
                    !serving.is_empty()
                }
                None => false,
            }
        },
    );
    assert_eq!(serving, "serve (this process)", "{}", log_tail(&log));
    let text = std::fs::read_to_string(&log).expect("log");
    assert!(
        text.contains("serving path decided"),
        "the serving decision did not reach the log:\n{}",
        log_tail(&log)
    );

    // Both ports answer a turn, from one process.
    for port in [svrn, serve] {
        let answer = chat(port).unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
        assert!(!answer.is_empty(), "an empty turn on :{port}");
    }
    // serve's NER route answers within a bound in this process: a NER handle
    // dialing its own `/v1/ner` would not.
    let ner = client()
        .post(format!("http://127.0.0.1:{serve}/v1/ner"))
        .timeout(Duration::from_secs(10))
        .json(&serde_json::json!({ "texts": ["Ada Lovelace met Babbage in London."], "labels": ["person"] }))
        .send();
    assert!(
        ner.is_ok(),
        "serve's /v1/ner did not answer within 10s: {ner:?}"
    );

    let (on_svrn, on_serve) = (local_models(svrn), local_models(serve));
    assert!(!on_serve.is_empty(), "serve names no local model");
    assert_eq!(
        on_svrn,
        on_serve,
        "the two ports name different models: svrn {} / serve {}",
        get_json(&format!("http://127.0.0.1:{svrn}/v1/models")).unwrap_or_default(),
        get_json(&format!("http://127.0.0.1:{serve}/v1/models")).unwrap_or_default()
    );
    assert_eq!(
        serve_processes(&root.path().join("svrnmesh")),
        serve_before,
        "a sovereign-serve process appeared: the stock install is one process"
    );
    let builds = |t: &str| t.matches(ENGINE_BUILT).count();
    assert_eq!(
        builds(&text),
        1,
        "boot built one engine:\n{}",
        log_tail(&log)
    );

    // A reload through svrn with a changed config: serve rebuilds ONE engine,
    // into the cell both ports answer from.
    std::fs::write(
        &config,
        config_text(root.path(), svrn, internal, dead_rails, "mock-v2.gguf"),
    )
    .expect("rewrite config");
    let reload = client()
        .post(format!("http://127.0.0.1:{svrn}/v1/admin/reload"))
        // The daemon's reload reads its default config path unless told
        // (admin_http.rs `ReloadRequest::config_path`); serve's reload route
        // re-reads the path the composition was handed, this same file.
        .json(&serde_json::json!({ "config_path": config }))
        .send()
        .expect("reload answered");
    let status = reload.status();
    let report: Value = reload.json().expect("reload report");
    assert!(
        status.is_success(),
        "reload refused: HTTP {status} {report}"
    );
    assert!(
        report["reloaded_fields"]
            .as_array()
            .is_some_and(|f| f.iter().any(|x| x == "models.primary")),
        "the changed primary did not reload: {report}"
    );
    let text = std::fs::read_to_string(&log).expect("log");
    assert_eq!(
        builds(&text),
        2,
        "boot and the reload each build one engine:\n{}",
        log_tail(&log)
    );
    for port in [svrn, serve] {
        let answer = chat(port).unwrap_or_else(|e| panic!("after reload: {e}\n{}", log_tail(&log)));
        assert!(!answer.is_empty(), "an empty turn on :{port} after reload");
    }
    assert_eq!(local_models(svrn), local_models(serve));

    // SIGTERM stops both programs: one process, one lifecycle.
    let pid = stock.0.id().to_string();
    assert!(Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("kill")
        .success());
    wait_until(
        "the stock process exited on SIGTERM",
        Duration::from_secs(60),
        &log,
        || stock.0.try_wait().expect("try_wait").is_some(),
    );
    for port in [svrn, serve] {
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
            ":{port} still answers after SIGTERM"
        );
    }
}

/// A config that opts into mesh-distributed inference is hosted too
/// (pb-serve-distributes): serve's assembly builds the one engine, and the
/// engine's mesh subsystems start beside it. Failing input: take
/// `ServingPath::DialsServe` out of `with_hosting`, and `/status` names
/// `serve`.
#[test]
fn a_distributed_config_is_hosted_and_starts_its_mesh_subsystems() {
    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir) = (root.path().join("home"), root.path().join("rails"));
    for d in [&home, &rails_dir, &root.path().join("data")] {
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
        config_text(root.path(), svrn, internal, dead_rails, "mock.gguf"),
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
            // A distributed opt-in the decider reads (phase-b-24): a manual
            // worker list. Discovery would need `[compute] distributed_primary`
            // past the containment guard, which the mock engine has no child for.
            .env("SOVEREIGN_RPC_WORKERS", "127.0.0.1:1")
            .env_remove("RUST_LOG")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    let mut serving = String::new();
    wait_until(
        "svrn's /status named its serving path",
        Duration::from_secs(120),
        &log,
        || {
            assert!(
                stock.0.try_wait().expect("try_wait").is_none(),
                "the stock process exited during boot:\n{}",
                log_tail(&log)
            );
            match get_json(&format!("http://127.0.0.1:{svrn}/status")) {
                Some(v) => {
                    serving = v["serving"].as_str().unwrap_or_default().to_string();
                    !serving.is_empty()
                }
                None => false,
            }
        },
    );
    assert_eq!(serving, "serve (this process)", "{}", log_tail(&log));
    let text = std::fs::read_to_string(&log).expect("log");
    assert!(
        text.contains("auto-warm orchestrator installed"),
        "the engine's mesh subsystems were not started beside the hosted engine:\n{}",
        log_tail(&log)
    );
    assert_eq!(
        text.matches(ENGINE_BUILT).count(),
        1,
        "boot built one engine:\n{}",
        log_tail(&log)
    );
    for port in [svrn, serve] {
        let answer = chat(port).unwrap_or_else(|e| panic!("{e}\n{}", log_tail(&log)));
        assert!(!answer.is_empty(), "an empty turn on :{port}");
    }
}

/// One JSON-RPC call on svrn's `/mcp`.
fn mcp(port: u16, id: u64, method: &str, params: Value) -> Value {
    client()
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .json(
            &serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )
        .send()
        .unwrap_or_else(|e| panic!("/mcp on :{port} did not answer: {e}"))
        .json()
        .expect("a JSON-RPC reply")
}

/// The stock binary composes the code program through code's face and
/// mounts it on svrn's one `:9741/mcp` (pb-code-daemon-exit; phase-b-33):
/// `symbols` is listed exactly once, beside svrn's own `wikipedia_fetch`, no
/// name is listed twice, code's `symbols` answers (not svrn's pointer to
/// `svrn code mcp`), `work_in_flight` is code's and answers, and
/// `/v1/projects` is code's router. Its boot log says ingest's
/// enrichment-config port was composed (pb-ingest-dial-tools-close). Code's
/// editor door answers on the client port with this binary's grammar lookup
/// (pb-meshapp-rest).
#[test]
fn the_stock_install_serves_code_on_its_one_mcp() {
    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir) = (root.path().join("home"), root.path().join("rails"));
    for d in [&home, &rails_dir, &root.path().join("data")] {
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
        config_text(root.path(), svrn, internal, dead_rails, "mock.gguf"),
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
            .env_remove("SOVEREIGN_WORKSPACE_DIR")
            // The per-family registry report is the `runtime_recipe` target's.
            .env("RUST_LOG", "runtime_recipe=info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-stock"),
    );
    wait_until(
        "svrn's /status answered",
        Duration::from_secs(120),
        &log,
        || {
            assert!(
                stock.0.try_wait().expect("try_wait").is_none(),
                "the stock process exited during boot:
{}",
                log_tail(&log)
            );
            get_json(&format!("http://127.0.0.1:{svrn}/status")).is_some()
        },
    );

    // Ingest is composed into svrn in this process, its engine and its
    // enrichment-config port (pb-ingest-dial-tools-close,
    // pb-ingest-dial-daemon); the bare daemon logs its absence instead.
    let boot = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        boot.contains("the ingest program is composed in this process"),
        "the stock process did not compose ingest's port:\n{}",
        log_tail(&log)
    );
    // svrn's open posture (phase-b-87): the turn registry holds web reach
    // and search's web fallback; the on-prem binary's census is the inverse.
    for (family, want) in [("web", "web: 1 tools"), ("core-turn", "core-turn: ")] {
        let line = boot
            .lines()
            .find(|l| l.contains(&format!("Tools:       {family}: ")))
            .unwrap_or_else(|| panic!("no `{family}` family report:\n{}", log_tail(&log)));
        assert!(line.contains(want), "{line}");
        assert!(!line.contains("web-fallback"), "{line}");
    }

    let list = mcp(svrn, 1, "tools/list", serde_json::json!({}));
    let names: Vec<String> = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list has no tools: {list}"))
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        names.len(),
        "a tool is listed twice: {names:?}"
    );
    assert_eq!(
        names.iter().filter(|n| *n == "symbols").count(),
        1,
        "code's symbols is not on the one /mcp exactly once: {names:?}
{}",
        log_tail(&log)
    );
    for name in ["wikipedia_fetch", "work_in_flight"] {
        assert!(names.iter().any(|n| n == name), "{name} missing: {names:?}");
    }

    let call = mcp(
        svrn,
        2,
        "tools/call",
        serde_json::json!({ "name": "symbols", "arguments": { "name": "main" } }),
    );
    assert!(
        call.get("error").is_none(),
        "code's symbols did not answer: {call}"
    );
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !text.is_empty() && !text.contains("svrn serves no code tool"),
        "symbols answered with svrn's pointer, not code's tool: {call}"
    );
    let atlas = mcp(
        svrn,
        3,
        "tools/call",
        serde_json::json!({ "name": "work_in_flight", "arguments": {} }),
    );
    assert!(
        atlas.get("error").is_none(),
        "work_in_flight did not answer: {atlas}"
    );

    let projects = client()
        .get(format!("http://127.0.0.1:{svrn}/v1/projects"))
        .send()
        .expect("/v1/projects answered");
    assert!(
        projects.status().is_success(),
        "/v1/projects is not code's router: HTTP {}",
        projects.status()
    );

    // Code's editor door on svrn's client port, with the grammar lookup this
    // binary supplies from corpus-engine's registry (pb-meshapp-rest): in a
    // `.tsx` buffer the `console.log` inside a comment is not a site, so one
    // edit is proposed, and the debug block names nothing unjudged. Without
    // the lookup both sites survive (sovereign-code tests/edit_door_grammar.rs).
    let unit = serde_json::json!({
        "before": "log", "after": "debug",
        "left": "  console.", "right": "(\"x\");"
    });
    let text = "console.debug(1);\nconsole.debug(2);\nconsole.log(3);\n// console.log(4);\n";
    let nes: Value = client()
        .post(format!("http://127.0.0.1:{svrn}/v1/edit_predictions"))
        .json(&serde_json::json!({
            "history": [unit, unit], "text": text, "cursor": 0,
            "path": "/work/src/App.tsx", "debug": true,
        }))
        .send()
        .expect("/v1/edit_predictions answered")
        .json()
        .expect("an edit prediction");
    assert_eq!(nes["object"], "edit_prediction", "{nes}");
    assert_eq!(
        nes["edits"].as_array().map(Vec::len),
        Some(1),
        "the stock door did not run the syntax filter on .tsx: {nes}"
    );
    assert!(
        nes["sovereign_debug"].get("syntax_filter").is_none(),
        "the stock door named its syntax filter unjudged: {nes}"
    );
}

/// The stock binary answers `vram-plan` with a pointer to svrn's verb, which
/// execs serve's one body (pb-serve-distributes deleted this binary's copy;
/// principle 8, 2 -> 1; pb-distribution-setup moved the body to serve).
/// Failing input: restore the daemon's `vram_plan::run` arm, and it sizes a
/// loadout (or prints its usage) instead of naming `svrn daemon vram-plan`.
#[test]
fn vram_plan_on_the_stock_binary_names_svrns_verb() {
    let out = Command::new(BIN)
        .arg("vram-plan")
        .output()
        .expect("run sovereign-stock vram-plan");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("svrn daemon vram-plan"),
        "no pointer to svrn's verb: {stderr}"
    );
}
