use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::planner::LlmPlanner;
use sovereign_core::router::LlmRouter;
use sovereign_core::runtime::Runtime;
use sovereign_core::stubs::PassthroughRouter;
use sovereign_core::traits::{ApprovalChannel, StateStore};
use sovereign_core::types::*;
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::shell::ShellTool;

// ─── CLI Approval Channel ──────────────────────────────────────

struct CliApprovalChannel {
    store: Arc<dyn StateStore>,
}

impl CliApprovalChannel {
    fn new(store: Arc<dyn StateStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ApprovalChannel for CliApprovalChannel {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool> {
        eprintln!();
        eprintln!("  [APPROVAL REQUIRED]");
        eprintln!("  Step {}: {}", step.id, step.description);
        eprintln!("  Tool: {}", preview.tool_id);
        eprintln!("  Action: {}", preview.description);
        if let Ok(params_str) = serde_json::to_string_pretty(&preview.params) {
            eprintln!("  Params: {params_str}");
        }

        loop {
            eprint!("  Allow? [yes/no/always] ");
            io::stderr().flush().unwrap_or(());

            let mut answer = String::new();
            if io::stdin().lock().read_line(&mut answer).unwrap_or(0) == 0 {
                return Ok(false);
            }

            match answer.trim().to_lowercase().as_str() {
                "yes" | "y" => return Ok(true),
                "no" | "n" => return Ok(false),
                "always" | "a" => {
                    // Persist permission for future sessions.
                    let _ = self
                        .store
                        .set_permission(&preview.tool_id, "Shell", true)
                        .await;
                    eprintln!("  Permission saved for future sessions.");
                    return Ok(true);
                }
                _ => eprintln!("  Please answer yes, no, or always"),
            }
        }
    }

    async fn ask_user(&self, question: &str) -> Result<String> {
        eprintln!("\n  [INPUT NEEDED] {question}");
        eprint!("  > ");
        io::stderr().flush().unwrap_or(());

        let mut answer = String::new();
        if io::stdin().lock().read_line(&mut answer).unwrap_or(0) == 0 {
            return Err(Error::Cancelled);
        }
        Ok(answer.trim().to_string())
    }

    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        let status = match output {
            StepOutput::Text(_) | StepOutput::Json(_) => "done",
            StepOutput::Jump(t) => {
                eprintln!("  [step {}] {} → jump to {t}", step.id, step.description);
                return;
            }
            StepOutput::Skipped => "skipped",
        };
        eprintln!("  [step {}] {} [{status}]", step.id, step.description);
    }
}

// ─── Args ──────────────────────────────────────────────────────

struct Args {
    model: PathBuf,
    primary_model: Option<PathBuf>,
    data_dir: PathBuf,
    skills_dir: Option<PathBuf>,
    use_router: bool,
    ingest: Option<PathBuf>,
    brave_api_key: Option<String>,
    tavily_api_key: Option<String>,
}

fn print_usage() {
    eprintln!("Usage: sovereign-cli --model <path.gguf> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --model <path>           Fast/default model (required)");
    eprintln!("  --primary-model <path>   Larger model for deep reasoning");
    eprintln!("  --data-dir <path>        Database directory (default: data)");
    eprintln!("  --skills-dir <path>      Skills directory (default: ~/.sovereign/skills)");
    eprintln!("  --ingest <path>          Ingest documents from directory");
    eprintln!("  --router                 Enable LLM-based intent routing");
    eprintln!("  --brave-api-key <key>    Use Brave Search (better quality)");
    eprintln!("  --tavily-api-key <key>   Use Tavily Search (best quality)");
}

fn parse_args() -> Option<Args> {
    let args: Vec<String> = std::env::args().collect();
    let mut model = None;
    let mut primary_model = None;
    let mut data_dir = None;
    let mut skills_dir = None;
    let mut use_router = false;
    let mut ingest = None;
    let mut brave_api_key = None;
    let mut tavily_api_key = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                model = args.get(i).map(PathBuf::from);
            }
            "--primary-model" => {
                i += 1;
                primary_model = args.get(i).map(PathBuf::from);
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            "--skills-dir" => {
                i += 1;
                skills_dir = args.get(i).map(PathBuf::from);
            }
            "--ingest" => {
                i += 1;
                ingest = args.get(i).map(PathBuf::from);
            }
            "--brave-api-key" => {
                i += 1;
                brave_api_key = args.get(i).cloned();
            }
            "--tavily-api-key" => {
                i += 1;
                tavily_api_key = args.get(i).cloned();
            }
            "--router" => {
                use_router = true;
            }
            _ => {}
        }
        i += 1;
    }

    Some(Args {
        model: model?,
        primary_model,
        data_dir: data_dir.unwrap_or_else(|| PathBuf::from("data")),
        skills_dir,
        use_router,
        ingest,
        brave_api_key,
        tavily_api_key,
    })
}

// ─── Main ──────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Some(a) => a,
        None => {
            print_usage();
            std::process::exit(1);
        }
    };

    // Load inference.
    eprintln!("Loading model: {}", args.model.display());
    if let Some(ref p) = args.primary_model {
        eprintln!("Primary model: {}", p.display());
    }

    let inference = match EmbeddedLlamaCpp::load_dual(
        &args.model,
        args.primary_model.as_deref(),
        2048,
        None,
    ) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("Failed to load model: {e}");
            std::process::exit(1);
        }
    };

    if args.primary_model.is_some() {
        inference.start_idle_monitor(60);
    }

    // Open database.
    let db_path = args.data_dir.join("sovereign.db");
    eprintln!("Database: {}", db_path.display());
    let store: Arc<dyn StateStore> = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("Failed to open database: {e}");
            std::process::exit(1);
        }
    };

    // Build components.
    let inference_arc: Arc<dyn sovereign_core::traits::InferenceProvider> = inference;

    // Load skills.
    let mut skills = SkillRegistry::new();
    let skills_dir = args.skills_dir.unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".sovereign")
            .join("skills")
    });
    if skills_dir.exists() {
        skills.load_and_register(&skills_dir);
        skills.activate_all();
        eprintln!("Skills: {} loaded from {}", skills.list().len(), skills_dir.display());
    } else {
        eprintln!("Skills: none ({})", skills_dir.display());
    }
    // Also check for bundled skills next to the binary.
    let bundled_skills = std::env::current_dir()
        .unwrap_or_default()
        .join("skills");
    if bundled_skills.exists() && bundled_skills != skills_dir {
        skills.load_and_register(&bundled_skills);
        skills.activate_all();
        eprintln!("Skills: +{} bundled", skills.list().len());
    }
    let skills = Arc::new(skills);

    let router: Box<dyn sovereign_core::traits::Router> = if args.use_router {
        eprintln!("Router: LLM-based classification");
        Box::new(LlmRouter::new(
            Arc::clone(&inference_arc),
            Arc::clone(&store),
            Arc::clone(&skills),
        ))
    } else {
        eprintln!("Router: passthrough");
        Box::new(PassthroughRouter)
    };

    let planner = LlmPlanner::new(Arc::clone(&inference_arc), Arc::clone(&skills));

    // Ingest documents if requested.
    if let Some(ref ingest_path) = args.ingest {
        eprintln!("Ingesting documents from: {}", ingest_path.display());
        match sovereign_tools::rag::ingest::ingest_directory(
            ingest_path,
            store.as_ref(),
            Some(inference_arc.as_ref()),
        )
        .await
        {
            Ok(result) => {
                eprintln!(
                    "Ingestion complete: {} files, {} chunks ({} skipped)",
                    result.files_processed, result.chunks_created, result.files_skipped,
                );
            }
            Err(e) => {
                eprintln!("Ingestion failed: {e}");
            }
        }
    }

    // Register tools.
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(sovereign_tools::document::DocumentTool::new(
        Arc::clone(&store),
        Arc::clone(&inference_arc),
    )));
    tools.register(Box::new(sovereign_tools::knowledge::KnowledgeTool::new(
        Arc::clone(&store),
        Arc::clone(&inference_arc),
    )));
    // Select search backend: Tavily > Brave > DuckDuckGo (free default).
    let search_backend = if let Some(ref key) = args.tavily_api_key {
        eprintln!("Search: Tavily");
        sovereign_tools::web::search::SearchBackend::Tavily {
            api_key: key.clone(),
        }
    } else if let Some(ref key) = args.brave_api_key {
        eprintln!("Search: Brave");
        sovereign_tools::web::search::SearchBackend::Brave {
            api_key: key.clone(),
        }
    } else {
        eprintln!("Search: DuckDuckGo (free)");
        sovereign_tools::web::search::SearchBackend::DuckDuckGo
    };
    tools.register(Box::new(sovereign_tools::web::WebSearchTool::with_backend(
        Arc::clone(&inference_arc),
        search_backend,
    )));
    tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    eprintln!("Tools: {} registered", tools.count());

    let approval = Arc::new(CliApprovalChannel::new(Arc::clone(&store)));

    let runtime = Runtime::new(
        inference_arc,
        router,
        Box::new(planner),
        Arc::new(tools),
        store.clone(),
        skills,
        approval,
    );

    // Resume or start conversation.
    let conversation_id = match store.list_conversations(1, 0).await {
        Ok(convos) if !convos.is_empty() => {
            eprintln!("Resuming conversation");
            convos[0].id.clone()
        }
        _ => {
            eprintln!("Starting new conversation");
            uuid::Uuid::new_v4().to_string()
        }
    };

    eprintln!("Ready. Type a message (or \"quit\" to exit).\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("> ");
        stdout.flush().unwrap();

        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap() == 0 {
            break;
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == "quit" || input == "exit" {
            eprintln!("Extracting memories...");
            let _ = runtime.end_conversation(&conversation_id).await;
            break;
        }

        match runtime.handle_message(input, &conversation_id).await {
            Ok(response) => {
                if let Some(ref task) = response.task {
                    eprintln!(
                        "\n[task] {} steps, status: {:?}",
                        task.completed_steps.len(),
                        task.status,
                    );
                }
                println!("\n{}\n", response.message.content);
            }
            Err(e) => {
                eprintln!("Error: {e}\n");
            }
        }
    }
}
