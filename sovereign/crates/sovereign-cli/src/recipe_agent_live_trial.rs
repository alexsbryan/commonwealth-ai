//! `sovereign recipe-agent live-trial` — scripted, daemon-driven trial.
//!
//! Drives the recipe-author agent loop end-to-end against a real LLM
//! (the running sovereign daemon's `/v1/chat/completions`). Reads
//! partner messages from a script file, prepends the situated-context
//! envelope to each turn, calls `Runtime::handle_message`, and at
//! the end validates the generated recipe + runs an initial fetch.
//!
//! Intended uses:
//!
//! - Regression test: pin Marcus's first-session script and assert
//!   the resulting recipe passes validation + extracts ≥ 1 doc.
//! - Prompt iteration: tweak the recipe-author skill manifest and
//!   re-run the same script to see how behaviour changes.
//! - Smoke test before a real partner session: verify the daemon +
//!   tools + skill + situated-context renderer all wire together.
//!
//! Minimal Runtime build — no atlas, no wikipedia graph, no mesh
//! knowledge. Just the chat REPL essentials plus the 10 recipe-
//! author + web-research tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use corpus_engine::{FeatureStore, NoteScope, NoteStore, ScopeFilter};
use sovereign_core::error::Result;
use sovereign_core::planner::LlmPlanner;
use sovereign_core::router::LlmRouter;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{
    ApprovalChannel, InferenceProvider, StateStore, Tool,
};
use sovereign_core::types::{
    ActionPreview, CompletionRequest, CompletionResponse, InferenceConfig,
    ProviderCapabilities, Speed, Step, StepOutput,
};
use sovereign_core::SkillRegistry;
use sovereign_core::ToolRegistry;
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::recipe_author::{
    situated_context, CapabilityRequestTool, CheckpointTool, DecisionLogTool,
    RecipeProject, RecipeReadTool, RecipeTestTool, RecipeValidateTool,
    RecipeWriteTool, RegistryBrowseTool,
};

// ─── Args ────────────────────────────────────────────────────────

#[derive(Debug)]
struct Args {
    charter_path: PathBuf,
    script_path: PathBuf,
    feature_id: Option<String>,
    title: Option<String>,
    daemon_base: String,
    skills_dir: PathBuf,
    sample_size: u64,
    chat_model: Option<String>,
    embed_model: Option<String>,
}

fn parse_args(argv: &[String]) -> std::result::Result<Args, String> {
    let mut charter: Option<PathBuf> = None;
    let mut script: Option<PathBuf> = None;
    let mut feature_id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut daemon_base = "http://localhost:9741".to_string();
    let mut skills_dir: Option<PathBuf> = None;
    let mut sample_size: u64 = 50;
    let mut chat_model: Option<String> = None;
    let mut embed_model: Option<String> = None;

    let mut iter = argv.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--charter" => charter = iter.next().map(PathBuf::from),
            "--script" => script = iter.next().map(PathBuf::from),
            "--feature-id" => feature_id = iter.next().cloned(),
            "--title" => title = iter.next().cloned(),
            "--daemon" => {
                if let Some(v) = iter.next() {
                    daemon_base = v.clone();
                }
            }
            "--skills-dir" => skills_dir = iter.next().map(PathBuf::from),
            "--sample-size" => {
                sample_size = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--sample-size requires an integer".to_string())?;
            }
            "--chat-model" => chat_model = iter.next().cloned(),
            "--embed-model" => embed_model = iter.next().cloned(),
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    let charter_path = charter.ok_or("missing --charter <FILE>")?;
    let script_path = script.ok_or("missing --script <FILE>")?;
    let skills_dir = skills_dir.unwrap_or_else(|| {
        // Default: look for `<repo-root>/sovereign/skills/` relative
        // to the binary's working dir, falling back to
        // `~/.sovereign/skills/`. The trial harness is meant to be
        // run from the workspace root during development.
        let cwd = std::env::current_dir().unwrap_or_default();
        let candidate = cwd.join("sovereign").join("skills");
        if candidate.exists() {
            return candidate;
        }
        let candidate2 = cwd.join("skills");
        if candidate2.exists() {
            return candidate2;
        }
        dirs::home_dir()
            .unwrap_or_default()
            .join(".sovereign")
            .join("skills")
    });
    Ok(Args {
        charter_path,
        script_path,
        feature_id,
        title,
        daemon_base,
        skills_dir,
        sample_size,
        chat_model,
        embed_model,
    })
}

fn print_help() {
    eprintln!(
        "Usage:\n  sovereign recipe-agent live-trial \\\n    \
         --charter <FILE> \\\n    \
         --script <FILE> \\\n    \
         [--feature-id <ID>] [--title <T>] \\\n    \
         [--daemon <URL>] [--skills-dir <D>] \\\n    \
         [--sample-size <N>] \\\n    \
         [--chat-model <ID>] [--embed-model <ID>]\n\n\
         Drive the recipe-author agent loop end-to-end against the \
         daemon's\n  /v1/chat/completions, then validate the generated \
         recipe and run\n  an initial fetch with --sample-size docs (default 50).\n\n\
         Script file format: one partner message per blank-line-separated \
         block.\n  Lines starting with # are comments and skipped.\n"
    );
}

// ─── Script file ─────────────────────────────────────────────────

fn parse_script(text: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.trim().is_empty() {
            if !cur.trim().is_empty() {
                blocks.push(cur.trim().to_string());
                cur.clear();
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    if !cur.trim().is_empty() {
        blocks.push(cur.trim().to_string());
    }
    blocks
}

// ─── Approval ────────────────────────────────────────────────────

/// Same auto-approve channel the chat REPL uses, repeated here so
/// this module doesn't depend on `chat_cmd::bootstrap` private items.
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

// ─── Inference provider — daemon over HTTP ───────────────────────

/// Mirror of `chat_cmd::bootstrap::SplitInferenceProvider`. Repeated
/// here rather than re-exported because `bootstrap`'s build_session
/// pulls in a lot of chat-specific scaffolding (atlas, wiki graph,
/// mesh knowledge) the trial harness doesn't want.
struct SplitInferenceProvider {
    chat: Arc<RemoteApiProvider>,
    embed: Arc<RemoteApiProvider>,
    chat_model_id: String,
}

impl SplitInferenceProvider {
    fn new(
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
    async fn complete(
        &self,
        request: &CompletionRequest,
    ) -> Result<CompletionResponse> {
        self.chat.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<String>> + Send>,
        >,
    > {
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
        self.chat_model_id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.chat.capabilities()
    }
}

async fn probe_daemon(base: &str) -> std::result::Result<(), String> {
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("http client build: {e}"))?;
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(format!(
            "daemon at {base} returned {} from /v1/models. \
             Is it really a sovereign daemon?",
            r.status()
        )),
        Err(_) => Err(format!(
            "daemon unreachable at {base}. \
             Start it with `sovereign daemon run`, or pass --daemon <URL>."
        )),
    }
}

async fn resolve_models(
    base: &str,
    args: &Args,
) -> std::result::Result<(String, String), String> {
    if let (Some(c), Some(e)) = (&args.chat_model, &args.embed_model) {
        return Ok((c.clone(), e.clone()));
    }
    // Prefer the daemon's setup config (the file the daemon actually
    // loaded). Falls back to /v1/models probe.
    if let Ok(cfg) = sovereign_core::setup_config::SetupConfig::load() {
        let chat_stem = cfg
            .models
            .primary
            .file_stem()
            .and_then(|s| s.to_str())
            .map(String::from);
        let embed_stem = cfg
            .models
            .embed
            .file_stem()
            .and_then(|s| s.to_str())
            .map(String::from);
        if let (Some(c), Some(e)) = (chat_stem, embed_stem) {
            return Ok((
                args.chat_model.clone().unwrap_or(c),
                args.embed_model.clone().unwrap_or(e),
            ));
        }
    }
    // Final fallback: probe /v1/models.
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET /v1/models: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse /v1/models: {e}"))?;
    let data = resp
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "/v1/models has no .data array".to_string())?;
    // Heuristics: pick the first non-embed-shaped id for chat, the
    // first embed-shaped id for embed.
    let chat = args
        .chat_model
        .clone()
        .or_else(|| {
            data.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
                .find(|id| !id.to_lowercase().contains("embed"))
                .map(String::from)
        })
        .ok_or_else(|| "no chat model on /v1/models".to_string())?;
    let embed = args
        .embed_model
        .clone()
        .or_else(|| {
            data.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
                .find(|id| id.to_lowercase().contains("embed"))
                .map(String::from)
        })
        .ok_or_else(|| "no embed model on /v1/models".to_string())?;
    Ok((chat, embed))
}

// ─── Trial entry ─────────────────────────────────────────────────

pub async fn run_live_trial(argv: &[String]) -> i32 {
    if argv.first().map(String::as_str) == Some("--help")
        || argv.first().map(String::as_str) == Some("-h")
    {
        print_help();
        return 0;
    }
    let args = match parse_args(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("recipe-agent live-trial: {e}");
            print_help();
            return 1;
        }
    };

    let charter = match std::fs::read_to_string(&args.charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "live-trial: failed to read charter {}: {e}",
                args.charter_path.display()
            );
            return 1;
        }
    };
    let script_text = match std::fs::read_to_string(&args.script_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "live-trial: failed to read script {}: {e}",
                args.script_path.display()
            );
            return 1;
        }
    };
    let messages = parse_script(&script_text);
    if messages.is_empty() {
        eprintln!("live-trial: script {} has no messages", args.script_path.display());
        return 1;
    }

    if let Err(e) = probe_daemon(&args.daemon_base).await {
        eprintln!("live-trial: {e}");
        return 2;
    }

    eprintln!("Daemon: {}", args.daemon_base);
    let v1 = format!("{}/v1", args.daemon_base);
    let (chat_model, embed_model) = match resolve_models(&args.daemon_base, &args).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("live-trial: {e}");
            return 2;
        }
    };
    eprintln!("Chat model:  {chat_model}");
    eprintln!("Embed model: {embed_model}");

    let inference: Arc<dyn InferenceProvider> = Arc::new(
        SplitInferenceProvider::new(&v1, chat_model, embed_model, 8192),
    );

    // Stores. We touch the user's real ~/.sovereign/{notes,features}.db
    // on purpose — a live trial against the running daemon is a real
    // session, not a sandbox. The harness is honest about its side
    // effects (project gets persisted, capability requests land in the
    // user's inbox). For a sandboxed run, point HOME at a tempdir.
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("live-trial: HOME not set");
            return 2;
        }
    };
    let dotsovereign = home.join(".sovereign");
    let notes = match NoteStore::open(&dotsovereign.join("notes.db")) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("live-trial: notes store: {e}");
            return 2;
        }
    };
    let features = match FeatureStore::open(&dotsovereign.join("features.db")) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("live-trial: feature store: {e}");
            return 2;
        }
    };

    let project = match args.feature_id.as_deref() {
        Some(fid) => {
            match RecipeProject::load(fid, Arc::clone(&notes), Arc::clone(&features))
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("live-trial: load project {fid}: {e}");
                    return 2;
                }
            }
        }
        None => {
            let title = args
                .title
                .clone()
                .or_else(|| {
                    args.charter_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "live-trial project".to_string());
            match RecipeProject::new(
                &title,
                &charter,
                Arc::clone(&notes),
                Arc::clone(&features),
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("live-trial: provision project: {e}");
                    return 2;
                }
            }
        }
    };
    eprintln!(
        "Project:     {} (feature_id={})",
        project.title(),
        project.feature_id()
    );
    eprintln!("Project dir: {}", project.project_dir().display());

    // Skills.
    let mut skills_reg = SkillRegistry::new();
    skills_reg.load_and_register(&args.skills_dir);
    skills_reg.activate("recipe-author");
    if !skills_reg
        .list()
        .iter()
        .any(|s| s.id == "recipe-author")
    {
        eprintln!(
            "live-trial: recipe-author skill not found under {}. \
             Did you copy or symlink sovereign/skills/recipe-author/ \
             to the skills dir?",
            args.skills_dir.display()
        );
        return 2;
    }
    eprintln!(
        "Skills:      {} loaded from {} (active: recipe-author)",
        skills_reg.list().len(),
        args.skills_dir.display()
    );
    let skills = Arc::new(skills_reg);

    // State store at <data>/sovereign.db. Use a per-trial sub-directory
    // so trial conversations don't pollute the desktop DB.
    let trial_data_dir = dotsovereign.join("recipe-agent-trial");
    if let Err(e) = std::fs::create_dir_all(&trial_data_dir) {
        eprintln!(
            "live-trial: failed to create {}: {e}",
            trial_data_dir.display()
        );
        return 2;
    }
    let store: Arc<dyn StateStore> = match SqliteStateStore::open(
        &trial_data_dir.join("sovereign.db"),
    ) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("live-trial: open trial state store: {e}");
            return 2;
        }
    };

    // CorpusEngine — needed by RecipeTestTool's underlying
    // `CorpusEngine::test_recipe`. Hardcode `~/.sovereign/{recipes,indexes}`.
    let recipes_dir = dotsovereign.join("recipes");
    let indexes_dir = dotsovereign.join("indexes");
    let _ = std::fs::create_dir_all(&recipes_dir);
    let _ = std::fs::create_dir_all(&indexes_dir);
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let inference_fn =
        sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(
            recipes_dir.clone(),
            indexes_dir,
            embed_fn,
        )
        .with_embedding_model(&embed_model_stem(&dotsovereign))
        .with_inference_fn(inference_fn),
    );

    // Tools — exactly the recipe-author surface plus web research.
    // No chat / claim / search / wiki tools; the live trial is
    // focused.
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(RecipeReadTool::new()));
    tools.register(Box::new(RecipeWriteTool::new()));
    tools.register(Box::new(RecipeValidateTool::new()));
    tools.register(Box::new(RecipeTestTool::new()));
    tools.register(Box::new(RegistryBrowseTool));
    tools.register(Box::new(DecisionLogTool::with_notes(Arc::clone(&notes))));
    tools.register(Box::new(CheckpointTool::with_stores(
        Arc::clone(&notes),
        Arc::clone(&features),
    )));
    tools.register(Box::new(CapabilityRequestTool::with_stores(
        Arc::clone(&notes),
        Arc::clone(&features),
    )));
    tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    tools.register(Box::new(sovereign_tools::search::SearchTool::with_web(
        Arc::clone(&store),
        Arc::clone(&inference),
        sovereign_tools::web::search::SearchBackend::DuckDuckGo,
    )));
    eprintln!("Tools:       {} registered", tools.count());

    let router: Box<dyn sovereign_core::traits::Router> = Box::new(
        LlmRouter::new(
            Arc::clone(&inference),
            Arc::clone(&store),
            Arc::clone(&skills),
        ),
    );
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
    let approval: Arc<dyn ApprovalChannel> = Arc::new(AutoApprove);

    let runtime = Runtime::new(
        Arc::clone(&inference),
        router,
        Box::new(planner),
        Arc::new(tools),
        Arc::clone(&store),
        Arc::clone(&skills),
        approval,
        InferenceConfig::default(),
    )
    .with_corpus_engine(Arc::clone(&corpus_engine));

    // Drive turns.
    let conversation_id = uuid::Uuid::new_v4().to_string();
    eprintln!(
        "\nDriving {} partner turn(s) on conversation {}\n",
        messages.len(),
        conversation_id
    );
    let mut tool_calls_per_turn: Vec<usize> = Vec::with_capacity(messages.len());
    let mut last_tool_call_count = match notes.tool_call_log_rows(10_000, 0).await {
        Ok(rows) => rows.len(),
        Err(_) => 0,
    };
    for (i, msg) in messages.iter().enumerate() {
        eprintln!("──── Turn {} ─────────────────────────────────", i + 1);
        eprintln!("Partner: {msg}\n");
        let situated = match situated_context::render(&project).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("live-trial: situated render failed: {e}");
                return 2;
            }
        };
        let envelope = situated_context::compose_envelope(&situated, msg);
        let started = std::time::Instant::now();
        match runtime
            .handle_message(&envelope, &conversation_id)
            .await
        {
            Ok(resp) => {
                let elapsed = started.elapsed();
                eprintln!(
                    "Agent ({:.1}s): {}\n",
                    elapsed.as_secs_f32(),
                    resp.message.content
                );
            }
            Err(e) => {
                eprintln!("live-trial: turn {} failed: {e}\n", i + 1);
                return 3;
            }
        }
        let now_count = match notes.tool_call_log_rows(10_000, 0).await {
            Ok(rows) => rows.len(),
            Err(_) => last_tool_call_count,
        };
        let calls_this_turn = now_count.saturating_sub(last_tool_call_count);
        tool_calls_per_turn.push(calls_this_turn);
        last_tool_call_count = now_count;
        eprintln!("(tool calls this turn: {calls_this_turn})\n");
    }

    // ─── Post-trial assertions ────────────────────────────────
    eprintln!("──── Post-trial summary ─────────────────────────");
    let summary = match project.read_summary() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("live-trial: read summary: {e}");
            return 4;
        }
    };
    let scope = ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(project.feature_id().to_string()),
    };
    let decisions = notes
        .read_notes_scoped(
            None,
            &[],
            &[],
            &["decision".to_string()],
            1000,
            false,
            &scope,
        )
        .await
        .unwrap_or_default();
    let checkpoints = project.list_checkpoints().unwrap_or_default();
    let cap_requests = notes
        .read_notes_scoped(
            None,
            &[],
            &[],
            &["capability_request".to_string()],
            100,
            false,
            &scope,
        )
        .await
        .unwrap_or_default();
    let total_tool_calls: usize = tool_calls_per_turn.iter().sum();

    eprintln!("Recipe id:           {:?}", summary.recipe_id);
    eprintln!("Decisions logged:    {}", decisions.len());
    eprintln!("Checkpoints:         {}", checkpoints.len());
    eprintln!("Capability requests: {}", cap_requests.len());
    eprintln!("Total tool calls:    {total_tool_calls}");

    // The agent might not have written a recipe — that's an outcome,
    // not a fatal error. We still validate + try the test fetch when
    // a recipe id is set, and surface a clear miss otherwise.
    let mut overall_pass = true;
    if let Some(recipe_id) = summary.recipe_id.as_deref() {
        eprintln!("\nValidating {} …", recipe_id);
        let validate_tool = RecipeValidateTool::with_recipes_dir(recipes_dir.clone());
        let validate_ctx = sovereign_core::types::ToolContext {
            conversation_id: conversation_id.clone(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
        };
        match validate_tool
            .execute(
                &serde_json::json!({"path": recipe_id}),
                &validate_ctx,
            )
            .await
        {
            Ok(StepOutput::Json(v)) => {
                let passed = v.get("passed").and_then(|p| p.as_bool()).unwrap_or(false);
                if passed {
                    eprintln!("  validate: PASS");
                } else {
                    let errs = v
                        .get("errors")
                        .and_then(|e| e.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    eprintln!("  validate: FAIL ({errs} error[s])");
                    overall_pass = false;
                }
            }
            Ok(other) => {
                eprintln!("  validate: unexpected output {other:?}");
                overall_pass = false;
            }
            Err(e) => {
                eprintln!("  validate: error {e}");
                overall_pass = false;
            }
        }

        eprintln!(
            "\nFetching initial sample (sample_size={}) …",
            args.sample_size
        );
        let test_tool = RecipeTestTool::with_recipes_dir(recipes_dir.clone());
        match test_tool
            .execute(
                &serde_json::json!({
                    "path": recipe_id,
                    "sample_size": args.sample_size,
                }),
                &validate_ctx,
            )
            .await
        {
            Ok(StepOutput::Json(v)) => {
                let attempted = v
                    .get("extraction")
                    .and_then(|e| e.get("records_attempted"))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let succeeded = v
                    .get("extraction")
                    .and_then(|e| e.get("records_succeeded"))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let rate = v
                    .get("extraction")
                    .and_then(|e| e.get("extraction_rate"))
                    .and_then(|n| n.as_f64())
                    .unwrap_or(0.0);
                eprintln!(
                    "  test: attempted={attempted} succeeded={succeeded} rate={rate:.2}"
                );
                if succeeded == 0 {
                    eprintln!("  test: FAIL — zero docs extracted");
                    overall_pass = false;
                } else {
                    eprintln!("  test: PASS");
                }
            }
            Ok(other) => {
                eprintln!("  test: unexpected output {other:?}");
                overall_pass = false;
            }
            Err(e) => {
                eprintln!("  test: error {e}");
                overall_pass = false;
            }
        }
    } else {
        eprintln!("\n(no recipe drafted — agent didn't reach recipe_write)");
        overall_pass = false;
    }

    if decisions.is_empty() {
        eprintln!(
            "\nWarning: no decisions logged. The agent should be calling \
             DecisionLog on every non-trivial choice."
        );
    }
    if checkpoints.is_empty() {
        eprintln!(
            "Warning: no checkpoints created. The agent should have \
             checkpointed at project creation at minimum."
        );
    }

    if overall_pass {
        eprintln!("\nLive trial: PASS");
        0
    } else {
        eprintln!("\nLive trial: FAIL");
        4
    }
}

fn embed_model_stem(_dotsovereign: &Path) -> String {
    sovereign_core::setup_config::SetupConfig::load()
        .ok()
        .and_then(|c| {
            c.models
                .embed
                .file_stem()
                .and_then(|s| s.to_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "qwen3-embedding-0.6b".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_script_skips_comments_and_blank_lines() {
        let text = "# header\n\nFirst message.\n\n# mid comment\nSecond,\nstill second.\n\nThird.\n";
        let blocks = parse_script(text);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], "First message.");
        assert_eq!(blocks[1], "Second,\nstill second.");
        assert_eq!(blocks[2], "Third.");
    }

    #[test]
    fn parse_script_handles_empty_input() {
        assert!(parse_script("").is_empty());
        assert!(parse_script("# only a comment\n").is_empty());
        assert!(parse_script("\n\n\n").is_empty());
    }

    #[test]
    fn parse_script_trims_trailing_whitespace() {
        let blocks = parse_script("Just one line.   \n\n");
        assert_eq!(blocks, vec!["Just one line."]);
    }
}
