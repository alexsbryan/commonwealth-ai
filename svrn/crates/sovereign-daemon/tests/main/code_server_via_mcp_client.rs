// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's turn reaches code's tools through the MCP client it already has
//! (pb-code-daemon-exit: "svrn's own turns reach code's tools through the
//! existing MCP client ... with code's server as an entry"; no new client).
//!
//! With code's server listed as an `[[mcp_servers]]` entry, the client
//! `sovereign_runtime_recipe` runs for a turn's tool set
//! (`McpServerManager::from_config`) registers code's `symbols` into the
//! registry the planner calls as `mcp_code_symbols`, and a call answers from
//! code's graph. Code's server is the BUILT `sovereign-cli-dev code mcp`,
//! found beside this test binary in the target dir: a missing one fails by
//! name, never skips. What a model would decide to call is not in this proof;
//! the tool it would call is.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use corpus_engine_scip::scip_graph::{ScipGraph, ScipSymbolRecord};
use serde_json::json;
use sovereign_contracts::mcp_config::{McpServerConfig, McpTransportConfig};
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::types::{StepOutput, ToolContext};
use sovereign_tools::mcp::McpServerManager;

/// Nothing listens on loopback ports 1 or 2: a dial is refused at once.
const DEAD_RAILS: &str = "http://127.0.0.1:1";
const DEAD_DAEMON: &str = "http://127.0.0.1:2";

/// The code server binary beside this test's (target/<profile>/).
fn code_server_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's path");
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>/deps/<test>");
    let bin = dir.join("sovereign-cli-dev");
    assert!(
        bin.is_file(),
        "the code server binary is not built beside this test: {} — build \
         sovereign-cli-dev (the code program) first",
        bin.display()
    );
    bin
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn a_turn_registry_reaches_codes_symbols_through_the_mcp_client() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, root, indexes) = (
        dir.path().join("repo"),
        dir.path().join("root"),
        dir.path().join("indexes"),
    );
    std::fs::create_dir_all(repo.join(".sovereign")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn fixture_target() {}
",
    )
    .unwrap();
    std::fs::create_dir_all(indexes.join("fixture")).unwrap();
    let mut config = SetupConfig::unconfigured();
    config.node.entry = Some(DEAD_DAEMON.to_string());
    config.daemon.rails_base = Some(DEAD_RAILS.to_string());
    config.save_to(&SetupConfig::path_in(&root)).unwrap();
    let graph = ScipGraph::open(&indexes.join("fixture/scip_graph.db"), "fixture").unwrap();
    graph
        .ingest_symbols_and_refs(
            vec![ScipSymbolRecord {
                name: "fixture_target".into(),
                qualified_name: "fixture src/lib.rs/fixture_target().".into(),
                kind: "function".into(),
                file_path: repo.join("src/lib.rs").display().to_string(),
                line_start: 0,
                line_end: 1,
                language: "rust".into(),
            }],
            vec![],
        )
        .await
        .unwrap();
    drop(graph);

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .unwrap()
        .port();
    let log = dir.path().join("code.log");
    let mut server = Server(
        Command::new(code_server_bin())
            .args(["code", "mcp", "--port", &port.to_string(), "--data-dir"])
            .arg(&indexes)
            .arg("--sovereign-dir")
            .arg(repo.join(".sovereign"))
            .current_dir(&repo)
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join(".config"))
            .env("SVRNMESH_DATA_DIR", &root)
            .env_remove("SOVEREIGN_DATA_DIR")
            .env("SVRNMESH_DAEMON_URL", DEAD_DAEMON)
            .env_remove("SOVEREIGN_DAEMON_URL")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("spawn sovereign-cli-dev code mcp"),
    );
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(Some(status)) = server.0.try_wait() {
            panic!(
                "code mcp exited {status}:\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        if reqwest::get(format!("{base}/mcp/stats"))
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "code mcp never served:\n{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let entry = McpServerConfig {
        name: "code".into(),
        description: None,
        enabled: true,
        transport: McpTransportConfig::Http {
            url: format!("{base}/mcp"),
            auth: Default::default(),
        },
        global: true,
    };
    let mut registry = sovereign_contracts::ToolRegistry::new();
    let _manager = McpServerManager::from_config(&[entry], &mut registry).await;
    let tool = registry.get("mcp_code_symbols").unwrap_or_else(|_| {
        panic!(
            "code's symbols is not in the turn registry: {:?}",
            registry
                .descriptors()
                .into_iter()
                .map(|d| d.id)
                .collect::<Vec<_>>()
        )
    });
    let out = tool
        .execute(
            &json!({ "name": "fixture_target" }),
            &ToolContext::default(),
        )
        .await;
    let text = match out {
        Ok(StepOutput::Text(t)) => t,
        other => format!("{other:?}"),
    };
    assert!(
        text.contains("fixture_target") && text.contains("lib.rs"),
        "code's symbols did not answer from its graph through the MCP client: {text}"
    );
    drop(server);
}
