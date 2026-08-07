// SPDX-License-Identifier: AGPL-3.0-or-later
// What's in sovereign-cli now (2026-05-22 split — slices 1-5):
//   * dev_bin / llm_bin — exec dispatchers into the two sibling
//     binaries (`sovereign-cli-dev`, `sovereign-cli-llm`).
//   * Pure delegators that translate the new flat CLI surface
//     (`svrn status`, `svrn drift accept`, etc.) into the
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
#[cfg(feature = "dev-tools")]
mod archaeology_eval_cmd;
mod audit_cmd;
#[cfg(feature = "awareness")]
mod awareness_cmd;
mod cache_audit_cmd;
mod charter_cmd;
// `svrn code index` in the shipped binary. Gated on `code-intel` rather than
// `dev-tools`: the index path needs corpus-engine's grammars and the SCIP db,
// but none of the workbench's heavy crates. The gate is here and ONLY here —
// no inner `#![cfg]` in the module, which is the bug that makes
// `--features awareness` alone fail to compile.
#[cfg(feature = "code-intel")]
mod code_index_cmd;
#[cfg(feature = "code-intel")]
mod code_index_incremental;
#[cfg(feature = "code-intel")]
mod code_refresh;
// `svrn init` / `svrn project init`. Same gate as the index path it drives —
// init's whole job is to produce a corpus, so a build that cannot index has
// nothing to offer it.
#[cfg(feature = "code-intel")]
mod project_init;
#[cfg(feature = "dev-tools")]
mod contract_cmd;
mod daemon_bin;
mod design_cmd;
mod dev_bin;
mod drift_cmd;
#[cfg(feature = "dev-tools")]
mod git_archaeology_cmd;
mod init;
mod llm_bin;
mod memory_cmd;
mod milestone_cmd;
mod notes_cmd;
mod notes_retrieval_cmd;
mod plan_cmd;
#[cfg(feature = "dev-tools")]
mod posture_cmd;
// NOT feature-gated, deliberately: the daemon-facing project registry adds
// zero dependencies and is the one thing a `curl | sh` user needs to reach
// the code-intelligence pipeline the daemon already runs.
mod project_registry;
mod reflect_cmd;
mod refresh_cmd;
#[cfg(feature = "dev-tools")]
mod rough_edges_cmd;
mod serve_cmd;
mod session_cmd;
mod session_lineage;
mod sibling;
mod status_cmd;
mod stop_cmd;
mod update_cmd;
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
    command: "svrn",
    summary: "Local AI assistant with code intelligence, knowledge bases, and an optional mesh.",
    sections: &[
        HelpSection::Usage(
            "svrn <subcommand> [flags]\n\
             sovereign --model <path.gguf> [options]   (legacy interactive REPL)",
        ),
        HelpSection::Subcommands(&[
            (
                "setup",
                "First-run: detect hardware, download models, start daemon",
            ),
            (
                "init",
                "Index this workspace for code intelligence, then start the MCP server",
            ),
            (
                "project",
                "Register a repo so the daemon indexes and watches it (init / register / list / watch)",
            ),
            (
                "model",
                "See/change the models the daemon loads; applies live (list / set / unset / context)",
            ),
            (
                "chat",
                "CLI mirror of the desktop chat flow (ask / session / inspect)",
            ),
            (
                "solve",
                "Give the daemon a coding goal; it makes the goal test-shaped and iterates to green",
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
            (
                "govern",
                "Common-law governance over a corpus — tensions / resolve / ask",
            ),
            ("doctor", "Diagnose setup and daemon health"),
            ("recipe", "Run a corpus ingestion recipe"),
            (
                "workflow",
                "Run a Step·Artifact·Runner workflow TOML (run / list / copy / new)",
            ),
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
            (
                "update",
                "Check for and install a newer CLI release (--check to only report)",
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
            ("--version, -V", "Print the version and exit"),
        ]),
        HelpSection::Notes("Run `svrn <subcommand> --help` for detail on any specific subcommand."),
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
    // Neither `code` nor `project` is here. Both are SPLIT surfaces whose
    // dispatch arms do their own per-subcommand routing across the four
    // (code-intel × dev-tools) build combinations; a blanket intercept here
    // would refuse `code index` in a build that can actually serve it.
    // `project` is NOT here. Its registry subcommands ship in the default
    // build (`project_registry`), so a blanket intercept would refuse verbs
    // this binary can actually serve. The `project` dispatch arm does its own
    // per-subcommand split; `refuse_workbench_subcommand` is the gate for the
    // half that still needs the sibling.
    "atos",
    "tools",
    "status",
    "charter",
    "design",
    "plan",
    "amend",
    "milestone",
    "drift",
    "audit",
    "serve",
    // `init` left this list 2026-08-07: `cmd_init` ships in the dispatcher
    // under `code-intel`, so a blanket intercept would refuse the one verb a
    // fresh `curl | sh` user types first. The `init` arm handles the
    // no-indexer build itself (init.rs), which is the same shape `code` and
    // `project` already use.
    "notes",
    "reflect",
    "rough-edges",
    "git-archaeology",
    "archaeology-eval",
    "agent-bench",
    "claim",
    "nudge",
    "contract",
    "posture",
];

/// Every top-level verb the dispatcher routes — the complete surface
/// `svrn __dump-commands` reports for the CLI-contract reverse check.
/// Independent of feature flags (it lists the dev-tools and awareness verbs
/// too) so the contract sees the whole surface in any build. Kept sorted; the
/// `all_verbs_is_complete_and_sorted` test pins it against `DEV_VERBS` + `HELP`
/// so it cannot silently drift from the dispatch `match` arms.
const ALL_VERBS: &[&str] = &[
    "agent-bench",
    "alignment",
    "amend",
    "archaeology-eval",
    "atlas",
    "atos",
    "audit",
    "awareness",
    "bench",
    "cache-audit",
    "charter",
    "chat",
    "claim",
    "code",
    "contract",
    "corpus",
    "daemon",
    "design",
    "doctor",
    "drift",
    "enrich",
    "eval",
    "git-archaeology",
    "govern",
    "init",
    "install-service",
    "knowledge-gym",
    "maintainer",
    "mcp",
    "memory",
    "mesh",
    "meshapp",
    "meta-atlas",
    "milestone",
    "mobile",
    "model",
    "newsworthy",
    "notes",
    "nudge",
    "pipeline",
    "plan",
    "portfolio",
    "posture",
    "project",
    "proxy",
    "reading-diag",
    "recipe",
    "recipe-agent",
    "reflect",
    "refresh",
    "rough-edges",
    "router-cache",
    "search-gym",
    "serve",
    "session",
    "setup",
    "solve",
    "status",
    "stop",
    "tools",
    "update",
    "voice",
    "workflow",
];

/// The developer-toolchain verbs as a help table, appended to `--help`
/// only under `--features dev-tools`. Help text is data (ARCH_PRINCIPLES §6).
#[cfg(feature = "dev-tools")]
const DEV_SUBCOMMANDS: &[(&str, &str)] = &[
    (
        "project",
        "Per-project code intelligence (init / serve / status / refresh)",
    ),
    (
        "code",
        "Code intelligence tooling (index / watch / mcp-status)",
    ),
    (
        "tools",
        "Invoke code-intelligence tools (list / describe / call)",
    ),
    (
        "atos",
        "Agent task orchestration (charter → plan → milestones)",
    ),
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
    (
        "archaeology-eval",
        "Evaluate atom provenance vs git history",
    ),
    ("agent-bench", "Eight-problem agent-coding battery"),
    ("claim", "Work-atlas scope claims (mesh coordination)"),
    ("nudge", "Dismiss audit nudges"),
    (
        "contract",
        "What the CLI promises, how much is proven, when it last ran",
    ),
    (
        "posture",
        "Artifact age + verdict per quality subsystem, one table",
    ),
];

fn print_usage() {
    crate::util::help::print(&HELP);
    // Developer builds additionally list the gated toolchain verbs so
    // `--features dev-tools` users see the full surface. The default
    // (public) build omits them — the product is the assistant + mesh.
    #[cfg(feature = "dev-tools")]
    crate::util::help::print_subcommands_titled("Developer toolchain", DEV_SUBCOMMANDS);
}

/// `svrn nudge dismiss <id>` — record a nudge id in
/// `~/.sovereign/dismissed_nudges.json` so the audit / status
/// surfaces stop showing it. The id can be a family name (e.g.
/// `recipe-publish`) to dismiss every variant, or a specific
/// instance (e.g. `recipe-publish:sec-investigation`) to dismiss
/// just that one.
async fn run_nudge(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: svrn nudge dismiss <id>");
        eprintln!("Example: sovereign nudge dismiss recipe-publish");
        return 2;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        println!(
            "Usage: svrn nudge <subcommand> [args]\n\n\
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
        eprintln!("svrn panic at {location}: {payload}\nbacktrace:\n{backtrace}");
        tracing::error!(
            location = %location,
            payload = %payload,
            backtrace = %backtrace,
            "svrn panic — see backtrace above"
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
    // Rebrand back-compat: bridge legacy SOVEREIGN_* env vars and migrate the
    // ~/.sovereign data dirs to ~/.svrnmesh before any threads spawn or state
    // is opened. Both are idempotent + non-destructive (see sovereign_core::rebrand).
    sovereign_core::rebrand::promote_legacy_env();
    sovereign_core::rebrand::run_startup_migration();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-rt-worker")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(async_main());
}

/// The `--version` line: program name + the workspace version the three
/// product binaries share (each inherits it via `version.workspace = true`).
/// It matches the `cli-vX.Y.Z` release tag, so it's the string a bug report
/// should carry. Pure + testable; the dispatch below prints it and exits.
fn version_line() -> String {
    format!("sovereign {}", env!("CARGO_PKG_VERSION"))
}

async fn async_main() {
    // Check for subcommands before standard arg parsing.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();

    // Top-level --help / help short-circuit. A lone "help" (no
    // subcommand) prints the banner; `svrn mesh --help` is
    // handled by the subcommand dispatcher below.
    if let Some(first) = raw_args.first() {
        if matches!(first.as_str(), "--help" | "-h" | "help") && raw_args.len() == 1 {
            print_usage();
            std::process::exit(0);
        }
    }

    // Top-level --version / -V (or a lone `version`). A bug report needs a
    // version string, and before this there was none — `svrn --version` fell
    // through to the banner. `svrn <subcommand> --version` still routes to the
    // subcommand dispatcher below, unshadowed.
    if let Some(first) = raw_args.first() {
        let f = first.as_str();
        if f == "--version" || f == "-V" || (f == "version" && raw_args.len() == 1) {
            println!("{}", version_line());
            std::process::exit(0);
        }
    }

    // Hidden introspection: `svrn __dump-commands` prints every top-level
    // verb the CLI dispatches (one per line) for the cli_contract_code reverse
    // check. Not advertised in HELP; runs in any build (before the dev-tools
    // gate) so the contract sees the whole surface regardless of features.
    if raw_args.first().map(String::as_str) == Some("__dump-commands") {
        for verb in ALL_VERBS {
            println!("{verb}");
        }
        std::process::exit(0);
    }

    // Hidden introspection: `svrn __contract-smoke` prints the manifest's
    // read-only smoke probes as TSV (`<expect_exit>\t<args>\t<expect_substr>`)
    // for the cli-contract-live-verify.sh harness. Reads docs/cli-contract.toml
    // (present in a dev checkout only); a no-op in the shipped binary.
    if raw_args.first().map(String::as_str) == Some("__contract-smoke") {
        match sovereign_cli_shared::cli_contract::Contract::load_default() {
            Ok(contract) => {
                for cmd in &contract.commands {
                    if let Some(smoke) = &cmd.smoke {
                        println!(
                            "{}\t{}\t{}",
                            smoke.expect_exit,
                            smoke.args.join(" "),
                            smoke.expect_stdout_contains.clone().unwrap_or_default()
                        );
                    }
                }
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("__contract-smoke: {e}");
                std::process::exit(1);
            }
        }
    }

    // Hidden introspection: `svrn __journey-plan` prints the manifest's
    // JOURNEYS — the sequenced use cases — for the cli-journey-verify.sh
    // harness. Two record kinds, tab-separated, journeys emitted hardest-
    // hitting first (tier ascending) and each immediately followed by its
    // steps in order:
    //
    //   J <id> <tier> <persona> <visibility> <live|skip:reason> <title>
    //     <experience> <needs,comma-joined|->
    //   S <id> <idx> <mut|ro> <exit|-> <contains|-> <absent|-> <1|0 non-empty>
    //     <live|skip:reason> <run>
    //
    // The two J columns added by the experience axis go AFTER `title`, which
    // looks backwards next to the S row's "a new column goes before `run`,
    // never after". The reason is the shell side: `read` gives its LAST
    // variable the remainder of the line, so a field that may contain
    // whitespace has to be last — `run` does, and it is. `title` also
    // contains whitespace but is NOT the runner's last variable (the read
    // declares enough names for the wider S row), so appending past it is
    // safe. Appending also keeps every hand-written plan in
    // scripts/tests/cli-journey-selftest.sh valid: those 18 J rows are the
    // runner's negative controls, and a layout change that silently shifted
    // their `title` into `experience` would weaken the one harness that
    // proves this runner can fail.
    //
    // `-` means "not asserted" so a bash `while IFS=$'\t' read` never sees a
    // collapsed empty field. `run` is last because it is the only field that
    // may contain spaces. Reads docs/cli-contract.toml (a dev checkout only);
    // a no-op in the shipped binary, like __contract-smoke above.
    if raw_args.first().map(String::as_str) == Some("__journey-plan") {
        match sovereign_cli_shared::cli_contract::Contract::load_default() {
            Ok(contract) => {
                let mut journeys: Vec<_> = contract.journeys.iter().collect();
                journeys.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.id.cmp(&b.id)));
                let dash = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
                let live = |o: &Option<String>| {
                    o.as_ref()
                        .map(|r| format!("skip:{r}"))
                        .unwrap_or_else(|| "live".into())
                };
                for j in journeys {
                    // `token:why` pairs joined by `;`. The WHY travels with the
                    // token so the shell runner can explain a skipped journey
                    // without restating the sentence — one source of truth
                    // (Need::why), printed by whoever needs it.
                    //
                    // `;` and NOT `,`: the reasons are prose and contain commas
                    // ("Claude transcripts, notes, drift report"), so a
                    // comma-joined list truncated every reason at its first
                    // comma when the runner split it. `needs_are_delimiter_safe`
                    // in cli_contract.rs pins the separator against the text.
                    let needs = if j.needs.is_empty() {
                        "-".to_string()
                    } else {
                        j.needs
                            .iter()
                            .map(|n| format!("{}:{}", n.as_str(), n.why()))
                            .collect::<Vec<_>>()
                            .join(";")
                    };
                    println!(
                        "J\t{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}",
                        j.id,
                        j.tier,
                        j.persona,
                        j.visibility,
                        live(&j.skip_live),
                        j.title,
                        j.experience,
                        needs
                    );
                    for (i, s) in j.steps.iter().enumerate() {
                        let e = s.expect.clone().unwrap_or_default();
                        // `run` stays LAST: it is the only field that may
                        // contain whitespace, and the shell runner reads it as
                        // the remainder of the line. A new column goes before
                        // it, never after.
                        println!(
                            "S\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                            j.id,
                            i,
                            if s.mutates { "mut" } else { "ro" },
                            e.exit.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
                            dash(&e.stdout_contains),
                            dash(&e.stdout_absent),
                            u8::from(e.stdout_non_empty),
                            live(&s.skip_live),
                            s.settle_secs.unwrap_or(0),
                            s.run
                        );
                    }
                }
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("__journey-plan: {e}");
                std::process::exit(1);
            }
        }
    }

    // A dev checkout running a dispatcher built without `--features
    // dev-tools` has silently lost the developer verbs. Warn on EVERY verb,
    // not only the gated ones: the whole failure mode is that the loss
    // surfaces later, on an unrelated command, long after the build that
    // caused it. Warn-only; never blocks. (After the hidden introspection
    // verbs above, which must stay machine-parseable on stdout.)
    sibling::warn_if_dev_tools_missing(cfg!(feature = "dev-tools"));

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
                     in the default build. Restore it with `cargo build -p \
                     sovereign-cli --features dev-tools` (the `-p` matters — \
                     without it you rebuild the workspace default, not this \
                     dispatcher), plus `cargo build -p sovereign-cli-dev` for the \
                     sibling that actually runs the verb."
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
            "mesh" | "meshapp" | "mobile" | "alignment" | "corpus" | "meta-atlas" | "mcp"
            | "recipe" | "pipeline" | "recipe-agent" | "maintainer" => {
                let code = llm_bin::exec(first, &raw_args[1..]);
                std::process::exit(code);
            }
            "code" => {
                // Split surface, same shape as `project`: `code index` runs
                // here under `code-intel`; the analysis subcommands stay in
                // the workbench sibling.
                #[cfg(feature = "code-intel")]
                let handled = code_index_cmd::try_run(&raw_args[1..]).await;
                #[cfg(not(feature = "code-intel"))]
                let handled: Option<i32> = None;

                let code = match handled {
                    Some(c) => c,
                    None if cfg!(feature = "dev-tools") => {
                        dev_bin::exec("code", &raw_args[1..])
                    }
                    #[cfg(feature = "code-intel")]
                    None => code_index_cmd::refuse_workbench_subcommand(
                        raw_args.get(1).map(String::as_str),
                    ),
                    #[cfg(not(feature = "code-intel"))]
                    None => {
                        eprintln!(
                            "svrn code: not available in this build. Rebuild with \
                             `--features code-intel` for `code index`."
                        );
                        2
                    }
                };
                std::process::exit(code);
            }
            "init" => {
                // Top-level `svrn init` — replaces
                // `svrn project init`. The old name continues
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
            "cache-audit" => {
                // In-process telemetry over Claude Code transcripts — reads
                // only local ~/.claude/projects/*.jsonl, no daemon/network.
                let code = cache_audit_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "session" => {
                // Session continuity — distill a transcript into a session
                // frame (docs/specs/SESSION_CONTINUITY.md). Reads the same
                // local transcripts as cache-audit; the synthesis stage
                // talks to the local daemon only.
                let code = session_cmd::run(&raw_args[1..]).await;
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
            #[cfg(feature = "dev-tools")]
            "contract" => {
                // The CLI's own quality surface. In-process and dev-gated: it
                // reads docs/cli-contract.toml, which only a source checkout has.
                let code = contract_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            #[cfg(feature = "dev-tools")]
            "posture" => {
                // Read-only roll-up of every posture-bearing subsystem's
                // artifact age (drift/arch/capability/nightly/watchers/…).
                let code = posture_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            #[cfg(feature = "dev-tools")]
            "rough-edges" => {
                let code = rough_edges_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            #[cfg(feature = "dev-tools")]
            "git-archaeology" => {
                let code = git_archaeology_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            #[cfg(feature = "dev-tools")]
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
            "solve" => {
                // Daemon-hosted TDD solver client (docs/specs/SOLVE_UX.md).
                // Lives in sovereign-cli-llm with the other daemon-HTTP
                // clients (chat, claim).
                let code = llm_bin::exec("solve", &raw_args[1..]);
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
            "update" => {
                // Self-update: check the release shelf + re-run the canonical
                // installer. In-process (owned by the dispatcher) because it
                // must replace ALL sibling binaries, not just one.
                let code = update_cmd::run(&raw_args[1..]).await;
                std::process::exit(code);
            }
            "project" => {
                // Split surface. `register` / `unregister` / `list` / `watch`
                // run HERE, in the shipped dispatcher, because they are pure
                // loopback HTTP and the daemon already owns the indexing
                // pipeline they drive. The heavier lifecycle subcommands
                // (`init`, `serve`, `status`, `found`, …) still live in the
                // workbench sibling.
                let code = match project_registry::try_run(&raw_args[1..]).await {
                    Some(c) => c,
                    None if cfg!(feature = "dev-tools") => {
                        dev_bin::exec("project", &raw_args[1..])
                    }
                    None => project_registry::refuse_workbench_subcommand(
                        raw_args.get(1).map(String::as_str),
                    ),
                };
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
            | "chat" | "reading-diag" | "newsworthy" | "govern" | "router-cache" | "proxy"
            | "portfolio" | "workflow" => {
                let code = llm_bin::exec(first, &raw_args[1..]);
                std::process::exit(code);
            }
            #[cfg(feature = "dev-tools")]
            "agent-bench" => {
                // Eleven-problem coding battery; subprocess-driven
                // pi / opencode / codex runners. See SYSTEM_OVERVIEW §4
                // and `sovereign/crates/sovereign-agent-bench/`.
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
            "model" => {
                // Reads/writes the [models] config and hot-applies via the
                // daemon's admin reload — lives in sovereign-cli-daemon next
                // to setup (shares the config type) and the daemon it reloads.
                let code = daemon_bin::exec("model", &raw_args[1..]);
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
    // interactive shell type `svrn chat`.
    print_usage();
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--version` must carry the real workspace version — the string a bug
    /// report should include. Guards against the earlier regression where
    /// `svrn --version` printed the banner with no version at all.
    #[test]
    fn version_line_carries_the_workspace_version() {
        let v = version_line();
        assert_eq!(v, format!("sovereign {}", env!("CARGO_PKG_VERSION")));
        let num = v
            .strip_prefix("sovereign ")
            .expect("version line is prefixed with `sovereign `");
        assert!(
            num.split('.').count() >= 3 && num.starts_with(|c: char| c.is_ascii_digit()),
            "version_line() should be `sovereign <semver>`, got `{v}`"
        );
    }

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

    /// `ALL_VERBS` (what `__dump-commands` reports) must list every dispatched
    /// top-level verb. Cross-checked against the two existing authoritative
    /// lists — `DEV_VERBS` and the `HELP` subcommand table — so the dump
    /// cannot drift from the `match` arms without a test going red. Also pins
    /// sorted + dedup so the reverse check's output is stable.
    #[test]
    fn all_verbs_is_complete_and_sorted() {
        for v in DEV_VERBS {
            assert!(
                ALL_VERBS.contains(v),
                "DEV_VERBS lists `{v}` but ALL_VERBS does not (update ALL_VERBS)"
            );
        }
        for section in HELP.sections {
            if let HelpSection::Subcommands(entries) = section {
                for (name, _) in *entries {
                    assert!(
                        ALL_VERBS.contains(name),
                        "HELP advertises `{name}` but ALL_VERBS does not (update ALL_VERBS)"
                    );
                }
            }
        }
        let mut sorted = ALL_VERBS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ALL_VERBS.len(), "ALL_VERBS has duplicates");
        assert_eq!(
            sorted.as_slice(),
            ALL_VERBS,
            "ALL_VERBS must be kept sorted"
        );
    }
}
