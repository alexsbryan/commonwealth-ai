// What's in sovereign-cli now (2026-05-22 split — slices 1-5):
//   * dev_bin / llm_bin — exec dispatchers into the two sibling
//     binaries (`sovereign-cli-dev`, `sovereign-cli-llm`).
//   * Pure delegators that translate the new flat CLI surface
//     (`sovereign status`, `sovereign drift accept`, etc.) into the
//     legacy `atos`/`project`/`code` handler arguments before exec'ing.
//   * Light commands that touch only SQLite stores + filesystem
//     (notes, claim, reflect, rough-edges, archaeology-eval,
//     git-archaeology).
//
// Slice 1 → sovereign-cli-dev: atos_cmd, atos_plugin.
// Slice 2 → sovereign-cli-dev: project_cmd, code_cmd, amend, phases,
//   honesty, observation, project_toml, found, doc_fetcher,
//   plan_composer, plan_enricher, design_session, design_onboarding,
//   audit_extract, audit_recover, drift_cmd_orchestrator.
// Slice 3 → sovereign-cli-dev: tools_cmd.
// Slice 4 → sovereign-cli-dev: daemon_cmd, doctor_cmd,
//   install_service_cmd, service_install, setup_cmd, setup_config.
// Slice 5 → sovereign-cli-llm: bench_cmd, chat_cmd, eval_cmd,
//   voice_eval, reading_diag_cmd, knowledge_gym_cmd, search_gym_cmd,
//   gym_judge, atlas_cmd, meta_atlas_cmd, enrich_cmd, newsworthy_cmd,
//   recipe_cmd, recipe_agent_cmd, recipe_agent_live_trial,
//   pipeline_cmd, mcp_cmd, alignment_cmd, mesh_cmd,
//   corpus_catalog_cmd, corpus_scrub_cmd, corpus_snapshot_cmd,
//   corpus_watch_cmd, worker_pod_provider, REPL Runtime construction.

mod amend_cmd;
mod archaeology_eval_cmd;
mod audit_cmd;
#[cfg(feature = "awareness")]
mod awareness_cmd;
mod charter_cmd;
mod daemon_bin;
mod design_cmd;
mod dev_bin;
mod drift_cmd;
mod git_archaeology_cmd;
mod init;
mod llm_bin;
mod memory_cmd;
mod milestone_cmd;
mod notes_cmd;
mod plan_cmd;
mod reflect_cmd;
mod refresh_cmd;
mod rough_edges_cmd;
mod serve_cmd;
mod status_cmd;
mod stop_cmd;
mod util;

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{ApprovalChannel, StateStore};
use sovereign_core::types::*;

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
            StepOutput::Text(_)
            | StepOutput::Json(_)
            | StepOutput::ReasonWithToolsResult { .. } => "done",
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
            (
                "setup",
                "First-run: detect hardware, download models, start daemon",
            ),
            (
                "chat",
                "CLI mirror of the desktop chat flow (ask / session / inspect)",
            ),
            ("mesh", "Mesh management (create / join / rotate / status)"),
            (
                "mobile",
                "Serve the phone-facing API, riding on the daemon's models (serve / status / pair)",
            ),
            (
                "alignment",
                "Mesh-replicated workspace migrate / status (~/.claude + notes.db)",
            ),
            ("corpus", "Knowledge corpus install / remove / status"),
            ("doctor", "Diagnose setup and daemon health"),
            ("recipe", "Run a corpus ingestion recipe"),
            (
                "pipeline",
                "Generic ingestion driver — durable worklist + retry + pause-resume",
            ),
            (
                "bench",
                "Throughput + correctness benchmarks for enrichment LLM tasks",
            ),
            (
                "search-gym",
                "Correctness harness for web-search-during-inference (mock-replay)",
            ),
            (
                "knowledge-gym",
                "Correctness harness for the unified knowledge_lookup tool (mock-replay)",
            ),
            (
                "atlas",
                "Atlas-style structural enrichment (Wikipedia link graph today)",
            ),
            (
                "eval",
                "Run a question bank against a corpus; measure retrieval quality",
            ),
            ("mcp", "MCP server diagnostics (list tools, proxy)"),
            (
                "daemon",
                "(internal) Long-running service managed by launchd/systemd",
            ),
        ]),
        HelpSection::Flags(&[
            ("--model <path>", "Quick responder GGUF (REPL mode only)"),
            (
                "--primary-model <path>",
                "Main responder GGUF (REPL, lazy-loaded)",
            ),
            ("--data-dir <path>", "Database directory (default: data)"),
            (
                "--skills-dir <path>",
                "Skills directory (default: ~/.sovereign/skills)",
            ),
            (
                "--ingest <path>",
                "Ingest documents from directory before REPL",
            ),
            ("--router", "Enable LLM-based intent routing"),
            (
                "--no-knowledge-view",
                "Disable KnowledgeView landscape digests (default: enabled)",
            ),
            ("--brave-api-key <key>", "Brave Search key (optional)"),
            ("--tavily-api-key <key>", "Tavily Search key (optional)"),
            ("--help, -h", "Show this message"),
        ]),
        HelpSection::Notes(
            "Run `sovereign <subcommand> --help` for detail on any specific subcommand.",
        ),
    ],
};

/// Verbs that belong to the **developer toolchain** — project lifecycle,
/// ATOS orchestration, code intelligence, git archaeology, agent benches.
/// They are gated out of the default (end-user) build: most exec the
/// `sovereign-cli-dev` sibling that a public build does not ship; the rest
/// are in-process dev tooling kept off the product surface. A default build
/// intercepts them (see `async_main`) and points the user at
/// `--features dev-tools`. Kept disjoint from the public `HELP` subcommands
/// by the `public_help_advertises_no_dev_verb` test.
const DEV_VERBS: &[&str] = &[
    "code", "project", "atos", "tools", "status", "charter", "design", "plan",
    "amend", "refresh", "milestone", "drift", "audit", "serve", "init", "notes",
    "reflect", "rough-edges", "git-archaeology", "archaeology-eval", "agent-bench",
    "claim", "nudge",
];

/// The developer-toolchain verbs as a help table, appended to `--help`
/// only under `--features dev-tools`. Help text is data (ARCH_PRINCIPLES §6).
#[cfg(feature = "dev-tools")]
const DEV_SUBCOMMANDS: &[(&str, &str)] = &[
    ("project", "Per-project code intelligence (init / serve / status / refresh)"),
    ("code", "Code intelligence tooling (index / watch / mcp-status)"),
    ("tools", "Invoke code-intelligence tools (list / describe / call)"),
    ("atos", "Agent task orchestration (charter → plan → milestones)"),
    ("status", "Project / ATOS status report"),
    ("charter", "Create or amend a project charter"),
    ("design", "Capture a design session"),
    ("plan", "Compose + align a project plan"),
    ("amend", "Amend a charter or plan"),
    ("milestone", "Advance or close an ATOS milestone"),
    ("drift", "Architectural-drift detection + spec accept"),
    ("audit", "Audit rollup / recover / teardown"),
    ("refresh", "Rebuild the project code index"),
    ("serve", "Run the code-intelligence MCP server"),
    ("init", "Scaffold AI-assistant config in a project"),
    ("notes", "Decision / invariant note store"),
    ("reflect", "Review session reflections; retire fixed ones"),
    ("rough-edges", "Surface rough edges from git history"),
    ("git-archaeology", "Mine commit history for provenance"),
    ("archaeology-eval", "Evaluate atom provenance vs git history"),
    ("agent-bench", "Eight-problem agent-coding battery"),
    ("claim", "Work-atlas scope claims (mesh coordination)"),
    ("nudge", "Dismiss audit nudges"),
];

fn print_usage() {
    crate::util::help::print(&HELP);
    // Developer builds additionally list the gated toolchain verbs so
    // `--features dev-tools` users see the full surface. The default
    // (public) build omits them — the product is the assistant + mesh.
    #[cfg(feature = "dev-tools")]
    crate::util::help::print_subcommands_titled("Developer toolchain", DEV_SUBCOMMANDS);
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
        eprintln!("sovereign panic at {location}: {payload}\nbacktrace:\n{backtrace}");
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

    // Gate the developer toolchain out of the default build. `cfg!` (not
    // `#[cfg]`) so `DEV_VERBS` stays referenced — and thus warning-free —
    // in both feature states; the optimizer drops this block when the
    // feature is on, letting the dev verbs fall through to the dispatch
    // table below.
    if !cfg!(feature = "dev-tools") {
        if let Some(first) = raw_args.first() {
            if DEV_VERBS.contains(&first.as_str()) {
                eprintln!(
                    "{first}: part of the Sovereign developer toolchain (project \
                     lifecycle, ATOS orchestration, code intelligence). It is not \
                     in the default build. Rebuild with `cargo build --features \
                     dev-tools` and build the `sovereign-cli-dev` sibling to enable it."
                );
                std::process::exit(2);
            }
        }
    }

    if let Some(first) = raw_args.first() {
        match first.as_str() {
            // ── LLM / bench / corpus / mesh cluster → sovereign-cli-llm ──
            // All these moved to the LLM sibling in slice 5. The shim
            // execs into it without setting up a tracing subscriber —
            // the sibling's main() installs the appropriate filter for
            // each verb.
            "mesh" | "mobile" | "alignment" | "corpus" | "meta-atlas" | "mcp" | "recipe"
            | "pipeline" | "recipe-agent" | "maintainer" => {
                let code = llm_bin::exec(first, &raw_args[1..]);
                std::process::exit(code);
            }
            "code" => {
                // Moved to the sovereign-cli-dev sibling.
                let code = dev_bin::exec("code", &raw_args[1..]);
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
                // Moved to sovereign-cli-llm (uses sovereign-mesh +
                // sovereign-work-atlas, both heavy).
                let code = llm_bin::exec("claim", &raw_args[1..]);
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
                // Lives in sovereign-cli-daemon alongside
                // setup_cmd + service_install.
                let code = daemon_bin::exec("install-service", &raw_args[1..]);
                std::process::exit(code);
            }
            "refresh" => {
                let code = refresh_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "project" => {
                // Moved to the sovereign-cli-dev sibling.
                let code = dev_bin::exec("project", &raw_args[1..]);
                std::process::exit(code);
            }
            "reflect" => {
                let code = reflect_cmd::run_reflect(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "atos" => {
                // Lives in the `sovereign-cli-dev` sibling binary now.
                // exec() replaces the current process on Unix; child
                // exit on other platforms.
                let code = dev_bin::exec("atos", &raw_args[1..]);
                std::process::exit(code);
            }
            "memory" => {
                util::tracing_init::init_tracing("sovereign_cli=info,sovereign_store=info");
                let code = memory_cmd::run_memory(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "awareness" => {
                #[cfg(feature = "awareness")]
                {
                    util::tracing_init::init_tracing(
                        "sovereign_cli=info,sovereign_tools=debug,corpus_engine=debug",
                    );
                    let code = awareness_cmd::run_awareness(&raw_args[1..]).await;
                    std::process::exit(code);
                }
                #[cfg(not(feature = "awareness"))]
                {
                    eprintln!(
                        "awareness: built only under the `awareness` cargo feature\n\
                         (it pulls the heavy knowledge-view surface). Rebuild with\n\
                         `cargo build --features awareness` to enable."
                    );
                    std::process::exit(2);
                }
            }
            "tools" => {
                // Moved to the sovereign-cli-dev sibling.
                let code = dev_bin::exec("tools", &raw_args[1..]);
                std::process::exit(code);
            }
            // ── LLM cluster (continued) → sovereign-cli-llm ──
            "enrich" | "atlas" | "eval" | "voice" | "bench" | "search-gym" | "knowledge-gym"
            | "chat" | "reading-diag" | "newsworthy" => {
                let code = llm_bin::exec(first, &raw_args[1..]);
                std::process::exit(code);
            }
            "agent-bench" => {
                // Eight-problem coding battery; subprocess-driven
                // pi / opencode / codex runners. See SYSTEM_OVERVIEW §11
                // and `sovereign/crates/sovereign-agent-bench/`.
                // Stays in sovereign-cli for now — light dep surface.
                let code = sovereign_agent_bench::run_agent_bench(&raw_args[1..]).await;
                std::process::exit(code as i32);
            }
            "nudge" => {
                let code = run_nudge(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "doctor" => {
                // ScipGraph integrity probe + health check lives in
                // sovereign-cli-daemon alongside the daemon it
                // diagnoses.
                let code = daemon_bin::exec("doctor", &raw_args[1..]);
                std::process::exit(code);
            }
            "setup" => {
                // Hardware planner + model manifest live in
                // sovereign-cli-daemon — same binary that hosts the
                // daemon they configure.
                let code = daemon_bin::exec("setup", &raw_args[1..]);
                std::process::exit(code);
            }
            "daemon" => {
                // Long-running host process. Its main() applies the
                // structured-tracing filter for launchd / systemd
                // before dispatch.
                let code = daemon_bin::exec("daemon", &raw_args[1..]);
                std::process::exit(code);
            }
            _ => {}
        }
    }

    // No recognised subcommand. Pre-2026-05-22 sovereign-cli used to
    // fall through here into a full REPL loop that constructed an
    // EmbeddedLlamaCpp, the KnowledgeView manager, the tool registry,
    // etc. — about 380 lines of Runtime construction. That path moved
    // to `sovereign-cli-llm` (the `chat` subcommand) along with every
    // other LLM-touching surface, so the dispatcher binary stops
    // linking llama-cpp-2 + lance.
    //
    // Bare `sovereign` now prints usage and exits. Users who want the
    // interactive shell type `sovereign chat`.
    print_usage();
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The public `--help` must never advertise a verb the default build
    /// rejects. Pins the gating invariant: every `Subcommands` entry in
    /// `HELP` is absent from `DEV_VERBS` (ARCH_PRINCIPLES §7.2 — an
    /// invariant as a test, not a comment).
    #[test]
    fn public_help_advertises_no_dev_verb() {
        let mut saw_subcommands = false;
        for section in HELP.sections {
            if let HelpSection::Subcommands(entries) = section {
                saw_subcommands = true;
                for (name, _) in *entries {
                    assert!(
                        !DEV_VERBS.contains(name),
                        "public help advertises gated dev-toolchain verb `{name}`"
                    );
                }
            }
        }
        assert!(saw_subcommands, "HELP is missing its Subcommands section");
    }
}
