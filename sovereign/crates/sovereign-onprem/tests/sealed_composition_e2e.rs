// SPDX-License-Identifier: AGPL-3.0-or-later
//! The on-prem binary composes none of the surfaces that could reach a shell,
//! the web or an arbitrary server-side path (phase-b-86, -87).
//!
//! Boots `sovereign-onprem` with the argv the sovereign-daemon binary takes
//! (`run --config <path>`), on a temp root with the model-free mock engine, an
//! API key on disk, cw-rails pinned to a closed port with no binary to bring
//! up, and every proxy variable pointed at a loopback listener that counts
//! connections (`NO_PROXY` covers loopback). Then:
//!
//! - the turn runtime's registry census (the boot's per-family report) names
//!   web, wikipedia and recipe-authoring withheld and `search` without its web
//!   fallback; no web_fetch, wikipedia_fetch or probe_url registers;
//! - `/mcp`, `/mcp/message`, `/mcp/stats` and `/v1/solve/jobs` answer a 503
//!   whose body names the absence (stock serves `/mcp`: its own e2e);
//! - a turn answers and nothing dialled the proxy;
//! - the binary carries no literal only sovereign-code or
//!   sovereign-recipe-author holds, and the stock binary carries each.
//!
//! Linux only, as the stock e2e is.
#![cfg(target_os = "linux")]

use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-onprem");
const KEY: &str = "onprem-key-000000000000000000000000000000000000000000000000000000";
/// IT's key: the mutating routes, `POST /mcp` among them, are the admin group's.
const ADMIN: &str = "onprem-admin-0000000000000000000000000000000000000000000000000000";

/// Literals only the crates on-prem does not compose carry: code's solve
/// events route (sovereign-code solve_http.rs) and `probe_url`'s worked
/// example (sovereign-recipe-author probe_url.rs). Tool ids are not here:
/// sovereign-contracts names them as routing data in every svrn build.
const NOT_COMPOSED: [&str; 2] = [
    "/v1/solve/jobs/{id}/events",
    "Confirm CourtListener v4 endpoint shape",
];

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

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .build()
        .expect("http client")
}

fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(40).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// A loopback listener standing in for every proxy: each connection is one
/// attempt to leave the box.
fn counting_proxy() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("proxy bind");
    let port = listener.local_addr().expect("proxy addr").port();
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&attempts);
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            seen.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 256];
            let _ = conn.read(&mut buf);
        }
    });
    (port, attempts)
}

fn config_text(root: &Path, client: u16, internal: u16, dead_rails: u16) -> String {
    format!(
        "[engine]\nkind = \"mock\"\n\n[models]\nprimary = \"{r}/mock.gguf\"\n\
         embed = \"{r}/mock-embed.gguf\"\n\n[daemon]\nclient_port = {client}\n\
         internal_port = {internal}\nrails_base = \"http://127.0.0.1:{dead_rails}\"\n\n\
         [data]\ndir = \"{r}/data\"\n",
        r = root.display()
    )
}

/// How many times `needle` occurs in the binary at `path`: `strings -a | grep
/// -c` by substring, without the external tools.
fn occurrences(path: &Path, needle: &str) -> usize {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    bytes
        .windows(needle.len())
        .filter(|w| *w == needle.as_bytes())
        .count()
}

#[test]
fn the_onprem_binary_composes_no_shell_web_or_mcp_surface() {
    let root = tempfile::tempdir().expect("tempdir");
    let (home, rails_dir, data) = (
        root.path().join("home"),
        root.path().join("rails"),
        root.path().join("data"),
    );
    for d in [&home, &rails_dir, &data] {
        std::fs::create_dir_all(d).expect("dir");
    }
    sovereign_daemon::client_tokens::keys::add_key(
        &sovereign_daemon::client_tokens::client_tokens_dir(&data),
        "lawyer",
        &[],
        KEY,
    )
    .expect("an API key on disk");
    sovereign_daemon::client_tokens::keys::add_key(
        &sovereign_daemon::client_tokens::client_tokens_dir(&data),
        "it",
        &[sovereign_daemon::client_tokens::KEY_ADMIN_GROUP.to_string()],
        ADMIN,
    )
    .expect("IT's admin key on disk");
    let (svrn, internal, serve, dead_rails) = (free_port(), free_port(), free_port(), free_port());
    let (proxy, attempts) = counting_proxy();
    let proxy_url = format!("http://127.0.0.1:{proxy}");
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        config_text(root.path(), svrn, internal, dead_rails),
    )
    .expect("config");
    let log = root.path().join("onprem.stderr.log");
    let mut onprem = Killed(
        Command::new(BIN)
            .args(["run", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .env("SVRNMESH_DATA_DIR", root.path().join("svrnmesh"))
            .env("CW_RAILS_DIR", &rails_dir)
            .env("CW_RAILS_BIN", root.path().join("no-cw-rails"))
            .env("SOVEREIGN_SERVE_PORT", serve.to_string())
            // The per-family registry report is the `runtime_recipe` target's.
            .env("RUST_LOG", "runtime_recipe=info")
            .env("HTTP_PROXY", &proxy_url)
            .env("HTTPS_PROXY", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("https_proxy", &proxy_url)
            .env("all_proxy", &proxy_url)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).expect("log")))
            .spawn()
            .expect("spawn sovereign-onprem"),
    );
    let get = |path: &str| {
        client()
            .get(format!("http://127.0.0.1:{svrn}{path}"))
            .bearer_auth(KEY)
            .send()
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    while !get("/status").is_ok_and(|r| r.status().is_success()) {
        assert!(
            onprem.0.try_wait().expect("try_wait").is_none(),
            "the on-prem process exited during boot:\n{}",
            log_tail(&log)
        );
        assert!(
            Instant::now() < deadline,
            "/status never answered:\n{}",
            log_tail(&log)
        );
        std::thread::sleep(Duration::from_millis(250));
    }

    // The registry census: every family reports, present or absent.
    let boot = std::fs::read_to_string(&log).expect("log");
    let family = |name: &str| {
        boot.lines()
            .find(|l| l.contains(&format!("Tools:       {name}: ")))
            .unwrap_or_else(|| panic!("no `{name}` family report:\n{}", log_tail(&log)))
            .to_string()
    };
    for name in ["web", "wikipedia", "recipe-authoring"] {
        let line = family(name);
        assert!(
            line.contains(&format!("{name}: 0 tools, withheld {name} (")),
            "{line}"
        );
    }
    assert!(
        family("wikipedia").contains("open-web surfaces"),
        "wikipedia is withheld by the posture, not by a missing ingest"
    );
    assert!(
        family("recipe-authoring").contains("without recipe authoring"),
        "{}",
        family("recipe-authoring")
    );
    assert!(
        family("core-turn").contains("withheld search:web-fallback ("),
        "search kept its web fallback: {}",
        family("core-turn")
    );
    assert!(
        boot.contains("the ingest program is composed in this process"),
        "ingest is composed on-prem:\n{}",
        log_tail(&log)
    );

    // svrn's `/mcp` and code's solve routes: a 503 naming the absence.
    for (path, names) in [
        ("/mcp", "does not serve MCP"),
        ("/mcp/message", "does not serve MCP"),
        ("/mcp/stats", "does not serve MCP"),
        ("/v1/solve/jobs", "code program"),
    ] {
        let resp = client()
            .post(format!("http://127.0.0.1:{svrn}{path}"))
            .bearer_auth(ADMIN)
            .json(&serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
            .send()
            .unwrap_or_else(|e| panic!("{path} did not answer: {e}"));
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        assert_eq!(status, 503, "{path}: {body}");
        assert!(
            body.contains(names),
            "{path} does not name the absence: {body}"
        );
    }

    // The granted reads (pb-distribution-onprem-routes): each answers under a
    // key and refuses without one; `/health` alone needs none.
    let window = "/v1/corpora/firm-docs/chunks/1";
    for path in ["/v1/corpora", "/v1/tools", window] {
        let resp = client()
            .get(format!("http://127.0.0.1:{svrn}{path}"))
            .send()
            .unwrap_or_else(|e| panic!("{path} did not answer: {e}"));
        assert_eq!(resp.status(), 401, "{path} without a key");
    }
    let resp = client()
        .get(format!("http://127.0.0.1:{svrn}/health"))
        .send()
        .expect("/health answers");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap_or_default(), "ok");
    let resp = get("/v1/corpora").expect("/v1/corpora answers");
    assert_eq!(resp.status(), 200);
    let corpora: serde_json::Value = resp.json().expect("corpora json");
    assert!(corpora["corpora"].is_array(), "{corpora}");
    // No `[retrieval] corpora` in this config: the grant is empty, so the
    // window refuses every corpus by name.
    let resp = get(window).expect("the reading window answers");
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("'firm-docs'"),
        "the refusal names the corpus: {body}"
    );
    // The hardening probe: the turn runtime holds no web, wikipedia or
    // recipe-authoring tool, and no tool the approval gate would stop on
    // (REST turns auto-approve, so such a tool would need an approve route).
    let resp = get("/v1/tools").expect("/v1/tools answers");
    assert_eq!(resp.status(), 200);
    let tools: serde_json::Value = resp.json().expect("tools json");
    let tools = tools["tools"].as_array().expect("a tools array");
    assert!(!tools.is_empty(), "the on-prem turn runtime holds tools");
    for t in tools {
        let id = t["id"].as_str().unwrap_or_default();
        assert!(
            !["web_fetch", "wikipedia_fetch", "probe_url"].contains(&id),
            "on-prem holds {id}"
        );
        assert_eq!(t["requires_approval"], false, "{t}");
    }

    // A turn over an empty corpus set answers on this box alone.
    let resp = client()
        .post(format!("http://127.0.0.1:{svrn}/v1/chat/completions"))
        .bearer_auth(ADMIN)
        .json(&serde_json::json!({ "messages": [{ "role": "user", "content": "What did the 2019 filing say?" }] }))
        .send()
        .expect("a turn answers");
    assert!(
        resp.status().is_success(),
        "turn: {}",
        resp.text().unwrap_or_default()
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "the on-prem process dialled off the box:\n{}",
        log_tail(&log)
    );
}

/// Literals only a non-composed crate holds: absent here, present in stock.
#[test]
fn the_onprem_binary_holds_no_literal_only_code_or_recipe_authoring_carries() {
    let stock = Path::new(BIN).with_file_name("sovereign-stock");
    assert!(
        stock.exists(),
        "{} is missing, so the strings check cannot be watched: build it with \
         `cargo build -p sovereign-stock`",
        stock.display()
    );
    for literal in NOT_COMPOSED {
        assert_eq!(
            occurrences(Path::new(BIN), literal),
            0,
            "on-prem holds {literal:?}"
        );
        assert!(
            occurrences(&stock, literal) > 0,
            "stock does not hold {literal:?}, so its absence on-prem proves nothing"
        );
    }
}
