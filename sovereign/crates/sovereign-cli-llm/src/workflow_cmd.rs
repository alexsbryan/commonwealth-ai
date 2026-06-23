// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign workflow` — run a user-authored `Step · Artifact · Runner`
//! workflow (model + MCP + tool + transform steps, authored as TOML).
//!
//! Assembles a *light* stack — daemon-routed inference (no per-process model
//! load), a minimal tool registry, and the MCP servers from `~/.sovereign/
//! config.toml` — then runs the workflow in-process over its source items.
//! P0+P1 of `docs/specs/WORKFLOW_SUBSTRATE.md`; durable/distributed execution
//! is P2 (the pipeline tool as an outer loop).

use std::sync::Arc;
use std::time::Duration;

use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_tools::shell::ShellTool;
use sovereign_tools::web::WebFetchTool;
use sovereign_workflow::{ArtifactCache, FileArtifactCache, NoCache, Runner, StepRegistry, Workflow};

const DEFAULT_DAEMON: &str = "http://localhost:9741";

pub async fn run_workflow(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    }
    match args[0].as_str() {
        "run" => cmd_run(&args[1..]).await,
        other => {
            eprintln!("Unknown workflow subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
    }
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "sovereign workflow",
    summary: "Run a Step·Artifact·Runner workflow — model, MCP, and tool steps authored as TOML.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "sovereign workflow run <file.toml> [--concurrency N] [--daemon <url>] [--no-cache]",
        ),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[(
            "run <file>",
            "Run a workflow over its source items (or once if it has no [source])",
        )]),
    ],
};

async fn cmd_run(args: &[String]) -> i32 {
    let mut file: Option<String> = None;
    let mut concurrency = 4usize;
    let mut daemon = DEFAULT_DAEMON.to_string();
    let mut no_cache = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-cache" => no_cache = true,
            "--concurrency" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) if n >= 1 => concurrency = n,
                    _ => {
                        eprintln!("--concurrency needs a positive number");
                        return 1;
                    }
                }
            }
            "--daemon" => {
                i += 1;
                match args.get(i) {
                    Some(u) => daemon = u.clone(),
                    None => {
                        eprintln!("--daemon needs a URL");
                        return 1;
                    }
                }
            }
            s if !s.starts_with('-') && file.is_none() => file = Some(s.to_string()),
            other => {
                eprintln!("Unknown argument: {other}");
                return 1;
            }
        }
        i += 1;
    }

    let Some(file) = file else {
        eprintln!("Usage: sovereign workflow run <file.toml>");
        return 1;
    };

    let toml = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read {file}: {e}");
            return 1;
        }
    };
    let wf = match Workflow::parse(&toml) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("workflow: {e}");
            return 1;
        }
    };

    // Inference: only assemble (and require) the daemon when a `model:` step is
    // present. Discovering the chat model doubles as the liveness probe.
    let v1 = format!("{}/v1", daemon.trim_end_matches('/'));
    let inference: Option<Arc<dyn InferenceProvider>> = if uses_model(&wf) {
        match discover_chat_model(&v1).await {
            Ok(model) => {
                eprintln!("Daemon: {daemon}  ·  model: {model}");
                Some(Arc::new(RemoteApiProvider::new(&v1, None, &model, 8192)))
            }
            Err(e) => {
                eprintln!(
                    "Daemon not reachable at {daemon} ({e}).\n\
                     A `model:` step needs it — start it with `sovereign daemon`."
                );
                return 1;
            }
        }
    } else {
        None
    };

    // Tools: cheap built-ins + MCP servers from the canonical config (same path
    // chat uses, so a server added via `sovereign mcp add` works here too).
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(WebFetchTool::new()));
    let mcp = sovereign_tools::mcp::load_from_setup_config(&mut tools).await;
    for st in mcp.server_statuses().await {
        if st.connected {
            eprintln!("MCP: {} ({} tools)", st.name, st.tool_count);
        } else if let Some(e) = &st.error {
            eprintln!("MCP: {} unavailable — {e}", st.name);
        }
    }

    // Content-addressed cache (default on): a re-run with unchanged inputs
    // skips Read-effect steps. `--no-cache` forces every step to run.
    let cache: Arc<dyn ArtifactCache> = if no_cache {
        Arc::new(NoCache)
    } else {
        Arc::new(FileArtifactCache::new(cache_dir()))
    };
    let registry = StepRegistry::new(inference, Arc::new(tools));
    let report = match Runner::with_cache(registry, cache)
        .run(&wf, concurrency)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("workflow run failed: {e}");
            return 1;
        }
    };

    eprintln!(
        "\n— {} — {} ok, {} failed · {} steps ran, {} cached —",
        report.workflow,
        report.ok_count(),
        report.failed_count(),
        report.ran_total(),
        report.cached_total()
    );
    for item in &report.items {
        match &item.result {
            Ok(text) => println!("\n## {}\n{}", item.item, text.trim()),
            Err(e) => eprintln!("✗ {}: {e}", item.item),
        }
    }
    i32::from(report.failed_count() > 0)
}

fn uses_model(wf: &Workflow) -> bool {
    wf.steps.iter().any(|s| s.uses.starts_with("model:"))
}

/// `~/.sovereign/workflow-cache` — alongside the canonical config (reuses its
/// home-dir resolution so we don't depend on `dirs` here).
fn cache_dir() -> std::path::PathBuf {
    sovereign_core::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("workflow-cache")
}

/// GET `<v1>/models`, return the first non-embedding model id. Doubles as the
/// daemon liveness probe (a connection error → a clear "start the daemon").
async fn discover_chat_model(v1: &str) -> std::result::Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("{v1}/models"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("/v1/models → HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let models = body
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| "malformed /v1/models response".to_string())?;
    let ids: Vec<&str> = models
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
        .collect();
    ids.iter()
        .find(|id| !id.to_lowercase().contains("embed"))
        .or_else(|| ids.first())
        .map(|s| s.to_string())
        .ok_or_else(|| "the daemon advertises no models".to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sovereign_core::registry::ToolRegistry;
    use sovereign_tools::mcp::config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
    use sovereign_tools::mcp::McpServerManager;
    use sovereign_workflow::{Runner, StepRegistry, Workflow};

    async fn spawn_demo() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                crate::mcp_demo_server::reference_mcp_router().into_make_service(),
            )
            .await;
        });
        format!("http://{addr}/mcp")
    }

    /// End to end, no daemon: a sealed file is read by a *real* MCP tool step
    /// over HTTP (`mcp:demo:read_memo`), then upper-cased by a `transform` step
    /// fed `{read.output}` — proving `mcp:` resolution + the runner threading an
    /// `Artifact` between steps through the production registry assembly.
    #[tokio::test]
    async fn mcp_read_threads_into_a_downstream_step() {
        let dir = tempfile::tempdir().unwrap();
        let memo = dir.path().join("note.md");
        let sealed = "ship the workflow substrate";
        std::fs::write(&memo, sealed).unwrap();

        let url = spawn_demo().await;
        let cfg = McpServerConfig {
            name: "demo".into(),
            description: None,
            enabled: true,
            transport: McpTransportConfig::Http {
                url,
                auth: McpAuthConfig::None,
            },
            global: true,
        };
        let mut tools = ToolRegistry::new();
        let _mgr = McpServerManager::from_config(std::slice::from_ref(&cfg), &mut tools).await;

        let toml = r#"
[workflow]
name = "read-upper"
[source]
type = "inline"
items = ["__PATH__"]
[[step]]
id = "read"
uses = "mcp:demo:read_memo"
params = { path = "{item.path}" }
[[step]]
id = "up"
uses = "transform:upper"
input = "{read.output}"
"#
        .replace("__PATH__", &memo.to_string_lossy());

        let wf = Workflow::parse(&toml).unwrap();
        let registry = StepRegistry::new(None, Arc::new(tools));
        let report = Runner::new(registry).run(&wf, 1).await.unwrap();

        assert_eq!(report.ok_count(), 1, "{:?}", report.items);
        let out = report.items[0].result.as_ref().unwrap();
        assert!(
            out.contains(&sealed.to_uppercase()),
            "MCP read must thread into the transform: {out}"
        );
    }

    async fn demo_registry(url: &str) -> StepRegistry {
        let cfg = McpServerConfig {
            name: "demo".into(),
            description: None,
            enabled: true,
            transport: McpTransportConfig::Http {
                url: url.to_string(),
                auth: McpAuthConfig::None,
            },
            global: true,
        };
        let mut tools = ToolRegistry::new();
        let _ = McpServerManager::from_config(std::slice::from_ref(&cfg), &mut tools).await;
        StepRegistry::new(None, Arc::new(tools))
    }

    /// The demonstrable payoff: a real `read (MCP) → upper (transform)` workflow
    /// runs once, is fully cached on an unchanged re-run, and re-runs when the
    /// source file changes — proving the content-addressed cache + fingerprint
    /// invalidation end to end.
    #[tokio::test]
    async fn cache_skips_unchanged_and_reruns_on_edit() {
        let dir = tempfile::tempdir().unwrap();
        let memo = dir.path().join("note.md");
        std::fs::write(&memo, "aaa").unwrap();
        let url = spawn_demo().await;
        let cache: Arc<dyn sovereign_workflow::ArtifactCache> =
            Arc::new(sovereign_workflow::FileArtifactCache::new(dir.path().join(".cache")));

        let toml = r#"
[workflow]
name = "read-upper"
[source]
type = "inline"
items = ["__PATH__"]
[[step]]
id = "read"
uses = "mcp:demo:read_memo"
params = { path = "{item.path}" }
[[step]]
id = "up"
uses = "transform:upper"
input = "{read.output}"
"#
        .replace("__PATH__", &memo.to_string_lossy());
        let wf = Workflow::parse(&toml).unwrap();

        // 1st run: both steps run.
        let r1 = sovereign_workflow::Runner::with_cache(demo_registry(&url).await, cache.clone())
            .run(&wf, 1)
            .await
            .unwrap();
        assert_eq!((r1.ran_total(), r1.cached_total()), (2, 0));
        assert!(r1.items[0].result.as_ref().unwrap().contains("AAA"));

        // 2nd run, unchanged file: both steps cached.
        let r2 = sovereign_workflow::Runner::with_cache(demo_registry(&url).await, cache.clone())
            .run(&wf, 1)
            .await
            .unwrap();
        assert_eq!((r2.ran_total(), r2.cached_total()), (0, 2), "unchanged -> fully cached");

        // Edit the file (different size -> fingerprint changes regardless of
        // mtime granularity) -> the read re-runs, and the transform with it.
        std::fs::write(&memo, "bbbbbb").unwrap();
        let r3 = sovereign_workflow::Runner::with_cache(demo_registry(&url).await, cache.clone())
            .run(&wf, 1)
            .await
            .unwrap();
        assert_eq!(r3.cached_total(), 0, "edited file -> re-runs, nothing cached");
        assert!(r3.items[0].result.as_ref().unwrap().contains("BBBBBB"));
    }
}

