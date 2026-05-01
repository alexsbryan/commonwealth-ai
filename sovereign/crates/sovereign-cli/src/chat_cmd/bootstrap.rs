//! Daemon-backed Runtime bootstrap for `sovereign chat`.
//!
//! Mirrors `sovereign-desktop::state::bootstrap` — same StateStore,
//! CorpusEngine, tools, mesh-knowledge wiring — but the
//! `InferenceProvider` is a `SplitInferenceProvider` that delegates
//! chat completions to the daemon's chat model and embeddings to the
//! daemon's embed model over HTTP. No embedded llama.cpp, no Tauri.
//!
//! Rationale
//! ---------
//! The desktop's Attach mode is the architectural template we want:
//! "the daemon already owns the model, talk to it over HTTP". The
//! desktop currently still loads local weights even in Attach mode
//! (historical quirk); this CLI does what Attach *should* do — pure
//! HTTP.
//!
//! The split-provider dance is required because `RemoteApiProvider`
//! uses a single `model_id` for both `/chat/completions` AND
//! `/embeddings`. Sending a chat model to the embeddings endpoint
//! returns non-embedding shapes (or errors). We keep two instances
//! and route by method.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::planner::LlmPlanner;
use sovereign_core::router::LlmRouter;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{
    ApprovalChannel, InferenceProvider, StateStore,
};
use sovereign_core::types::*;
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::shell::ShellTool;

use crate::chat_cmd::config::ChatGlobals;

/// Wraps two `RemoteApiProvider`s — one per endpoint — and routes
/// `InferenceProvider` trait calls to the correct one.
///
/// Everything that generates text (complete / complete_stream /
/// complete_batch / capabilities / model_id_for) goes to `chat`.
/// Everything that produces vectors (embed / embed_batch / embed_query)
/// goes to `embed`. Keeps the daemon honest: the chat endpoint never
/// sees an embed model id, and vice versa.
pub struct SplitInferenceProvider {
    chat: Arc<RemoteApiProvider>,
    embed: Arc<RemoteApiProvider>,
    chat_model_id: String,
}

impl SplitInferenceProvider {
    pub fn new(
        endpoint_v1: &str,
        chat_model_id: String,
        embed_model_id: String,
        context_size: u32,
    ) -> Self {
        let chat = Arc::new(RemoteApiProvider::new(
            endpoint_v1,
            None,
            &chat_model_id,
            context_size,
        ));
        let embed = Arc::new(RemoteApiProvider::new(
            endpoint_v1,
            None,
            &embed_model_id,
            context_size,
        ));
        Self {
            chat,
            embed,
            chat_model_id,
        }
    }
}

#[async_trait]
impl InferenceProvider for SplitInferenceProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.chat.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.chat.complete_stream(request).await
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        self.chat.complete_batch(requests).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed.embed(text).await
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        // We only have one chat slot over HTTP; the daemon itself
        // maps Fast/Slow to its loaded models. Reporting the request
        // model is the most honest signal we have client-side.
        self.chat_model_id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.chat.capabilities()
    }
}

/// Bundle of everything the chat subcommands need from bootstrap.
/// Carries `Arc<Runtime>` plus the handles required to persist turns
/// (the store) and browse prior conversations.
pub struct ChatSession {
    pub runtime: Arc<Runtime>,
    pub store: Arc<dyn StateStore>,
    pub corpus_engine: Arc<corpus_engine::CorpusEngine>,
    pub inference: Arc<dyn InferenceProvider>,
    pub daemon_base: String,
}

/// Build a `Runtime` backed by the daemon over HTTP.
///
/// Fails fast if the daemon isn't answering — there's no recovery
/// path a retry could fix, and a partially-initialized Runtime
/// pointing at a dead endpoint would produce confusing errors deep
/// in retrieval. The caller should exit with a hint.
pub async fn build_session(globals: &ChatGlobals) -> Result<ChatSession> {
    // 1. Probe the daemon before we touch anything else. A fast fail
    //    here prints a clean "start the daemon" message instead of
    //    the cryptic timeout from the first real request.
    let base = globals.daemon_base.clone();
    let v1 = format!("{base}/v1");
    probe_or_bail(&base).await?;

    // 2. Resolve model IDs. Preference order:
    //       a) explicit `--chat-model` / `--embed-model` flag,
    //       b) the daemon's `SetupConfig.models.*` filename stems
    //          — this is what the daemon actually loaded, and the
    //          daemon advertises those IDs on `/v1/models`,
    //       c) fallback: probe `/v1/models` and pick the first
    //          chat- and first embed-shaped entries.
    //    The historical (c)-only path picked non-deterministically
    //    between a locally-loaded `qwen-embedding-0.6b` (1024-dim)
    //    and a mesh-peer-advertised `Qwen3-Embedding-0.6B-Q8_0`
    //    whose dimensionality didn't match any installed corpus —
    //    silently downgrading every retrieval to FTS-only. Reading
    //    the config directly removes that race.
    let (chat_model, embed_model) = resolve_model_ids(&v1, globals).await?;
    eprintln!("Daemon: {base}");
    eprintln!("Chat model:  {chat_model}");
    eprintln!("Embed model: {embed_model}");

    let inference: Arc<dyn InferenceProvider> = Arc::new(SplitInferenceProvider::new(
        &v1,
        chat_model,
        embed_model.clone(),
        // Matches the RemoteApiProvider default from the desktop
        // Attach path. `Runtime` consumers read this via
        // `capabilities().max_context_tokens`; for today's models
        // this is approximate but non-blocking.
        8192,
    ));

    // 3. Open the state store. Creating the data dir on the fly is
    //    safe — mirrors the desktop's behaviour and means a first
    //    `sovereign chat` against a fresh home directory doesn't
    //    stumble on a missing folder.
    std::fs::create_dir_all(&globals.data_dir)
        .map_err(|e| Error::Serialization(format!("create {:?}: {e}", globals.data_dir)))?;
    let db_path = globals.data_dir.join("sovereign.db");
    eprintln!("Database:    {}", db_path.display());
    let store_concrete = Arc::new(
        SqliteStateStore::open(&db_path)
            .map_err(|e| Error::Serialization(format!("open db {:?}: {e}", db_path)))?,
    );
    let store: Arc<dyn StateStore> = store_concrete.clone();

    // 4. Build the CorpusEngine. The desktop (`state.rs:706-707`) and
    //    the legacy REPL (`main.rs:477-478`) both hardcode
    //    `~/.sovereign/{recipes,indexes}` regardless of
    //    `config.data.dir` — that field governs the state DB only,
    //    not corpus storage. Matching that convention means this CLI
    //    sees the same corpora the desktop just ingested.
    //
    //    If a user passed `--data-dir` explicitly they almost
    //    certainly meant to override BOTH paths; honour that by
    //    using `<data_dir>/indexes` when `--data-dir` was given.
    //    Otherwise stick to the hardcoded well-known path.
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dotsovereign = home.join(".sovereign");
    let (recipes_dir, indexes_dir): (PathBuf, PathBuf) = if globals.data_dir_explicit {
        (globals.data_dir.join("recipes"), globals.data_dir.join("indexes"))
    } else {
        (dotsovereign.join("recipes"), dotsovereign.join("indexes"))
    };
    eprintln!("Indexes:     {}", indexes_dir.display());
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    // The engine's `expected_embedding_model` flows into
    // `_corpus_meta.json` at ingest time and into shard-consistency
    // checks. The CLI doesn't ingest during chat, but if any tool
    // path later triggers an ingest (e.g. watcher-driven reindex
    // through the same engine), it must match what the desktop
    // would have written. We've already resolved `embed_model` from
    // SetupConfig above.
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir.clone(), embed_fn)
            .with_embedding_model(&embed_model)
            .with_inference_fn(inference_fn),
    );
    log_installed_corpora(&corpus_engine).await;

    // 5. Skills — empty registry is fine for chat; the runtime uses
    //    them to prefix system prompts with skill descriptors, and
    //    the chat flow is identical under "no active skill".
    let skills = Arc::new(SkillRegistry::new());

    // 6. Tools. Keep this list identical to the desktop bootstrap so
    //    the retrieval + tool-use path exercised here matches what
    //    the user sees in the GUI. Notably: `SearchTool::with_web`
    //    drives the "Searched ... web" sources in provenance.
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(sovereign_tools::document::DocumentTool::new(
        Arc::clone(&store),
        Arc::clone(&inference),
    )));
    tools.register(Box::new(sovereign_tools::ClaimSearchTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(sovereign_tools::EpistemicLandscapeTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(
        sovereign_tools::CodeSearchTool::new(Arc::clone(&corpus_engine))
            .with_inference(Arc::clone(&inference)),
    ));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(sovereign_tools::search::SearchTool::with_web(
        Arc::clone(&store),
        Arc::clone(&inference),
        // DuckDuckGo — free, no key required. Matches the no-API-key
        // fallback in main.rs for parity with the legacy REPL.
        sovereign_tools::web::search::SearchBackend::DuckDuckGo,
    )));
    tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    eprintln!("Tools:       {} registered", tools.count());

    // 7. Router + planner. The legacy REPL defaults to
    //    `PassthroughRouter`; the desktop uses LLM-based routing.
    //    Use the LLM router here so the chat flow is bit-for-bit
    //    identical to the desktop surface — the point of the CLI is
    //    to reproduce that flow, not a simplified version.
    let router: Box<dyn sovereign_core::traits::Router> = Box::new(LlmRouter::new(
        Arc::clone(&inference),
        Arc::clone(&store),
        Arc::clone(&skills),
    ));
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));

    // 8. Approval channel. Chat turns don't trigger confirmations
    //    in the normal path; we wire a yes-only stub so any stray
    //    approval request is auto-granted rather than deadlocking a
    //    one-shot CLI.
    let approval: Arc<dyn ApprovalChannel> = Arc::new(AutoApprove);

    // 9. Mesh knowledge client. Talks to the daemon's `/v1/mesh` —
    //    when no mesh is running, reqwest gets ECONNREFUSED on the
    //    first call and the Runtime falls through to local-only
    //    retrieval. Safe to install unconditionally (same policy as
    //    the desktop).
    let mesh_knowledge: Option<Arc<dyn sovereign_core::traits::MeshKnowledgeSource>> =
        match sovereign_mesh::knowledge_client::MeshKnowledgeClient::new(&base) {
            Ok(c) => Some(Arc::new(c)),
            Err(_) => None,
        };

    // 10. Runtime. Only the fields we need — routing events stay at
    //     the no-op default (the CLI has no UI to emit to), and
    //     landscape-digest / KnowledgeView is intentionally omitted
    //     (desktop feature, not load-bearing for chat correctness).
    let mut inference_config = InferenceConfig::default();
    if let Some(t) = globals.temperature {
        inference_config.temperature = t;
        eprintln!("Temperature: {t} (override)");
    }
    if let Some(n) = globals.max_tokens {
        inference_config.max_tokens = n;
        eprintln!("Max tokens: {n} (override)");
    }
    let mut runtime = Runtime::new(
        Arc::clone(&inference),
        router,
        Box::new(planner),
        Arc::new(tools),
        Arc::clone(&store),
        skills,
        approval,
        inference_config,
    )
    .with_corpus_engine(Arc::clone(&corpus_engine));
    if let Some(m) = mesh_knowledge {
        runtime = runtime.with_mesh_knowledge(m);
    }
    // Atlas Layer 0: load any installed Wikipedia link graph. Probes
    // `<indexes_dir>/<corpus>/wikipedia_graph.db` for each installed
    // corpus and, on the first hit, wires it into the Runtime. Today
    // we expect at most one Wikipedia-class corpus per install — if
    // a future build needs multiple, switch this to a registry of
    // (corpus_id, Arc<WikipediaGraph>).
    if let Some(graph) = load_wikipedia_graph(&corpus_engine, &indexes_dir).await {
        eprintln!(
            "Wiki graph:  {} articles, {} edges",
            graph.article_count().await,
            graph.edge_count().await,
        );
        runtime = runtime.with_wikipedia_graph(graph);
    }

    Ok(ChatSession {
        runtime: Arc::new(runtime),
        store,
        corpus_engine,
        inference,
        daemon_base: base,
    })
}

/// GET `/v1/models` with a 2s timeout. Any non-200 aborts bootstrap
/// with a clear remediation hint — the alternative is cryptic
/// "connection refused" errors minutes later, mid-retrieval.
async fn probe_or_bail(base: &str) -> Result<()> {
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| Error::Serialization(format!("http client build: {e}")))?;
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(Error::Serialization(format!(
            "daemon at {base} returned {} from /v1/models. \
             Is it really a sovereign daemon? Try `sovereign doctor`.",
            r.status()
        ))),
        Err(_) => Err(Error::Serialization(format!(
            "daemon unreachable at {base}. \
             Start it with `sovereign daemon run`, or pass --daemon <url>."
        ))),
    }
}

/// Resolve `(chat_model_id, embed_model_id)` against the daemon.
/// See the call-site comment in `build_session` for the preference
/// order — explicit flag → SetupConfig stem → `/v1/models` probe.
async fn resolve_model_ids(
    v1: &str,
    globals: &ChatGlobals,
) -> Result<(String, String)> {
    // (a) Explicit flags short-circuit everything.
    if let (Some(c), Some(e)) = (&globals.chat_model, &globals.embed_model) {
        return Ok((c.clone(), e.clone()));
    }

    // (b) SetupConfig filename stems. The daemon loads
    //     `config.models.embed` and advertises it on `/v1/models`
    //     under its filename stem (e.g. `qwen-embedding-0.6b.gguf`
    //     → `qwen-embedding-0.6b`). Preferring the stem over
    //     `/v1/models` iteration means we always reach the
    //     *local* slot, never a mesh-peer advertisement, and the
    //     answer is stable across invocations.
    let from_config = chat_and_embed_stems_from_config();
    let mut chat_found = globals.chat_model.clone().or_else(|| {
        from_config.as_ref().and_then(|s| s.chat.clone())
    });
    let mut embed_found = globals.embed_model.clone().or_else(|| {
        from_config.as_ref().and_then(|s| s.embed.clone())
    });
    if let (Some(c), Some(e)) = (chat_found.as_ref(), embed_found.as_ref()) {
        return Ok((c.clone(), e.clone()));
    }

    // (c) Fallback: probe `/v1/models`. Used when SetupConfig is
    //     absent (fresh install, dev without setup) or when it
    //     lacks one of the two slots.
    let url = format!("{v1}/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::Serialization(format!("http client build: {e}")))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| Error::Serialization(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Serialization(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| Error::Serialization(format!("parse /v1/models: {e}")))?;
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| Error::Serialization("/v1/models: no `data` array".into()))?;
    for m in arr {
        let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
        let lower = id.to_lowercase();
        let is_embed = lower.contains("embedding") || lower.contains("-embed");
        if is_embed {
            if embed_found.is_none() {
                embed_found = Some(id.to_string());
            }
        } else if chat_found.is_none() {
            chat_found = Some(id.to_string());
        }
    }

    match (chat_found, embed_found) {
        (Some(c), Some(e)) => Ok((c, e)),
        (None, _) => Err(Error::Serialization(
            "daemon lists no chat models — check `sovereign setup` and the primary/fast slots".into(),
        )),
        (_, None) => Err(Error::Serialization(
            "daemon lists no embedding model — retrieval will fail. Set `[models] embed` in \
             ~/.config/sovereign/config.toml or pass --embed-model."
                .into(),
        )),
    }
}

/// Filename-stem extraction for `SetupConfig.models.{primary,embed}`.
/// The daemon advertises these on `/v1/models` using exactly the
/// file stem (`qwen-embedding-0.6b.gguf` → `qwen-embedding-0.6b`),
/// so returning those stems gives us the stable local-model IDs
/// without any `/v1/models` round-trip.
struct ConfigModelStems {
    chat: Option<String>,
    embed: Option<String>,
}

fn chat_and_embed_stems_from_config() -> Option<ConfigModelStems> {
    let cfg = sovereign_core::setup_config::SetupConfig::load().ok()?;
    Some(ConfigModelStems {
        chat: cfg
            .models
            .primary
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string()),
        embed: cfg
            .models
            .embed
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string()),
    })
}

/// Emit a one-line summary of what the CorpusEngine can see. Helps
/// the user confirm they're pointing at the right `~/.sovereign/indexes`
/// before running a confused-retrieval diagnostic.
async fn log_installed_corpora(engine: &corpus_engine::CorpusEngine) {
    match engine.installed_indexes().await {
        Ok(ix) if ix.is_empty() => {
            eprintln!("Corpora:     (none installed)");
        }
        Ok(ix) => {
            let names: Vec<String> = ix
                .iter()
                .map(|i| format!("{} ({} chunks)", i.corpus_id, i.chunk_count))
                .collect();
            eprintln!("Corpora:     {}", names.join(", "));
        }
        Err(e) => {
            eprintln!("Corpora:     <error: {e}>");
        }
    }
}

/// Probe `<indexes_dir>/<corpus_id>/wikipedia_graph.db` for each
/// installed corpus and return the first WikipediaGraph that opens
/// cleanly. `None` when no graph file is present — retrieval then
/// behaves exactly as before (no graph expansion, no contested
/// markers). Builds graphs out-of-band via
/// `sovereign atlas wikipedia build-graph <corpus-id>`.
async fn load_wikipedia_graph(
    engine: &corpus_engine::CorpusEngine,
    indexes_dir: &std::path::Path,
) -> Option<Arc<corpus_engine::WikipediaGraph>> {
    let infos = engine.installed_indexes().await.ok()?;
    for info in infos {
        let db_path = corpus_engine::WikipediaGraph::default_db_path(
            indexes_dir,
            &info.corpus_id,
        );
        if !db_path.exists() {
            continue;
        }
        match corpus_engine::WikipediaGraph::open(&db_path, &info.corpus_id) {
            Ok(g) => return Some(Arc::new(g)),
            Err(e) => {
                tracing::warn!(
                    corpus = %info.corpus_id,
                    db = %db_path.display(),
                    error = %e,
                    "wikipedia_graph: open failed; skipping"
                );
            }
        }
    }
    None
}

/// Approval channel that silently yes-answers everything. Chat never
/// hits the ask-user path in practice; this prevents a surprise
/// deadlock in a one-shot CLI invocation.
struct AutoApprove;

#[async_trait]
impl ApprovalChannel for AutoApprove {
    async fn request_approval(
        &self,
        _step: &Step,
        _preview: &ActionPreview,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok(String::new())
    }

    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
}

