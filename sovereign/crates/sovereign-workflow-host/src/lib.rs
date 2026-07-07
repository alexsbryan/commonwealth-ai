// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-workflow-host` — the daemon-runnable workflow host.
//!
//! The `sovereign-workflow` engine is contract-only (Step·Artifact·Runner over
//! `sovereign-contracts` traits); it doesn't know about concrete tools or inference.
//! Running a workflow needs that *assembly*: the standard tool registry
//! (`sovereign-tools-base` pure built-ins + the MCP servers from
//! `~/.sovereign/config.toml`), daemon-routed inference (a `SplitInferenceProvider`
//! from `oicp-client`), and the content cache.
//!
//! That assembly used to live inside the CLI (`workflow_cmd::run_assembled`), so
//! only the CLI could run a workflow. This crate hoists it to a place the **daemon**
//! can depend on, which is the prerequisite for the living trigger (a watched folder
//! running a workflow on its own). The CLI now calls [`run_workflow_in_process`] and
//! adds its own presentation; the daemon calls it headless.
//!
//! Dependency shape (the extractable "studio" package boundary): `workflow-host →
//! {sovereign-contracts, oicp-client, sovereign-tools-base, sovereign-workflow}` — all
//! contract crates or leaves, no reach into the monolith. The corpus/atlas tools the
//! base registry drops are injected by call sites via the runner's `extra_tools` slot
//! (`sovereign_tools::workflow_corpus_tools()`), and the living trigger's daemon glue
//! moved to `sovereign-cli-daemon`. No cycle.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use oicp_client::SplitInferenceProvider;
use sovereign_contracts::registry::ToolRegistry;
use sovereign_contracts::traits::{CorpusInstaller, InferenceProvider, Tool};
use sovereign_contracts::types::{Effect, Permission};
use sovereign_workflow::{
    ArtifactCache, FileArtifactCache, NoCache, ResourceNeed, RunReport, Runner, StepKind,
    StepRegistry, Workflow,
};

/// Re-exported so a caller (the desktop run surface) can attach a progress
/// observer and type the events it receives without depending on
/// `sovereign-workflow` directly.
pub use sovereign_workflow::{StepObserver, WorkflowProgress};

pub mod author;
pub use author::{
    author_tools, WorkflowValidateTool, WorkflowWriteStructuredTool, WorkflowWriteTool,
};

pub mod author_schema;
pub use author_schema::workflow_json_schema;

pub mod installer;
pub use installer::HttpCorpusInstaller;

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
    (
        "genome",
        include_str!("../../sovereign-workflow/recipes/genome.toml"),
    ),
    (
        "movie-catalog",
        include_str!("../../sovereign-workflow/recipes/movie-catalog.toml"),
    ),
    (
        "recommend-personal",
        include_str!("../../sovereign-workflow/recipes/recommend-personal.toml"),
    ),
];

/// `~/.sovereign/workflows` — user-owned, editable workflows (the `copy`/`new`
/// target). Resolved from the same home-dir as the setup config + workflow cache.
pub fn workflows_dir() -> std::path::PathBuf {
    sovereign_contracts::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("workflows")
}

/// Resolve a workflow reference to `(toml, origin)`: an existing file path, else a
/// user workflow (`~/.sovereign/workflows/<name>.toml`), else a shipped starter.
/// User shadows shipped (same name → the user's edit wins).
pub fn resolve_workflow_source(
    name_or_path: &str,
) -> std::result::Result<(String, String), String> {
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
    sovereign_contracts::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("workflow-cache")
}

// ── Registry + run ──────────────────────────────────────────────────────

/// The corpus/atlas tool ids that [`standard_registry`] deliberately does NOT
/// register — each is backed by corpus-engine/LanceDB, which the base bundle
/// exists to avoid. Call sites that run workflows inject the concrete tools via
/// the runner's `extra_tools` slot (`sovereign_tools::workflow_corpus_tools()`),
/// so at runtime these ids ARE resolvable.
///
/// This const is the contract's manifest of "provided by injection, not by the
/// base registry": the shipped-workflow resolution test treats a `tool:` step
/// naming one of these as valid even though the standalone base registry can't
/// resolve it. It must mirror `sovereign_tools::workflow_corpus_tools()` (the
/// helper that actually constructs these tools). Drift is self-correcting: a
/// shipped workflow that names a corpus tool absent from BOTH the base registry
/// and this list trips `shipped_recipes_parse_and_resolve_tools`, which forces
/// the id to be added here.
pub const WORKFLOW_CORPUS_TOOL_IDS: &[&str] = &[
    "extract",
    "corpus_store",
    "corpus_search",
    "atlas_gaps",
    "atlas_tensions",
];

/// The standard tool registry: the `sovereign-tools` built-ins every workflow can
/// use (chunk/section/extract, read/write file+json, zip, corpus_store, web_fetch,
/// shell, the structural-atlas leaves) + the MCP servers from the canonical config
/// (so a server added via `sovereign mcp add` works here) + the caller's
/// `extra_tools` (the CLI injects its enrichment-authoring tools; the daemon passes
/// none). The MCP servers are HTTP — Sovereign connects endpoints, it doesn't spawn
/// subprocesses.
pub async fn standard_registry(extra_tools: Vec<Box<dyn Tool>>) -> ToolRegistry {
    use sovereign_tools_base::rag::chunk::ChunkTool;
    use sovereign_tools_base::rag::section::SectionTool;
    use sovereign_tools_base::read_csv::ReadCsvTool;
    use sovereign_tools_base::read_file::ReadFileTool;
    use sovereign_tools_base::read_json::ReadJsonTool;
    use sovereign_tools_base::shell::ShellTool;
    use sovereign_tools_base::vector_mean::VectorMeanTool;
    use sovereign_tools_base::web::WebFetchTool;
    use sovereign_tools_base::write_file::WriteFileTool;
    use sovereign_tools_base::write_json::WriteJsonTool;
    use sovereign_tools_base::zip::ZipTool;

    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(WebFetchTool::new()));
    tools.register(Box::new(ChunkTool));
    tools.register(Box::new(SectionTool));
    tools.register(Box::new(ReadJsonTool));
    tools.register(Box::new(ReadFileTool));
    tools.register(Box::new(ZipTool));
    tools.register(Box::new(ReadCsvTool));
    tools.register(Box::new(VectorMeanTool));
    tools.register(Box::new(WriteJsonTool));
    tools.register(Box::new(WriteFileTool));
    // The corpus/atlas tools ([`WORKFLOW_CORPUS_TOOL_IDS`]: corpus_store,
    // corpus_search, atlas_gaps, atlas_tensions, and the heavy ExtractTool) stay
    // in sovereign-tools — registering them here would drag corpus-engine/LanceDB
    // into this bundle. Call sites that need them inject via `extra_tools` (the
    // daemon trigger and the CLI/desktop workflow commands do, preserving the full
    // tool surface).
    for t in extra_tools {
        tools.register(t);
    }
    let mcp = sovereign_tools_base::mcp::load_from_setup_config(&mut tools).await;
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
    // The embed slot's query-instruction prefix, resolved by the caller from the
    // discovered embed-model id. Injected (not read from a bundled manifest here)
    // so this bundle carries no sovereign-core dependency; the CLI and daemon
    // pass a `ModelsManifest`-backed closure. (B:P9a switches callers to source
    // it from the OICP capabilities manifest.)
    embed_query_instruction_for: impl Fn(&str) -> String,
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
        // Behavior-preserving: the embed slot's query-instruction prefix, which
        // the old constructor derived from `DEFAULT_MANIFEST` internally — now
        // supplied by the caller. Computed before `embed` is moved into arg 3.
        let embed_query_instruction = embed_query_instruction_for(&embed);
        Some(Arc::new(SplitInferenceProvider::new(
            &v1,
            chat,
            embed,
            8192,
            embed_query_instruction,
        )))
    } else {
        None
    };

    // Attach the corpus installer so `recipe:` stages can run. It targets the
    // daemon's loopback install endpoint — the same path `corpus install` uses, so
    // both CLI runs and daemon-triggered runs delegate to it (no mesh refactor).
    let installer: Arc<dyn CorpusInstaller> = Arc::new(HttpCorpusInstaller::new());
    run_workflow_with_provider(
        wf,
        inference,
        Some(installer),
        concurrency,
        no_cache,
        params,
        extra_tools,
        None,
    )
    .await
}

/// Run a workflow with an **injected** inference provider (plus an optional
/// corpus installer and progress observer), rather than constructing a
/// daemon-routed provider from a URL.
///
/// This is the embedding entry for a process that already holds a provider — the
/// desktop passes its `AppState.inference` (a [`SplitInferenceProvider`] in
/// attach mode, an in-process provider when embedded), so the run surface works
/// in both modes and reuses exactly the provider desktop chat uses, with no extra
/// `/v1/models` round-trip. [`run_workflow_in_process`] is this with the provider
/// built from a daemon URL and no observer.
///
/// `observer`, when `Some`, receives a [`WorkflowProgress`] event at each
/// lifecycle point so a UI can stream "watch it go" updates; `None` is the
/// headless path (progress goes only to `tracing`).
#[allow(clippy::too_many_arguments)]
pub async fn run_workflow_with_provider(
    wf: &Workflow,
    inference: Option<Arc<dyn InferenceProvider>>,
    installer: Option<Arc<dyn CorpusInstaller>>,
    concurrency: usize,
    no_cache: bool,
    params: BTreeMap<String, String>,
    extra_tools: Vec<Box<dyn Tool>>,
    observer: Option<StepObserver>,
) -> std::result::Result<RunReport, String> {
    let tools = standard_registry(extra_tools).await;
    let cache: Arc<dyn ArtifactCache> = if no_cache {
        Arc::new(NoCache)
    } else {
        Arc::new(FileArtifactCache::new(cache_dir()))
    };
    let registry = StepRegistry::new(inference, Arc::new(tools)).with_installer(installer);
    Runner::with_cache(registry, cache)
        .with_params(params)
        .with_observer(observer)
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
                    Permission::WorkflowAuthoring => "author workflows",
                    Permission::CorpusIngest => "download and index a corpus",
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

    let add_tool = |id: &str,
                    uses: &str,
                    effects: &mut Vec<Effect>,
                    permissions: &mut Vec<Permission>,
                    unresolved: &mut Vec<String>| {
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
            Ok(StepKind::Recipe { .. }) => {
                // A recipe stage downloads + indexes a corpus (network + compute +
                // disk write) via the install path. One honest capability line.
                if !effects.contains(&Effect::Write) {
                    effects.push(Effect::Write);
                }
                if !permissions.contains(&Permission::CorpusIngest) {
                    permissions.push(Permission::CorpusIngest);
                }
            }
            Ok(StepKind::Tool { id }) => add_tool(
                &id,
                &step.uses,
                &mut effects,
                &mut permissions,
                &mut unresolved,
            ),
            Ok(StepKind::Mcp { server, tool }) => {
                let mcp_id = format!("mcp_{server}_{tool}");
                if registry.get(&mcp_id).is_ok() {
                    add_tool(
                        &mcp_id,
                        &step.uses,
                        &mut effects,
                        &mut permissions,
                        &mut unresolved,
                    );
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

    /// Every shipped recipe parses AND every `tool:` step it names resolves — either
    /// to a tool in the base registry, or to one of the [`WORKFLOW_CORPUS_TOOL_IDS`]
    /// that real call sites inject via `extra_tools`. Guards the hand-written
    /// `recipes/` TOML against a typo or a primitive that doesn't exist (e.g. a
    /// renamed/forgotten tool) — parse + resolve, no daemon. (`shipped_examples_parse`
    /// in sovereign-workflow covers `examples/`; this covers the host's
    /// `SHIPPED_WORKFLOWS`.)
    #[tokio::test]
    async fn shipped_recipes_parse_and_resolve_tools() {
        let registry = standard_registry(vec![]).await;
        assert!(
            SHIPPED_WORKFLOWS.len() >= 5,
            "expected several shipped recipes, found {}",
            SHIPPED_WORKFLOWS.len()
        );
        for (name, toml) in SHIPPED_WORKFLOWS {
            let wf = Workflow::parse(toml)
                .unwrap_or_else(|e| panic!("shipped recipe `{name}` must parse: {e}"));
            for step in &wf.steps {
                if let Some(id) = step.uses.strip_prefix("tool:") {
                    // A shipped workflow may name a corpus/atlas tool the base
                    // registry omits by design; those are injected at runtime, so
                    // treat a known-injected id as resolved.
                    if WORKFLOW_CORPUS_TOOL_IDS.contains(&id) {
                        continue;
                    }
                    registry.get(id).unwrap_or_else(|_| {
                        panic!("shipped recipe `{name}` names unregistered tool `{id}`")
                    });
                }
            }
        }
    }

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
