use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::router::LlmRouter;
use sovereign_core::runtime::Runtime;
use sovereign_core::stubs::{NoOpPlanner, PassthroughRouter};
use sovereign_core::traits::StateStore;
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_store::sqlite::SqliteStateStore;

struct Args {
    model: PathBuf,
    primary_model: Option<PathBuf>,
    data_dir: PathBuf,
    use_router: bool,
}

fn print_usage() {
    eprintln!("Usage: sovereign-cli --model <path.gguf> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --model <path>           Fast/default model (required)");
    eprintln!("  --primary-model <path>   Larger model for deep reasoning");
    eprintln!("  --data-dir <path>        Database directory (default: data)");
    eprintln!("  --router                 Enable LLM-based intent routing");
}

fn parse_args() -> Option<Args> {
    let args: Vec<String> = std::env::args().collect();
    let mut model = None;
    let mut primary_model = None;
    let mut data_dir = None;
    let mut use_router = false;

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
        use_router,
    })
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Some(a) => a,
        None => {
            print_usage();
            std::process::exit(1);
        }
    };

    // Load inference provider.
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

    // Start idle monitor for primary slot (60s timeout).
    if args.primary_model.is_some() {
        inference.start_idle_monitor(60);
    }

    // Open database.
    let db_path = args.data_dir.join("sovereign.db");
    eprintln!("Database: {}", db_path.display());
    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("Failed to open database: {e}");
            std::process::exit(1);
        }
    };

    // Build router.
    let router: Box<dyn sovereign_core::traits::Router> = if args.use_router {
        eprintln!("Router: LLM-based classification enabled");
        Box::new(LlmRouter::new(Arc::clone(&inference) as Arc<dyn sovereign_core::traits::InferenceProvider>))
    } else {
        eprintln!("Router: passthrough (all messages → SimpleQuery)");
        Box::new(PassthroughRouter)
    };

    let runtime = Runtime::new(
        inference as Arc<dyn sovereign_core::traits::InferenceProvider>,
        router,
        Box::new(NoOpPlanner),
        ToolRegistry::new(),
        store.clone(),
        SkillRegistry::new(),
    );

    // Resume last conversation or start a new one.
    let conversation_id = match store.list_conversations(1, 0).await {
        Ok(convos) if !convos.is_empty() => {
            let id = convos[0].id.clone();
            eprintln!("Resuming conversation");
            id
        }
        _ => {
            let id = uuid::Uuid::new_v4().to_string();
            eprintln!("Starting new conversation");
            id
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
            break;
        }

        match runtime.handle_message(input, &conversation_id).await {
            Ok(response) => {
                println!("\n{}\n", response.message.content);
            }
            Err(e) => {
                eprintln!("Error: {e}\n");
            }
        }
    }
}
