// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-workflow-host` — the daemon-runnable workflow host.
//!
//! The `sovereign-workflow` engine is core-only (Step·Artifact·Runner over
//! `sovereign-core` traits); it doesn't know about concrete tools or inference.
//! Running a workflow needs that *assembly*: the standard tool registry
//! (`sovereign-tools` built-ins + the MCP servers from `~/.sovereign/config.toml`),
//! daemon-routed inference (a `SplitInferenceProvider`), and the content cache.
//!
//! That assembly used to live inside the CLI (`workflow_cmd::run_assembled`), so
//! only the CLI could run a workflow. This crate hoists it to a place the **daemon**
//! can depend on (it depends on `sovereign-tools` already, but not `sovereign-cli-llm`),
//! which is the prerequisite for the living trigger (a watched folder running a
//! workflow on its own). The CLI now calls [`run_workflow_in_process`] and adds its
//! own presentation; the daemon calls it headless.
//!
//! Dependency shape: `workflow-host → {sovereign-workflow, sovereign-tools,
//! sovereign-inference, sovereign-core}`. No cycle (workflow is core-only; tools and
//! inference don't depend on workflow).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::{InferenceProvider, Tool};
use sovereign_core::types::{Effect, Permission};
use sovereign_inference::remote::SplitInferenceProvider;
use sovereign_workflow::{
    ArtifactCache, FileArtifactCache, NoCache, ResourceNeed, RunReport, Runner, StepKind,
    StepRegistry, Workflow,
};

pub mod trigger;
pub use trigger::DaemonWorkflowRuntime;

// ── Catalog ─────────────────────────────────────────────────────────────
// Shipped starters + the user's own (`~/.sovereign/workflows/`). Shared by the
// CLI (`workflow list/copy/new/run <name>`) and the daemon trigger runtime, which
// resolves a watched folder's `run_on_changes` workflow name the same way.

/// Shipped starter workflows, embedded at compile time.
pub const SHIPPED_WORKFLOWS: &[(&str, &str)] = &[
    (
        "notebook",
        include_str!("../../sovereign-workflow/recipes/notebook.toml"),
    ),
    (
        "summarize",
        include_str!("../../sovereign-workflow/recipes/summarize.toml"),
    ),
    (
        "web-digest",
        include_str!("../../sovereign-workflow/recipes/web-digest.toml"),
    ),
    (
        "meeting-to-done",
        include_str!("../../sovereign-workflow/recipes/meeting-to-done.toml"),
    ),
];

/// `~/.sovereign/workflows` — user-owned, editable workflows (the `copy`/`new`
/// target). Resolved from the same home-dir as the setup config + workflow cache.
pub fn workflows_dir() -> std::path::PathBuf {
    sovereign_core::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("workflows")
}

/// Resolve a workflow reference to `(toml, origin)`: an existing file path, else a
/// user workflow (`~/.sovereign/workflows/<name>.toml`), else a shipped starter.
/// User shadows shipped (same name → the user's edit wins).
pub fn resolve_workflow_source(name_or_path: &str) -> std::result::Result<(String, String), String> {
    let p = std::path::Path::new(name_or_path);
    if p.is_file() {
        return std::fs::read_to_string(p)
            .map(|t| (t, p.display().to_string()))
            .map_err(|e| format!("read {name_or_path}: {e}"));
    }
    let name = name_or_path.strip_suffix(".toml").unwrap_or(name_or_path);
    let user = workflows_dir().join(format!("{name}.toml"));
    if user.is_file() {
        return std::fs::read_to_string(&user)
            .map(|t| (t, format!("user:{name}")))
            .map_err(|e| format!("read {}: {e}", user.display()));
    }
    if let Some((_, toml)) = SHIPPED_WORKFLOWS.iter().find(|(n, _)| *n == name) {
        return Ok((toml.to_string(), format!("shipped:{name}")));
    }
    Err(format!(
        "no workflow `{name_or_path}` — not a file, not in {}, not a shipped starter.\n\
         See `sovereign workflow list`.",
        workflows_dir().display()
    ))
}

/// The first `#` comment line of a workflow's TOML — its one-line description for
/// `list`. Empty when the file has no leading comment.
pub fn first_comment_line(toml: &str) -> String {
    toml.lines()
        .map(|l| l.trim())
        .find(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default()
}

// ── Inference discovery ─────────────────────────────────────────────────

/// The chat + embed model ids the daemon advertises.
pub struct DaemonModels {
    pub chat: Option<String>,
    pub embed: Option<String>,
}

/// GET `<v1>/models` and split the advertised ids into chat vs. embed (by the
/// `embed` substring convention — the same one `/embeddings` routing uses).
/// Doubles as the daemon liveness probe (a connection error → "start the daemon").
pub async fn discover_models(v1: &str) -> std::result::Result<DaemonModels, String> {
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
    let embed = ids
        .iter()
        .find(|id| id.to_lowercase().contains("embed"))
        .map(|s| s.to_string());
    let chat = ids
        .iter()
        .find(|id| !id.to_lowercase().contains("embed"))
        .map(|s| s.to_string());
    if chat.is_none() && embed.is_none() {
        return Err("the daemon advertises no models".to_string());
    }
    Ok(DaemonModels { chat, embed })
}

/// Whether any step needs daemon-routed inference (so the daemon provider is
/// assembled). Classifies via the typed `StepKind::resources()` — the exhaustive
/// classifier — not a `uses.starts_with(…)` probe.
pub fn needs_inference(wf: &Workflow) -> bool {
    wf.steps.iter().any(|s| {
        StepKind::parse(&s.uses)
            .map(|k| k.resources() == ResourceNeed::Inference)
            .unwrap_or(false)
    })
}

/// Whether the workflow has an `embed:` step (so we require an embedding model).
pub fn uses_embed(wf: &Workflow) -> bool {
    wf.steps
        .iter()
        .any(|s| matches!(StepKind::parse(&s.uses), Ok(StepKind::Embed { .. })))
}

/// `~/.sovereign/workflow-cache` — alongside the canonical config.
pub fn cache_dir() -> std::path::PathBuf {
    sovereign_core::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("workflow-cache")
}

// ── Registry + run ──────────────────────────────────────────────────────

/// The standard tool registry: the `sovereign-tools` built-ins every workflow can
/// use (chunk/section/extract, read/write file+json, zip, corpus_store, web_fetch,
/// shell, the structural-atlas leaves) + the MCP servers from the canonical config
/// (so a server added via `sovereign mcp add` works here) + the caller's
/// `extra_tools` (the CLI injects its enrichment-authoring tools; the daemon passes
/// none). The MCP servers are HTTP — Sovereign connects endpoints, it doesn't spawn
/// subprocesses.
pub async fn standard_registry(extra_tools: Vec<Box<dyn Tool>>) -> ToolRegistry {
    use sovereign_tools::atlas_phase::gaps::AtlasGapsTool;
    use sovereign_tools::atlas_phase::tensions::AtlasTensionsTool;
    use sovereign_tools::corpus_store::CorpusStoreTool;
    use sovereign_tools::extract::ExtractTool;
    use sovereign_tools::rag::chunk::ChunkTool;
    use sovereign_tools::rag::section::SectionTool;
    use sovereign_tools::read_file::ReadFileTool;
    use sovereign_tools::read_json::ReadJsonTool;
    use sovereign_tools::shell::ShellTool;
    use sovereign_tools::web::WebFetchTool;
    use sovereign_tools::write_file::WriteFileTool;
    use sovereign_tools::write_json::WriteJsonTool;
    use sovereign_tools::zip::ZipTool;

    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(WebFetchTool::new()));
    tools.register(Box::new(ChunkTool));
    tools.register(Box::new(SectionTool));
    tools.register(Box::new(ExtractTool));
    tools.register(Box::new(ReadJsonTool));
    tools.register(Box::new(ReadFileTool));
    tools.register(Box::new(ZipTool));
    tools.register(Box::new(CorpusStoreTool));
    tools.register(Box::new(WriteJsonTool));
    tools.register(Box::new(WriteFileTool));
    tools.register(Box::new(AtlasGapsTool));
    tools.register(Box::new(AtlasTensionsTool));
    for t in extra_tools {
        tools.register(t);
    }
    let mcp = sovereign_tools::mcp::load_from_setup_config(&mut tools).await;
    for st in mcp.server_statuses().await {
        if st.connected {
            tracing::info!(server = %st.name, tools = st.tool_count, "workflow-host: mcp connected");
        } else if let Some(e) = &st.error {
            tracing::warn!(server = %st.name, error = %e, "workflow-host: mcp unavailable");
        }
    }
    tools
}

/// Run a workflow in-process against the daemon at `daemon` (e.g.
/// `http://localhost:9741`). Assembles inference (only when a `model:`/`embed:`
/// step is present), the standard registry (+ `extra_tools`), and the content
/// cache, then runs over the workflow's source items. Headless: progress goes to
/// `tracing`, and the `RunReport` is returned for the caller to present (the CLI
/// prints a summary; the daemon trigger logs it). Errors are human-readable strings.
pub async fn run_workflow_in_process(
    wf: &Workflow,
    daemon: &str,
    concurrency: usize,
    no_cache: bool,
    params: BTreeMap<String, String>,
    extra_tools: Vec<Box<dyn Tool>>,
) -> std::result::Result<RunReport, String> {
    let v1 = format!("{}/v1", daemon.trim_end_matches('/'));
    let inference: Option<Arc<dyn InferenceProvider>> = if needs_inference(wf) {
        let models = discover_models(&v1).await.map_err(|e| {
            format!(
                "Daemon not reachable at {daemon} ({e}). A `model:`/`embed:` step needs it — \
                 start it with `sovereign daemon`."
            )
        })?;
        if uses_embed(wf) && models.embed.is_none() {
            return Err(format!(
                "The daemon at {daemon} advertises no embedding model, but this workflow has an \
                 `embed:` step. Load an embed model (see `sovereign setup`) and retry."
            ));
        }
        let chat = models.chat.clone().unwrap_or_default();
        let embed = models.embed.clone().unwrap_or_default();
        tracing::info!(daemon, chat = %chat, embed = %embed, "workflow-host: daemon inference");
        Some(Arc::new(SplitInferenceProvider::new(&v1, chat, embed, 8192)))
    } else {
        None
    };

    let tools = standard_registry(extra_tools).await;
    let cache: Arc<dyn ArtifactCache> = if no_cache {
        Arc::new(NoCache)
    } else {
        Arc::new(FileArtifactCache::new(cache_dir()))
    };
    let registry = StepRegistry::new(inference, Arc::new(tools));
    Runner::with_cache(registry, cache)
        .with_params(params)
        .run(wf, concurrency)
        .await
        .map_err(|e| format!("workflow run failed: {e}"))
}

// ── Capability summary (the trust gate) ─────────────────────────────────

/// What a workflow is capable of, derived from the declared `Effect` and
/// `required_permissions()` of each step's tool. A triggered workflow runs
/// unattended, so this is surfaced at attach time — the user consents to "this can
/// run shell / fetch the network / write files" before the trigger is armed.
#[derive(Debug, Default)]
pub struct CapabilitySummary {
    /// Distinct effects across the steps' tools (Read / Write / ReadWrite).
    pub effects: Vec<Effect>,
    /// Distinct permissions the steps' tools declare (Shell, Network, FileWrite…).
    pub permissions: Vec<Permission>,
    /// The workflow has a `model:`/`embed:` step (uses your local model).
    pub needs_inference: bool,
    /// Steps whose tool couldn't be resolved right now (e.g. an MCP server that
    /// isn't connected) — surfaced so consent isn't given blind.
    pub unresolved: Vec<String>,
}

impl CapabilitySummary {
    /// Plain-language bullet phrases for a consent prompt, e.g.
    /// `["run shell commands", "write files", "use your local model"]`. Empty when
    /// the workflow only reads and transforms (a benign read-only summary).
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        for p in &self.permissions {
            out.push(
                match p {
                    Permission::Shell => "run shell commands",
                    Permission::Network => "fetch the network",
                    Permission::FileRead => "read files anywhere",
                    Permission::FileWrite => "write files",
                    Permission::EmailRead => "read your email",
                    Permission::EmailWrite => "send or draft email",
                    Permission::CalendarRead => "read your calendar",
                    Permission::CalendarWrite => "change your calendar",
                    Permission::RecipeAuthoring => "author recipes",
                }
                .to_string(),
            );
        }
        // A write effect with no specific permission still mutates state.
        if out.is_empty()
            && self
                .effects
                .iter()
                .any(|e| matches!(e, Effect::Write | Effect::ReadWrite))
        {
            out.push("write data".to_string());
        }
        if self.needs_inference {
            out.push("use your local model".to_string());
        }
        out
    }
}

/// Build the standard registry and derive a [`CapabilitySummary`] for `wf`. Async
/// because resolving `mcp:` steps needs the configured MCP servers loaded.
pub async fn summarize_capabilities(wf: &Workflow) -> CapabilitySummary {
    let registry = standard_registry(vec![]).await;
    workflow_capabilities(wf, &registry)
}

/// Derive a [`CapabilitySummary`] given an already-built registry — the pure,
/// testable core of [`summarize_capabilities`].
pub fn workflow_capabilities(wf: &Workflow, registry: &ToolRegistry) -> CapabilitySummary {
    let mut effects: Vec<Effect> = Vec::new();
    let mut permissions: Vec<Permission> = Vec::new();
    let mut needs_inference = false;
    let mut unresolved: Vec<String> = Vec::new();

    let add_tool = |id: &str, uses: &str, effects: &mut Vec<Effect>, permissions: &mut Vec<Permission>, unresolved: &mut Vec<String>| {
        match registry.get(id) {
            Ok(t) => {
                let e = t.descriptor().effect;
                if !effects.contains(&e) {
                    effects.push(e);
                }
                for p in t.required_permissions() {
                    if !permissions.contains(&p) {
                        permissions.push(p);
                    }
                }
            }
            Err(_) => unresolved.push(uses.to_string()),
        }
    };

    for step in &wf.steps {
        match StepKind::parse(&step.uses) {
            Ok(StepKind::Model { .. }) | Ok(StepKind::Embed { .. }) => needs_inference = true,
            Ok(StepKind::Transform { .. }) => {}
            Ok(StepKind::Tool { id }) => {
                add_tool(&id, &step.uses, &mut effects, &mut permissions, &mut unresolved)
            }
            Ok(StepKind::Mcp { server, tool }) => {
                let mcp_id = format!("mcp_{server}_{tool}");
                if registry.get(&mcp_id).is_ok() {
                    add_tool(&mcp_id, &step.uses, &mut effects, &mut permissions, &mut unresolved);
                } else {
                    // An MCP tool we can't see right now is still a network action.
                    // Record that AND flag it so consent isn't blind.
                    if !permissions.contains(&Permission::Network) {
                        permissions.push(Permission::Network);
                    }
                    unresolved.push(step.uses.clone());
                }
            }
            Err(_) => unresolved.push(step.uses.clone()),
        }
    }

    CapabilitySummary {
        effects,
        permissions,
        needs_inference,
        unresolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workflow's declared capabilities come straight from its steps' tools:
    /// `tool:shell` → Shell, `tool:write_file` → FileWrite, `model:` →
    /// needs_inference, `transform:` → nothing. Pins the trust-gate derivation.
    #[tokio::test]
    async fn capabilities_reflect_the_steps() {
        let toml = r#"
[workflow]
name = "cap-test"
[source]
type = "inline"
items = ["x"]
[[step]]
id = "a"
uses = "tool:shell"
params = { command = "echo hi" }
[[step]]
id = "b"
uses = "model:thoughtful"
prompt = "hi"
[[step]]
id = "c"
uses = "transform:json"
"#;
        let wf = Workflow::parse(toml).expect("parses");
        let registry = standard_registry(vec![]).await;
        let caps = workflow_capabilities(&wf, &registry);
        assert!(caps.needs_inference, "model: step → needs_inference");
        assert!(
            caps.permissions.contains(&Permission::Shell),
            "tool:shell declares Shell, got {:?}",
            caps.permissions
        );
        let phrases = caps.describe();
        assert!(phrases.iter().any(|p| p.contains("shell")));
        assert!(phrases.iter().any(|p| p.contains("local model")));
    }
}
