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
use sovereign_inference::remote::SplitInferenceProvider;
use sovereign_tools::corpus_store::CorpusStoreTool;
use sovereign_tools::rag::chunk::ChunkTool;
use sovereign_tools::shell::ShellTool;
use sovereign_tools::web::WebFetchTool;
use sovereign_workflow::{
    ArtifactCache, FileArtifactCache, NoCache, ResourceNeed, Runner, StepKind, StepRegistry,
    Workflow,
};

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

    run_assembled(&wf, &daemon, concurrency, no_cache).await
}

/// Assemble the light stack — daemon-routed inference (only when a `model:` or
/// `embed:` step is present, via a `SplitInferenceProvider` so embed hits the
/// embed slot and chat the chat slot), a tool registry, MCP servers, and the
/// content cache — then run `wf` and print a per-item summary. Shared by
/// `workflow run <file>` and `corpus ingest <folder>` so the assembly lives in
/// one place.
pub(crate) async fn run_assembled(
    wf: &Workflow,
    daemon: &str,
    concurrency: usize,
    no_cache: bool,
) -> i32 {
    let v1 = format!("{}/v1", daemon.trim_end_matches('/'));
    let inference: Option<Arc<dyn InferenceProvider>> = if needs_inference(wf) {
        let models = match discover_models(&v1).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "Daemon not reachable at {daemon} ({e}).\n\
                     A `model:`/`embed:` step needs it — start it with `sovereign daemon`."
                );
                return 1;
            }
        };
        // An `embed:` step needs an embedding model; fail clearly if none is loaded.
        if uses_embed(wf) && models.embed.is_none() {
            eprintln!(
                "The daemon at {daemon} advertises no embedding model, but this workflow \
                 has an `embed:` step.\nLoad an embed model (see `sovereign setup`) and retry."
            );
            return 1;
        }
        let chat = models.chat.clone().unwrap_or_default();
        let embed = models.embed.clone().unwrap_or_default();
        eprintln!(
            "Daemon: {daemon}  ·  chat: {}  ·  embed: {}",
            if chat.is_empty() { "—" } else { &chat },
            if embed.is_empty() { "—" } else { &embed },
        );
        Some(Arc::new(SplitInferenceProvider::new(&v1, chat, embed, 8192)))
    } else {
        None
    };

    // Tools: cheap built-ins + MCP servers from the canonical config (same path
    // chat uses, so a server added via `sovereign mcp add` works here too).
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(WebFetchTool::new()));
    tools.register(Box::new(ChunkTool));
    tools.register(Box::new(CorpusStoreTool));
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
    let report = match Runner::with_cache(registry, cache).run(wf, concurrency).await {
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

/// Whether any step needs daemon-routed inference (so the daemon provider is
/// assembled and required). Classifies via the typed `StepKind::resources()` —
/// the exhaustive classifier — not a `uses.starts_with(…)` probe that a new
/// inference-bearing kind could slip past (ARCH §2.1).
fn needs_inference(wf: &Workflow) -> bool {
    wf.steps.iter().any(|s| {
        StepKind::parse(&s.uses)
            .map(|k| k.resources() == ResourceNeed::Inference)
            .unwrap_or(false)
    })
}

/// Whether the workflow has an `embed:` step (so we require an embedding model).
/// Matches the typed `Embed` variant, not the `"embed:"` prefix string.
fn uses_embed(wf: &Workflow) -> bool {
    wf.steps
        .iter()
        .any(|s| matches!(StepKind::parse(&s.uses), Ok(StepKind::Embed { .. })))
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

/// The chat + embed model ids the daemon advertises.
pub(crate) struct DaemonModels {
    pub chat: Option<String>,
    pub embed: Option<String>,
}

/// GET `<v1>/models` and split the advertised ids into chat vs. embed (by the
/// `embed` substring convention — the same one `/embeddings` routing uses).
/// Doubles as the daemon liveness probe (a connection error → "start the
/// daemon"). Shared with `corpus search` so the convention lives in one place.
pub(crate) async fn discover_models(v1: &str) -> std::result::Result<DaemonModels, String> {
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

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;
    use sovereign_core::error::Result as CoreResult;
    use sovereign_core::registry::ToolRegistry;
    use sovereign_core::traits::{InferenceProvider, Tool};
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, Depth, Effect, Idempotency, Latency, Permission,
        ProviderCapabilities, Scope as ToolScope, Speed, StepOutput, ToolContext, ToolDescriptor,
    };
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

    /// The generalization proof. Re-express corpus ingest's `chunk → embed`
    /// stage as a Workflow and diff it, byte for byte, against the **real**
    /// chunker + embed run directly. The `Artifact` never changed — a chunker's
    /// `1→N` output is just a JSON-array collection — only the Runner grew
    /// `for_each` to map `embed` over it. A clean diff means the substrate
    /// subsumes a fifth, previously-bespoke pipeline; the second run shows the
    /// content cache giving free resume on that same pipeline.
    #[tokio::test]
    async fn chunk_then_embed_matches_the_real_corpus_pipeline() {
        // Several paragraphs so the real chunker yields more than one chunk
        // (exercising the fan-out, not a degenerate single element).
        let doc = (0..6)
            .map(|i| {
                format!(
                    "Paragraph {i}. {}",
                    "The quick brown fox jumps over the lazy dog near the riverbank. ".repeat(3)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.txt");
        std::fs::write(&path, &doc).unwrap();

        // ── Oracle: the real corpus steps, run directly ──
        let oracle_chunks = sovereign_tools::rag::chunk::chunk_text(&doc);
        assert!(
            oracle_chunks.len() > 1,
            "doc must split into several chunks to exercise fan-out"
        );
        let provider = DeterministicEmbed;
        let mut oracle = Vec::new();
        for c in &oracle_chunks {
            oracle.push(provider.embed(&c.content).await.unwrap());
        }
        // Shape the oracle exactly as EmbedStep does (f32 → f64 → JSON) so the
        // comparison is byte-identical, not approximate.
        let oracle_json = serde_json::Value::Array(
            oracle
                .iter()
                .map(|v| {
                    serde_json::Value::Array(
                        v.iter().map(|f| serde_json::Value::from(*f as f64)).collect(),
                    )
                })
                .collect(),
        );

        // ── The same pipeline, authored as a Workflow ──
        let toml = r#"
[workflow]
name = "chunk-embed"
[source]
type = "folder"
path = "__DIR__"
glob = "*.txt"
[[step]]
id = "chunk"
uses = "tool:chunk"
params = { path = "{item.path}" }
[[step]]
id = "embed"
uses = "embed:default"
for_each = "chunk"
input = "{element.text}"
"#
        .replace("__DIR__", &dir.path().to_string_lossy());
        let wf = Workflow::parse(&toml).unwrap();

        let mk_registry = || {
            let mut tools = ToolRegistry::new();
            tools.register(Box::new(ChunkerTool));
            StepRegistry::new(
                Some(Arc::new(DeterministicEmbed) as Arc<dyn InferenceProvider>),
                Arc::new(tools),
            )
        };

        // First run, fresh cache: the chunk step + one embed per chunk run.
        let cache: Arc<dyn sovereign_workflow::ArtifactCache> = Arc::new(
            sovereign_workflow::FileArtifactCache::new(dir.path().join(".cache")),
        );
        let r1 = Runner::with_cache(mk_registry(), cache.clone())
            .run(&wf, 4)
            .await
            .unwrap();
        assert_eq!(r1.ok_count(), 1, "{:?}", r1.items);

        // The byte-identical diff: same chunks, same embeddings, same order.
        // Compare the *serialized* forms, not parsed Values: serde_json's
        // default float parser is 1-ULP lossy (no `float_roundtrip` feature),
        // so parse-then-compare shows phantom last-digit diffs. The Ryū
        // serializer is exact and deterministic, so equal f64 arrays serialize
        // to identical strings — a faithful diff of the two pipelines.
        let wf_text = r1.items[0].result.as_ref().unwrap();
        let oracle_text = serde_json::to_string(&oracle_json).unwrap();
        assert_eq!(
            wf_text, &oracle_text,
            "workflow chunk→embed must equal the real pipeline"
        );
        let wf_json: serde_json::Value = serde_json::from_str(wf_text).unwrap();
        assert_eq!(wf_json.as_array().unwrap().len(), oracle_chunks.len());
        assert_eq!(
            (r1.ran_total(), r1.cached_total()),
            (oracle_chunks.len() + 1, 0),
            "fresh: chunk + one embed per chunk"
        );

        // Re-run, unchanged: every step is a cache hit — free resume on the
        // real pipeline.
        let r2 = Runner::with_cache(mk_registry(), cache.clone())
            .run(&wf, 4)
            .await
            .unwrap();
        assert_eq!(
            (r2.ran_total(), r2.cached_total()),
            (0, oracle_chunks.len() + 1),
            "unchanged re-run is fully cached"
        );
    }

    // ── doubles: the real chunker as a tool + a deterministic embed ──

    /// Wraps the *production* `chunk_text` so the diff runs the real chunker,
    /// not a stand-in. Reads its `path` param and emits the `1→N` collection.
    struct ChunkerTool;

    #[async_trait]
    impl Tool for ChunkerTool {
        fn descriptor(&self) -> ToolDescriptor {
            ToolDescriptor {
                id: "chunk".to_string(),
                name: "chunk".to_string(),
                description: "split a file into the corpus chunker's chunks".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }),
                examples: vec![],
                effect: Effect::Read,
                idempotency: Idempotency::Idempotent,
                latency: Latency::Fast,
                scope: ToolScope::Session,
                output_schema: None,
            }
        }
        fn required_permissions(&self) -> Vec<Permission> {
            vec![]
        }
        async fn execute(
            &self,
            params: &serde_json::Value,
            _ctx: &ToolContext,
        ) -> CoreResult<StepOutput> {
            let path = params.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let text = std::fs::read_to_string(path).map_err(|e| {
                sovereign_core::error::Error::Execution(format!("chunk read {path}: {e}"))
            })?;
            let arr: Vec<serde_json::Value> = sovereign_tools::rag::chunk::chunk_text(&text)
                .into_iter()
                .map(|c| serde_json::json!({ "text": c.content, "index": c.index }))
                .collect();
            Ok(StepOutput::Json(serde_json::Value::Array(arr)))
        }
    }

    /// A deterministic 8-dim embedding (identical text → identical vector,
    /// distinct text → distinct vector). Lets the diff assert byte-identity
    /// without a daemon or weights — the claim is about the Runner's fan-out,
    /// not the embedding model.
    struct DeterministicEmbed;

    #[async_trait]
    impl InferenceProvider for DeterministicEmbed {
        async fn complete(&self, _req: &CompletionRequest) -> CoreResult<CompletionResponse> {
            unreachable!("the chunk→embed diff never calls complete()")
        }
        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> CoreResult<Pin<Box<dyn Stream<Item = CoreResult<String>> + Send>>> {
            unreachable!("the chunk→embed diff never streams")
        }
        async fn embed(&self, text: &str) -> CoreResult<Vec<f32>> {
            Ok((0..8u64)
                .map(|d| {
                    // FNV-1a over (dim, bytes) → a bounded, text-sensitive scalar.
                    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ d.wrapping_mul(0x0000_0100_0000_01b3);
                    for b in text.bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                    ((h % 2000) as f32) / 1000.0 - 1.0
                })
                .collect())
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 8192,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }
}

