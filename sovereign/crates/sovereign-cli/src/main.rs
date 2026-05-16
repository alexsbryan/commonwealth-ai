mod amend;
mod amend_cmd;
mod atlas_cmd;
mod atos_cmd;
mod atos_plugin;
mod audit_cmd;
mod audit_extract;
mod audit_recover;
#[cfg(feature = "dev-tools")]
mod awareness_cmd;
mod bench_cmd;
mod charter_cmd;
mod chat_cmd;
mod claim_cmd;
mod code_cmd;
mod daemon_cmd;
mod design_cmd;
mod design_onboarding;
mod design_session;
mod archaeology_eval_cmd;
mod drift_cmd;
mod drift_cmd_orchestrator;
mod git_archaeology_cmd;
mod enrich_cmd;
mod eval_cmd;
mod corpus_catalog_cmd;
mod meta_atlas_cmd;
mod corpus_snapshot_cmd;
mod corpus_watch_cmd;
mod doc_fetcher;
mod alignment_cmd;
mod doctor_cmd;
mod found;
mod honesty;
mod init;
mod install_service_cmd;
mod mcp_cmd;
mod mesh_cmd;
mod milestone_cmd;
mod newsworthy_cmd;
mod notes_cmd;
mod observation;
mod phases;
mod plan_cmd;
mod plan_composer;
mod plan_enricher;
mod project_cmd;
mod project_toml;
mod reading_diag_cmd;
mod recipe_agent_cmd;
mod recipe_agent_live_trial;
mod pipeline_cmd;
mod recipe_cmd;
mod refresh_cmd;
mod reflect_cmd;
mod rough_edges_cmd;
mod serve_cmd;
mod service_install;
mod setup_cmd;
mod setup_config;
mod status_cmd;
mod stop_cmd;
mod tools_cmd;
mod util;
mod voice_eval;

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
            StepOutput::Text(_) | StepOutput::Json(_) | StepOutput::ReasonWithToolsResult { .. } => "done",
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
    /// Whether the KnowledgeView landscape-digest feature is active.
    /// Default `true`; `--no-knowledge-view` on the command line flips
    /// it to `false`. When disabled, the CLI skips the three enriched
    /// views + cross-view resonance, matching the desktop app's
    /// Settings → Knowledge toggle and the server's
    /// `[knowledge_view] enabled = false` config.
    knowledge_view_enabled: bool,
}

use crate::util::help::{Help, HelpSection};

/// Top-level help: lists every subcommand plus the flags for the
/// fall-through interactive REPL mode (what runs when no subcommand
/// is given). The subcommand table points users at the modern flow
/// (setup / project / mesh) rather than the legacy REPL.
const HELP: Help = Help {
    command: "sovereign",
    summary: "Local AI assistant with code intelligence, knowledge bases, and an optional mesh.",
    sections: &[
        HelpSection::Usage(
            "sovereign <subcommand> [flags]\n\
             sovereign --model <path.gguf> [options]   (legacy interactive REPL)",
        ),
        HelpSection::Subcommands(&[
            ("setup",   "First-run: detect hardware, download models, start daemon"),
            ("chat",    "CLI mirror of the desktop chat flow (ask / session / inspect)"),
            ("project", "Per-project code intelligence (init / serve / status / refresh)"),
            ("mesh",    "Mesh management (create / join / rotate / status)"),
            ("alignment", "Mesh-replicated workspace migrate / status (~/.claude + notes.db)"),
            ("corpus",  "Knowledge corpus install / remove / status"),
            ("code",    "Code intelligence tooling (index / watch / mcp-status)"),
            ("doctor",  "Diagnose setup and daemon health"),
            ("reflect", "Review session reflections; retire fixed ones"),
            ("recipe",  "Run a corpus ingestion recipe"),
            ("pipeline", "Generic ingestion driver — durable worklist + retry + pause-resume"),
            ("bench",   "Throughput + correctness benchmarks for enrichment LLM tasks"),
            ("atlas",   "Atlas-style structural enrichment (Wikipedia link graph today)"),
            ("eval",    "Run a question bank against a corpus; measure retrieval quality"),
            ("tools",   "Invoke code-intelligence tools from the CLI (list / describe / call)"),
            ("mcp",     "MCP server diagnostics (list tools, proxy)"),
            ("daemon",  "(internal) Long-running service managed by launchd/systemd"),
        ]),
        HelpSection::Flags(&[
            ("--model <path>",         "Quick responder GGUF (REPL mode only)"),
            ("--primary-model <path>", "Main responder GGUF (REPL, lazy-loaded)"),
            ("--data-dir <path>",      "Database directory (default: data)"),
            ("--skills-dir <path>",    "Skills directory (default: ~/.sovereign/skills)"),
            ("--ingest <path>",        "Ingest documents from directory before REPL"),
            ("--router",               "Enable LLM-based intent routing"),
            ("--no-knowledge-view",    "Disable KnowledgeView landscape digests (default: enabled)"),
            ("--brave-api-key <key>",  "Brave Search key (optional)"),
            ("--tavily-api-key <key>", "Tavily Search key (optional)"),
            ("--help, -h",             "Show this message"),
        ]),
        HelpSection::Notes(
            "Run `sovereign <subcommand> --help` for detail on any specific subcommand.",
        ),
    ],
};

fn print_usage() {
    crate::util::help::print(&HELP);
}

/// `sovereign nudge dismiss <id>` — record a nudge id in
/// `~/.sovereign/dismissed_nudges.json` so the audit / status
/// surfaces stop showing it. The id can be a family name (e.g.
/// `recipe-publish`) to dismiss every variant, or a specific
/// instance (e.g. `recipe-publish:sec-investigation`) to dismiss
/// just that one.
async fn run_nudge(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: sovereign nudge dismiss <id>");
        eprintln!("Example: sovereign nudge dismiss recipe-publish");
        return 2;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        println!(
            "Usage: sovereign nudge <subcommand> [args]\n\n\
             Subcommands:\n\
               dismiss <id>   Suppress a nudge id (family or specific instance).\n\n\
             Examples:\n\
               sovereign nudge dismiss recipe-publish\n\
               sovereign nudge dismiss recipe-publish:sec-investigation\n"
        );
        return 0;
    }
    match args[0].as_str() {
        "dismiss" => {
            let Some(id) = args.get(1) else {
                eprintln!("error: `nudge dismiss` requires a nudge id");
                return 2;
            };
            match record_dismissed_nudge(id) {
                Ok(_) => {
                    println!("Dismissed nudge: `{id}`");
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
        other => {
            eprintln!("Unknown nudge subcommand: {other}");
            1
        }
    }
}

/// Append `id` to `~/.sovereign/dismissed_nudges.json`. Idempotent:
/// re-dismissing an already-dismissed id is a no-op. The file is
/// a flat JSON array of strings; created on first dismissal.
fn record_dismissed_nudge(id: &str) -> std::io::Result<()> {
    let root = crate::util::dirs::sovereign_root();
    std::fs::create_dir_all(&root)?;
    let path = root.join("dismissed_nudges.json");
    let mut current: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !current.iter().any(|x| x == id) {
        current.push(id.to_string());
    }
    let bytes = serde_json::to_vec_pretty(&current)?;
    std::fs::write(&path, bytes)?;
    Ok(())
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
    let mut knowledge_view_enabled = true;

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
            "--no-knowledge-view" => {
                knowledge_view_enabled = false;
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
        knowledge_view_enabled,
    })
}

// ─── Main ──────────────────────────────────────────────────────

/// Entry point. Builds the tokio runtime explicitly (rather than via
/// `#[tokio::main]`) so we can hand the multi-thread executor an 8 MiB
/// per-worker stack and install a panic hook that survives a worker
/// abort.
///
/// Why this matters: tokio spawns its worker threads through
/// `pthread_create` with an explicit stack size that defaults to 2 MiB
/// on macOS. `RUST_MIN_STACK` only influences `std::thread::Builder`,
/// not pthread-created tokio workers. Drift-detect load reproducibly
/// overflowed those 2 MiB stacks (77 overflows / 166 daemon starts on
/// 2026-05-12) and the daemon died via SIGABRT with no backtrace
/// because the panic hook ran on an already-corrupted stack frame.
///
/// The panic hook below routes panic info through both `tracing::error!`
/// (so launchd/systemd log pipelines and the daemon.err tail see it)
/// AND `eprintln!` (so it lands even if tracing isn't initialized yet,
/// e.g. during the wizard or before `init_tracing`). Both paths print
/// the full backtrace when `RUST_BACKTRACE=full` is set.
fn main() {
    // Set the diagnostic env vars BEFORE the tokio runtime is built —
    // any worker thread spawned afterwards reads them at panic time.
    // (`RUST_MIN_STACK` won't propagate to tokio workers but does help
    // plain `std::thread::Builder::spawn` calls — rayon, blocking-pool
    // threads, etc.)
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }

    // Global panic hook. Captured panic info goes through both
    // `tracing::error!` and `eprintln!` so the line lands regardless
    // of whether tracing-subscriber is wired yet. Without this hook
    // tokio's default behaviour writes panic frames straight to stderr
    // bypassing the structured-logging layer, and on a worker abort
    // (e.g. stack overflow → SIGABRT) the line never appears at all.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload: &str = if let Some(s) = info.payload().downcast_ref::<&'static str>() {
            s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "<non-string panic payload>"
        };
        // eprintln first — survives before/after tracing setup.
        eprintln!(
            "sovereign panic at {location}: {payload}\nbacktrace:\n{backtrace}"
        );
        tracing::error!(
            location = %location,
            payload = %payload,
            backtrace = %backtrace,
            "sovereign panic — see backtrace above"
        );
        // Chain to the previous hook so any installed test harness /
        // tracing layer still sees the panic.
        prev_hook(info);
    }));

    // Explicit multi-thread runtime with 8 MiB worker stacks. 8 MiB is
    // the same headroom Cargo's build worker threads use and matches
    // what corpus-engine's tree-sitter path needs on deeply-nested
    // wikitext templates. Use `enable_all` to match `#[tokio::main]`'s
    // default feature set (IO + time drivers).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-rt-worker")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(async_main());
}

async fn async_main() {
    // Check for subcommands before standard arg parsing.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();

    // Top-level --help / help short-circuit. A lone "help" (no
    // subcommand) prints the banner; `sovereign mesh --help` is
    // handled by the subcommand dispatcher below.
    if let Some(first) = raw_args.first() {
        if matches!(first.as_str(), "--help" | "-h" | "help") && raw_args.len() == 1 {
            print_usage();
            std::process::exit(0);
        }
    }

    if let Some(first) = raw_args.first() {
        match first.as_str() {
            "mesh" => {
                // The mesh subcommand drives network I/O whose only
                // user-visible signal is tracing output. Without a
                // subscriber, `tracing::info!("handshake_sent …")`
                // vanishes into the void and mesh failures look
                // identical from outside. Honour RUST_LOG if set,
                // otherwise show all mesh-layer info lines.
                util::tracing_init::init_tracing(
                    "sovereign_cli=info,\
                     sovereign_mesh=info,\
                     commonwealth_discovery=info,\
                     commonwealth_api=info",
                );
                let code = mesh_cmd::run_mesh(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "alignment" => {
                let code = alignment_cmd::run_alignment(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "corpus" => {
                let code = mesh_cmd::run_corpus(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "meta-atlas" => {
                let code = meta_atlas_cmd::run_meta_atlas(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "mcp" => {
                let code = mcp_cmd::run_mcp(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "recipe" => {
                let code = recipe_cmd::run_recipe(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "pipeline" => {
                util::tracing_init::init_tracing("sovereign_cli=info,sovereign_pipeline=info");
                let code = pipeline_cmd::run_pipeline(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "recipe-agent" => {
                let code = recipe_agent_cmd::run_recipe_agent(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "maintainer" => {
                let code = recipe_agent_cmd::run_maintainer(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "code" => {
                let code = code_cmd::run_code(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "init" => {
                // Top-level `sovereign init` — replaces
                // `sovereign project init`. The old name continues
                // to work via the alias arm in
                // `project_cmd::run_project`.
                let code = init::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "status" => {
                let code = status_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "audit" => {
                let code = audit_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "milestone" => {
                let code = milestone_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "notes" => {
                let code = notes_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "drift" => {
                let code = drift_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "rough-edges" => {
                let code = rough_edges_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "git-archaeology" => {
                let code = git_archaeology_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "archaeology-eval" => {
                let code = archaeology_eval_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "charter" => {
                let code = charter_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "claim" => {
                let code = claim_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "amend" => {
                let code = amend_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "design" => {
                let code = design_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "plan" => {
                let code = plan_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "serve" => {
                let code = serve_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "stop" => {
                let code = stop_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "install-service" => {
                // Phase 4: explicit service registration. Pre-Phase-4
                // this happened implicitly inside `sovereign setup`;
                // splitting it lets users run the daemon foreground
                // first and register only when they're ready.
                let code = install_service_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "refresh" => {
                let code = refresh_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "project" => {
                let code = project_cmd::run_project(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "reflect" => {
                let code = reflect_cmd::run_reflect(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "atos" => {
                let code = atos_cmd::run_atos(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "awareness" => {
                #[cfg(feature = "dev-tools")]
                {
                    util::tracing_init::init_tracing(
                        "sovereign_cli=info,sovereign_tools=debug,corpus_engine=debug",
                    );
                    let code = awareness_cmd::run_awareness(&raw_args[1..]).await;
                    std::process::exit(code);
                }
                #[cfg(not(feature = "dev-tools"))]
                {
                    eprintln!(
                        "awareness: this subcommand is gated behind the `dev-tools` cargo\n\
                         feature. Rebuild with `cargo build --features dev-tools` to enable."
                    );
                    std::process::exit(2);
                }
            }
            "tools" => {
                let code = tools_cmd::run_tools(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "enrich" => {
                // Phase 1 / atlas runs spend minutes inside one
                // `/v1/chat/completions` call. Without a subscriber the
                // inference client's heartbeat + per-request timing
                // logs silently disappear and a stuck call looks
                // identical to a slow one.
                util::tracing_init::init_tracing(
                    "sovereign_cli=info,\
                     corpus_engine=info",
                );
                let code = enrich_cmd::run_enrich(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "atlas" => {
                let code = atlas_cmd::run_atlas(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "eval" => {
                let code = eval_cmd::run_eval(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "voice" => {
                util::tracing_init::init_tracing("sovereign_cli=info");
                let code = voice_eval::run_voice_eval(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "bench" => {
                let code = bench_cmd::run_bench(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "chat" => {
                let code = chat_cmd::run_chat(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "reading-diag" => {
                // No tracing init — reading-diag is meant to be
                // pipeable (`--format json | jq ...`), so we keep
                // stdout clean. Set RUST_LOG manually if you want
                // corpus-engine internals on stderr.
                let code = reading_diag_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "nudge" => {
                let code = run_nudge(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "doctor" => {
                let code = doctor_cmd::run_doctor(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "newsworthy" => {
                let code = newsworthy_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "setup" => {
                util::tracing_init::init_tracing("sovereign_cli=info");
                let code = setup_cmd::run_setup(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "daemon" => {
                // The daemon run loop depends on tracing for visibility
                // into model load, mesh resume, and gossip. Initialize a
                // subscriber up front so launchd/systemd can tail it.
                //
                // `corpus_engine=info` is load-bearing for the
                // wikipedia-newsworthy watcher specifically: its
                // tick / portal-ingest / tracked-state events live in
                // `corpus_engine::update::newsworthy_watcher`, and
                // without this entry every `newsworthy.tick` /
                // `newsworthy.portal_ingested` line was being dropped
                // at the subscriber — masking whether the watcher was
                // actually running.
                util::tracing_init::init_tracing(
                    "sovereign_cli=info,\
                     sovereign_mesh=info,\
                     sovereign_inference=info,\
                     corpus_engine=info,\
                     commonwealth_discovery=info,\
                     commonwealth_api=info",
                );
                let code = daemon_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            _ => {}
        }
    }

    let args = match parse_args() {
        Some(a) => a,
        None => {
            print_usage();
            std::process::exit(1);
        }
    };

    // Load inference.
    eprintln!("Quick responder: {}", args.model.display());
    if let Some(ref p) = args.primary_model {
        eprintln!("Main responder:  {}", p.display());
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

    // Open database. Two handles: the concrete `Arc<SqliteStateStore>`
    // used below to install the KnowledgeView manager as the store's
    // observer, and the `Arc<dyn StateStore>` used by the runtime +
    // tools. Both point at the same store.
    let db_path = args.data_dir.join("sovereign.db");
    eprintln!("Database: {}", db_path.display());
    let store_concrete: Arc<SqliteStateStore> = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("Failed to open database: {e}");
            std::process::exit(1);
        }
    };
    let store: Arc<dyn StateStore> = store_concrete.clone();

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

    // Construct a shared CorpusEngine for the epistemic tools.
    // The recipes dir is where user-supplied recipes live; the index dir
    // is the shared on-disk corpus directory used by both Sovereign and
    // Commonwealth.
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let recipes_dir = home.join(".sovereign").join("recipes");
    let indexes_dir = home.join(".sovereign").join("indexes");
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference_arc));
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference_arc));
    // Derive the embed model stem from whatever the REPL loaded:
    // prefer `SetupConfig.models.embed` (the daemon's canonical
    // source), fall back to the `--model` path we were invoked
    // with (which in the REPL context is the quick-responder
    // slot — not technically the embed slot, but at least a stable
    // label). A missing stem becomes `"unknown-embed"`; the engine
    // will refuse to `ingest()` with an empty string so this path
    // still surfaces the error rather than writing a bogus label.
    let repl_embed_stem = sovereign_core::setup_config::SetupConfig::load()
        .ok()
        .and_then(|c| c.models.embed.file_stem().and_then(|s| s.to_str()).map(String::from))
        .or_else(|| {
            args.model
                .file_stem()
                .and_then(|s| s.to_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "unknown-embed".to_string());
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir, embed_fn)
            .with_embedding_model(&repl_embed_stem)
            .with_inference_fn(inference_fn.clone()),
    );

    // Register the "pdf" custom extractor so recipes declaring
    // `extract = { type = "custom", kind = "pdf" }` can ingest PDF
    // documents downloaded by `http_api + follow`. Implementation
    // lives in sovereign-tools (uses pdf-extract) so corpus-engine
    // stays free of the heavy PDF dep. Required by olc-opinions and
    // scotus-opinions; harmless when no PDF recipe runs.
    sovereign_tools::local_corpus::recipe_extractor::register_pdf_extractor(
        corpus_engine.as_ref(),
    );

    // KnowledgeView: register the SQLite acquirer, construct the
    // manager (excluding inner-work conversations from the
    // conversational view), install it as the post-commit observer,
    // and run initial ingest of empty views. See the server binary
    // for the full rationale; this is the CLI mirror.
    // Gated on `--no-knowledge-view` CLI flag (default enabled).
    // Mirror of the desktop Settings toggle + server config section.
    // When disabled, skip ingest, observer install, and the landscape-
    // digest splice entirely.
    let knowledge_view_manager = if args.knowledge_view_enabled {
        let local_only_skill_ids = skills.local_only_skill_ids();
        eprintln!(
            "knowledge_view: enabled; {} local-only skill(s) excluded from conversational corpus",
            local_only_skill_ids.len()
        );
        // Project-local ATOS store paths — `.sovereign/features.db`
        // + `.sovereign/project.toml` at the current repo root.
        // Mirrors `sovereign atos` subcommand layout. Used by the
        // splice path to compose initiative entities with phase + drift
        // annotations on the strategic digest.
        let project_sov_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".sovereign");
        let features_db_path = project_sov_dir.join("features.db");
        let project_toml_path = project_sov_dir.join("project.toml");
        let mut mgr = sovereign_tools::knowledge_view::KnowledgeViewManager::new(
            Arc::clone(&corpus_engine),
            inference_fn.clone(),
            db_path.clone(),
            local_only_skill_ids,
        )
        .await;
        if features_db_path.exists() {
            mgr = mgr.with_features_db_path(features_db_path);
        }
        if project_toml_path.exists() {
            mgr = mgr.with_project_toml_path(project_toml_path);
        }
        let mgr = Arc::new(mgr);
        store_concrete.set_observer(
            mgr.clone() as sovereign_core::observer::SharedStateStoreObserver,
        );
        // Background init — see the server binary for rationale.
        let _init_handle = Arc::clone(&mgr).spawn_init();
        Some(mgr)
    } else {
        eprintln!(
            "knowledge_view: disabled via --no-knowledge-view; landscape digests skipped"
        );
        None
    };

    // Register tools.
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(sovereign_tools::document::DocumentTool::new(
        Arc::clone(&store),
        Arc::clone(&inference_arc),
    )));
    tools.register(Box::new(sovereign_tools::ClaimSearchTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(sovereign_tools::EpistemicLandscapeTool::new(
        Arc::clone(&corpus_engine),
    )));
    // Code Intelligence tools — active as soon as any code corpus is
    // indexed via `sovereign code index`. They always register; with no
    // code corpora they return "no results" honestly rather than failing.
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(
        sovereign_tools::CodeSearchTool::new(Arc::clone(&corpus_engine))
            .with_inference(Arc::clone(&inference_arc)),
    ));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&corpus_engine),
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
    tools.register(Box::new(sovereign_tools::search::SearchTool::with_web(
        Arc::clone(&store),
        Arc::clone(&inference_arc),
        search_backend,
    )));
    tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));

    // Recipe-authoring tools — let the chat LLM run the
    // author → validate → test → publish loop documented in the
    // recipe-authoring platform plan. All five tools are
    // allowlisted to ~/.sovereign/recipes/ via Permission::RecipeAuthoring,
    // so the operator approves "this agent can iterate on recipes"
    // once instead of granting blanket FileWrite.
    tools.register(Box::new(sovereign_tools::RecipeReadTool::new()));
    tools.register(Box::new(sovereign_tools::RecipeWriteTool::new()));
    tools.register(Box::new(sovereign_tools::RecipeWriteStructuredTool::new()));
    tools.register(Box::new(sovereign_tools::RecipeValidateTool::new()));
    tools.register(Box::new(sovereign_tools::RecipeTestTool::new()));
    tools.register(Box::new(sovereign_tools::RegistryBrowseTool));

    // Recipe-author project tools — DecisionLog / Checkpoint /
    // CapabilityRequest. These funnel through NoteStore +
    // FeatureStore for state, so we open both stores under
    // ~/.sovereign/ and pass them into the tool constructors. Same
    // permission gate (RecipeAuthoring) as the five above.
    let recipe_notes_path = home.join(".sovereign").join("notes.db");
    let recipe_features_path = home.join(".sovereign").join("features.db");
    if let (Ok(rn), Ok(rf)) = (
        corpus_engine::NoteStore::open(&recipe_notes_path),
        corpus_engine::FeatureStore::open(&recipe_features_path),
    ) {
        let rn = Arc::new(rn);
        let rf = Arc::new(rf);
        tools.register(Box::new(sovereign_tools::DecisionLogTool::with_notes(
            Arc::clone(&rn),
        )));
        tools.register(Box::new(sovereign_tools::CheckpointTool::with_stores(
            Arc::clone(&rn),
            Arc::clone(&rf),
        )));
        tools.register(Box::new(
            sovereign_tools::CapabilityRequestTool::with_stores(
                Arc::clone(&rn),
                Arc::clone(&rf),
            ),
        ));
    } else {
        eprintln!(
            "Recipe-author state stores unavailable; \
             DecisionLog/Checkpoint/CapabilityRequest will not be registered."
        );
    }

    eprintln!("Tools: {} registered", tools.count());

    let approval = Arc::new(CliApprovalChannel::new(Arc::clone(&store)));

    let mut runtime = Runtime::new(
        inference_arc,
        router,
        Box::new(planner),
        Arc::new(tools),
        store.clone(),
        skills,
        approval,
        sovereign_core::types::InferenceConfig::default(),
    )
    .with_corpus_engine(Arc::clone(&corpus_engine));
    // Install the landscape-digest provider only when KnowledgeView
    // is enabled. When disabled, Runtime.landscape_digests stays None
    // and the splice path is a no-op — identical to pre-KnowledgeView
    // behaviour.
    if let Some(ref mgr) = knowledge_view_manager {
        runtime = runtime.with_landscape_digests(
            Arc::clone(mgr) as Arc<dyn sovereign_core::traits::LandscapeDigestProvider>,
        );
    }

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
// trigger
