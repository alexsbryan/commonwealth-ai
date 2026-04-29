//! `sovereign project` subcommand — one-shot workspace setup for code intelligence.
//!
//! Run `sovereign project init` from any repo root and the entire code
//! intelligence stack is wired up: tree-sitter symbol index, SCIP call
//! graph, `.claude/settings.json`, `SOVEREIGN.md`, git hooks, and a
//! filesystem watcher. Two minutes from first run to fully working tools.

use std::collections::HashMap;
use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use arc_swap::ArcSwap;
use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn, IngestProgress};

/// Human-readable identifier for the embed model this user has set up,
/// used as the `expected_embedding_model` on the `CorpusEngine` so the
/// log line and `_corpus_meta.json` reflect what they actually loaded
/// (e.g. `qwen3-embedding-0.6b-q8_0`) instead of the engine's default.
///
/// Sources `SetupConfig::load()` and falls back to the default when
/// the user hasn't run `sovereign setup` yet (in which case the
/// engine's default is harmless — code indexes are FTS-only).
fn configured_embed_model_name() -> String {
    if let Ok(cfg) = sovereign_core::setup_config::SetupConfig::load() {
        if let Some(stem) = cfg
            .models
            .embed
            .file_stem()
            .and_then(|s| s.to_str())
        {
            return stem.to_lowercase();
        }
    }
    "qwen3-embedding-0.6b".to_string()
}

// ─── Dispatch ────────────────────────────────────────────────

pub async fn run_project(args: &[String]) -> i32 {
    // Top-level `project --help` / `project -h` / `project help`.
    // Specific sub-subcommand help (e.g. `project init --help`) is
    // handled inside each cmd_* function via `util::help::wants_help`.
    if args.is_empty() {
        crate::util::help::print(&HELP);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        crate::util::help::print(&HELP);
        return 0;
    }

    // Every leaf below is also reachable as a top-level `sovereign
    // <leaf>` after the namespace collapse. The shims here keep the
    // old `project <leaf>` working — they print a one-time banner
    // and forward to the same handler the new top-level arm uses,
    // so behaviour is identical modulo the banner. Suppress with
    // SOVEREIGN_QUIET_DEPRECATIONS=1.
    use crate::util::deprecation::announce;
    match args[0].as_str() {
        "init" => {
            announce("sovereign project init", "sovereign init");
            cmd_init(&args[1..]).await
        }
        "design" => {
            announce("sovereign project design", "sovereign design");
            cmd_design(&args[1..]).await
        }
        "plan" => {
            announce("sovereign project plan", "sovereign plan");
            cmd_plan(&args[1..]).await
        }
        "charter" => {
            announce("sovereign project charter", "sovereign charter");
            cmd_charter(&args[1..]).await
        }
        "found" => cmd_found(&args[1..]).await,
        "amend" => {
            announce("sovereign project amend", "sovereign amend");
            cmd_amend(&args[1..]).await
        }
        "phase" => cmd_phase(&args[1..]).await,
        "audit" => {
            announce("sovereign project audit", "sovereign audit");
            cmd_audit(&args[1..]).await
        }
        "status" => {
            announce("sovereign project status", "sovereign status");
            cmd_status(&args[1..]).await
        }
        "refresh" => {
            announce("sovereign project refresh", "sovereign refresh");
            cmd_refresh(&args[1..]).await
        }
        "serve" => {
            announce("sovereign project serve", "sovereign serve");
            cmd_serve(&args[1..]).await
        }
        "install-hooks" => cmd_install_hooks(&args[1..]).await,
        "register" => cmd_register(&args[1..]).await,
        "unregister" => cmd_unregister(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
        "watch" => cmd_watch(&args[1..]).await,
        other => {
            eprintln!("Unknown project subcommand: {other}");
            crate::util::help::print(&HELP);
            1
        }
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project",
    summary: "Per-project code intelligence: indexes, call graphs, and the MCP tool server.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign project <subcommand> [flags]"),
        crate::util::help::HelpSection::Subcommands(&[
            ("init",           "Set up code intelligence for the current workspace (also registers with the daemon)"),
            ("design",         "Agent-collaborative DESIGN.md session (opencode-first). --solo to skip the agent"),
            ("plan",           "Compose IMPLEMENTATION_PLAN.md from DESIGN.md + OPEN_QUESTIONS.md; indexes plan items in .sovereign/plan.db"),
            ("charter",        "Write / edit the free-form team CHARTER.md (governance, culture, onboarding); separate from DESIGN.md"),
            ("found",          "Once per project: structured conversation that produces CHARTER.md + PHASES.md"),
            ("amend",          "Edit CHARTER.md with an adversarial review — every amendment logs who, why, and what was argued against"),
            ("phase",          "phase status | phase pass [N] — track PHASES.md progression, run stop conditions, write phase-N.md"),
            ("audit",          "One-page reviewer rollup: founding, phases passed, notes-by-kind, drift status, open questions, red-team findings"),
            ("status",         "Show the status of code intelligence"),
            ("refresh",        "Nudge the daemon to rebuild the SCIP graph now"),
            ("serve",          "Foreground watcher mode for debugging test/lint scripts"),
            ("register",       "Tell the daemon to watch this project (run once per repo)"),
            ("unregister",     "Remove a project from the daemon's watch list"),
            ("list",           "List every project the daemon is watching"),
            ("watch",          "Inspect or control watchers: `watch status | restart | logs`"),
            ("install-hooks",  "Deprecated — the daemon now owns freshness; prints migration hint"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Run `sovereign project <subcommand> --help` for subcommand-specific flags.",
        ),
    ],
};

const HELP_INIT: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project init",
    summary: "Set up code intelligence for the workspace in the current directory.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign project init [--name <id>] [--port <port>]\n    \
             [--data-dir <dir>] [--workspace-root <path>]\n    \
             [--no-scip] [--no-hooks] [--no-claude-config]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--name <id>",          "Corpus ID (default: directory name)"),
            ("--port <port>",        "MCP server port (default: 9741)"),
            ("--data-dir <dir>",     "Index directory (default: ~/.sovereign/indexes)"),
            ("--workspace-root <p>", "Monorepo root; discover every Cargo/Go/etc. workspace under <p>"),
            ("--no-scip",            "Skip SCIP call graph export"),
            ("--no-hooks",           "Skip git hook installation"),
            ("--no-claude-config",   "Skip writing .claude/settings.json (overrides harness prompt)"),
        ]),
        crate::util::help::HelpSection::Examples(&[
            ("sovereign project init",                           "Index the current workspace"),
            ("sovereign project init --workspace-root ..",       "Index a monorepo from a sibling dir"),
            ("sovereign project init --no-scip",                 "Skip call graph (no exporter installed)"),
        ]),
    ],
};

const HELP_DESIGN: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project design",
    summary: "Agent-collaborative DESIGN.md session. opencode is the blessed path; --solo and --stopgap are fallbacks.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign project design [--import <path>] [--via <agent>]\n    \
             [--solo] [--stopgap] [--port <port>]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--import <path>",  "Copy <path> into <repo>/DESIGN.md (diff-confirms if one already exists)"),
            ("--via <agent>",    "Choose the agent: opencode (default) | claude-code | cursor"),
            ("--solo",           "Skip the agent; walk structural gaps with CLI prompts, write OPEN_QUESTIONS.md"),
            ("--stopgap",        "Provisional embedded CLI chat (banner-labelled; install opencode for the real experience)"),
            ("--port <port>",    "Commonwealth daemon port (default: 9741)"),
        ]),
        crate::util::help::HelpSection::Examples(&[
            ("sovereign project design",                         "Launch opencode with the session brief primed"),
            ("sovereign project design --import ./design.md",    "Import an existing doc, then start the session"),
            ("sovereign project design --solo",                  "No agent — CLI prompts driven by the structural parser"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Requires the Commonwealth daemon (start it with `commonwealth daemon start`).\n\
             The session writes DESIGN.md and OPEN_QUESTIONS.md at repo root; artifacts live in\n\
             .sovereign/.atos/design/<session-id>/.",
        ),
    ],
};

const HELP_PLAN: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project plan",
    summary: "Compose IMPLEMENTATION_PLAN.md from DESIGN.md + OPEN_QUESTIONS.md; upsert rows into .sovereign/plan.db.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign project plan [--allow-open]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--allow-open", "Proceed even if OPEN_QUESTIONS.md has unanswered entries (they surface as open risks on the matching phase)"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Phase 0 = Skeleton (language-specific build+test stop).\n\
             Phases 1..N come from H2 sections in DESIGN.md, in order (skipping Anchors / Open questions).\n\
             Answered OPEN_QUESTIONS.md entries surface as `Resolved (for the record)` on the matching phase.\n\
             Unanswered OQs block the plan unless --allow-open is set; they then surface as open risks.\n\
             Stale plan_items (from an older DESIGN.md) are marked `deferred` rather than deleted, preserving references.",
        ),
    ],
};

const HELP_CHARTER: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project charter",
    summary: "Create or edit the team's free-form CHARTER.md (governance, culture, onboarding). Distinct from DESIGN.md.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign project charter [--print]"),
        crate::util::help::HelpSection::Flags(&[
            ("--print", "Print the current CHARTER.md to stdout and exit without opening $EDITOR"),
        ]),
        crate::util::help::HelpSection::Notes(
            "CHARTER.md is the low-ceremony team governance doc — who we are, how we decide, \
             onboarding pointers. It is NOT auto-generated from DESIGN.md: DESIGN.md says what \
             we're building; CHARTER.md says how we work together on it.\n\n\
             First invocation writes a minimal skeleton and opens $EDITOR. Subsequent invocations \
             just open the existing file. The file lives at `.sovereign/CHARTER.md` (the path \
             `sovereign project amend` already uses for drift detection).",
        ),
    ],
};

/// Minimal free-form CHARTER.md skeleton. Data, not program — lives
/// next to its consumer here rather than in an asset file because
/// it's a one-off with no anticipated operator-tuning. See
/// ARCH_PRINCIPLES.md §6 for when to split prose out.
const CHARTER_SKELETON: &str = r#"# Charter

<!-- Low-ceremony governance + onboarding doc. NOT auto-generated.
     DESIGN.md says what we're building; CHARTER.md says how we work
     together on it. Free-form — write honestly, not form-fill. -->

## Who we are


## How we decide


## Onboarding pointers

<!-- Where should a new teammate start? What should they read first,
     in what order? Pointers to DESIGN.md, IMPLEMENTATION_PLAN.md,
     runbooks, dashboards — whatever they'll need. -->


## Amendment log

<!-- Appended to by `sovereign project amend`. -->
"#;

const HELP_SERVE: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project serve",
    summary: "Start a lightweight MCP server for locally-indexed projects (no model required).",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign project serve [--port <port>] [--data-dir <dir>]\n    \
             [--sovereign-dir <dir>]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--port <port>",         "Listen port (default: 9741)"),
            ("--data-dir <dir>",      "Index directory (default: ~/.sovereign/indexes)"),
            ("--sovereign-dir <dir>", "Path to .sovereign/ (default: nearest ancestor with .sovereign/)"),
        ]),
    ],
};

const HELP_STATUS: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project status",
    summary: "Show the status of code intelligence for the current project.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign project status"),
    ],
};

const HELP_REFRESH: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project refresh",
    summary: "Re-export the SCIP call graph + rebuild the LanceDB index when embeddings are stale.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign project refresh [--quiet] [--rebuild-index]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--quiet", "Suppress progress output (use from hook scripts)"),
            (
                "--rebuild-index",
                "Force-rebuild the LanceDB corpus index (chunks + embeddings) \
                 even when the on-disk embed model matches the current daemon.",
            ),
        ]),
        crate::util::help::HelpSection::Notes(
            "Runs automatically on commit via the installed hook (SCIP only; the hook does \
             NOT force a LanceDB rebuild). The LanceDB index auto-rebuilds whenever this \
             command detects an embed-model mismatch between `_corpus_meta.json` and \
             `SetupConfig.models.embed` — once the indexes are on the current model, \
             subsequent refreshes stay SCIP-only and fast.",
        ),
    ],
};

const HELP_INSTALL_HOOKS: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign project install-hooks",
    summary: "Upgrade (or install) the post-commit hook in the current repo.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign project install-hooks"),
        crate::util::help::HelpSection::Notes(
            "Use this when you've upgraded sovereign-cli and want the hook to pick up the new\n\
             binary without re-running `sovereign project init`.",
        ),
    ],
};

// ─── Init helpers ────────────────────────────────────────────

/// Auto-detected presence of supported AI coding assistants in the
/// user's environment. Replaces the old interactive harness prompt:
/// if a harness isn't detected, we skip it silently — no clutter.
struct HarnessDetection {
    claude_code: bool,
    opencode: bool,
}

/// Detect which coding harnesses are plausibly in use. Checks both
/// project-local dotfolders (`.claude/`, `.opencode/`), home-level
/// dotfolders (`~/.claude/`, `~/.opencode/`), and `PATH` for the
/// binary names. Any one signal flips the harness on.
fn detect_harnesses(project_root: &Path) -> HarnessDetection {
    let home = dirs::home_dir();

    let claude_code = project_root.join(".claude").exists()
        || home.as_ref().is_some_and(|h| h.join(".claude").exists())
        || binary_on_path("claude");

    let opencode = project_root.join(".opencode").exists()
        || home.as_ref().is_some_and(|h| h.join(".opencode").exists())
        || binary_on_path("opencode");

    HarnessDetection { claude_code, opencode }
}

/// Best-effort check for a binary on PATH. Uses `which(1)` on unix and
/// `where.exe` on Windows; returns false on error / not-found.
fn binary_on_path(name: &str) -> bool {
    #[cfg(unix)]
    let cmd = std::process::Command::new("which").arg(name).output();
    #[cfg(windows)]
    let cmd = std::process::Command::new("where").arg(name).output();

    cmd.map(|o| o.status.success()).unwrap_or(false)
}

/// Prompt `Detected <harness>. Write config automatically? [Y/n]` with
/// `Y` as the default. In non-TTY environments (CI, pipes) returns
/// `true` without prompting — matches the `--yes` semantics used by
/// `sovereign setup`.
fn confirm_write_config(harness: &str) -> bool {
    if !io::stdin().is_terminal() {
        return true;
    }
    eprint!("  Detected {harness}. Write config automatically? [Y/n] ");
    io::stderr().flush().ok();
    let mut ans = String::new();
    io::stdin().lock().read_line(&mut ans).unwrap_or(0);
    let trimmed = ans.trim().to_lowercase();
    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
}

/// Probe the Commonwealth OICP capabilities endpoint and return available model IDs.
/// Returns an empty vec silently if Commonwealth is not reachable — this is normal
/// when the daemon isn't running at init time.
async fn fetch_commonwealth_models(commonwealth_url: &str) -> Vec<String> {
    let base = commonwealth_url
        .trim_end_matches('/')
        .replace(":9742", ":9741");
    let url = format!("{base}/oicp/v1/capabilities");

    let resp = match reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };

    resp.json::<sovereign_core::oicp::ProviderManifest>()
        .await
        .map(|m| {
            m.models
                .into_iter()
                .filter(|model| model.status.available)
                .map(|model| model.id)
                .collect()
        })
        .unwrap_or_default()
}

// ─── Init ────────────────────────────────────────────────────

pub(crate) async fn cmd_init(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_INIT);
        return 0;
    }
    let mut name: Option<String> = None;
    let mut no_scip = false;
    let mut no_hooks = false;
    let mut no_claude_config = false;
    let mut port: u16 = 9741;
    let mut data_dir: Option<PathBuf> = None;
    // Monorepo root: when set, sovereign discovers all workspace roots under
    // this path rather than treating the git root as the sole workspace.
    let mut workspace_root_arg: Option<PathBuf> = None;

    // `--yes-git` / `--no-git` let scripted callers bypass the
    // interactive git prompt. `None` means "prompt if TTY, auto-init
    // if not." `Some(true)` means "run git init without asking."
    // `Some(false)` means "do not run git init; skip the prompt."
    let mut git_override: Option<bool> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                name = args.get(i).cloned();
            }
            "--no-scip" => no_scip = true,
            "--no-hooks" => no_hooks = true,
            "--no-claude-config" => no_claude_config = true,
            "--yes-git" => git_override = Some(true),
            "--no-git" => git_override = Some(false),
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<u16>() {
                        Ok(p) => port = p,
                        Err(_) => {
                            eprintln!("error: --port must be a number");
                            return 1;
                        }
                    }
                }
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            "--workspace-root" => {
                i += 1;
                workspace_root_arg = args.get(i).map(PathBuf::from);
            }
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let corpus_id = name.unwrap_or_else(|| {
        repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string()
    });

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("error: cannot create data dir {}: {e}", data_dir.display());
        return 1;
    }

    let has_git = repo_root.join(".git").exists();
    // Read any prior project.toml eagerly so the git flow can honor a
    // previous `git_declined_at_init` (the user answered "no" last
    // time — we don't re-badger). This read is intentionally
    // non-fatal: a missing or unreadable project.toml means "first
    // init", which is the common case.
    let prior_project_toml_path = repo_root.join(".sovereign").join("project.toml");
    let prior_git_declined: bool = crate::project_toml::ProjectTomlFile::read(
        &prior_project_toml_path,
    )
    .map(|t| t.lifecycle.git_declined_at_init)
    .unwrap_or(false);
    let design_md_path = repo_root.join("DESIGN.md");
    let design_exists = design_md_path.exists();

    // Git auto-with-confirm. Runs BEFORE the observation report so the
    // report has an up-to-date `has_git` to render (either "✓ Git
    // repository" or the deferred note). The design-doc presence is
    // passed through because the prompt's kindness wording changes
    // based on whether the user is about to start drafting a
    // DESIGN.md (the main value prop for git) or not.
    let git_outcome = resolve_git(&repo_root, has_git, git_override, design_exists, prior_git_declined);
    let has_git = matches!(git_outcome, GitOutcome::Present | GitOutcome::InitializedNow);

    // Resolve workspace roots for SCIP export and language detection.
    // Single-repo (default): just the git root.
    // Monorepo (--workspace-root): discover sibling workspaces under the given parent.
    let abs_path = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());

    let scip_workspace_roots: Vec<PathBuf> = if let Some(ref wr) = workspace_root_arg {
        let canonical_wr = wr.canonicalize().unwrap_or_else(|_| wr.clone());
        let roots = corpus_engine::scip_export::find_cargo_workspace_roots(&canonical_wr);
        if roots.is_empty() {
            vec![canonical_wr]
        } else {
            roots
        }
    } else {
        vec![abs_path.clone()]
    };

    // ════════════════════════════════════════════════════════════
    println!();
    println!("  Sovereign Project Intelligence");
    println!("  {}", "─".repeat(54));

    // ── Observation pass (M6.1) ─────────────────────────────────
    //
    // Before touching anything on disk, gather every fact about the
    // project we can derive without asking the user. The result
    // feeds both the human-facing report below AND
    // `.sovereign/project.toml` (written later in this function),
    // which `status` / `found` / `doctor` read from rather than
    // re-observing.
    //
    // The report is bucketed per the requirements:
    //   READY      — everything the user doesn't need to act on
    //   ACTIONABLE — install commands, copy-pasteable, unindented
    //   DEFERRED   — things we note now but address in `found`
    let mut observation = crate::observation::observe(&repo_root);
    // If we just ran `git init` in this invocation, the observation
    // (captured before resolve_git) is stale on the `has_git` axis.
    // Patch it so the report reflects reality.
    observation.has_git = has_git;
    let report_ctx = ObservationReportContext {
        design_exists,
        git_declined: matches!(
            git_outcome,
            GitOutcome::DeclinedByUser | GitOutcome::DeclinedPreviously
        ),
    };
    print_observation_report(&observation, &report_ctx);

    // Persist observations BEFORE any indexing/SCIP work so the
    // durable record survives even if downstream init steps fail.
    // Read-modify-write: preserve any existing lifecycle fields so
    // re-running init after `sovereign project found` doesn't reset
    // `founded`, `charter_version`, or `current_phase`.
    let project_toml_path = repo_root.join(".sovereign").join("project.toml");
    if let Err(e) = std::fs::create_dir_all(
        project_toml_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    ) {
        eprintln!("    \u{2717} Cannot create .sovereign/: {e}");
        return 1;
    }
    let mut project_toml = crate::project_toml::ProjectTomlFile::read(&project_toml_path)
        .unwrap_or_else(|_| crate::project_toml::ProjectTomlFile::from_observation(&observation));
    project_toml.update_observation(&observation, &project_toml_path);
    // Persist a fresh git declination if the user just said "no" —
    // but preserve a prior declination (user already said no before).
    // Never un-set: once they've opted out, that stays opted out
    // until they run `git init` themselves.
    if matches!(git_outcome, GitOutcome::DeclinedByUser) {
        project_toml.lifecycle.git_declined_at_init = true;
    }
    if let Err(e) = project_toml.write(&project_toml_path) {
        eprintln!("    \u{2717} Cannot write project.toml: {e}");
    }

    // Detect languages across all workspace roots for downstream
    // SCIP + indexing logic. Display already handled by
    // `print_observation_report` above — no second pass of ✓-lines.
    let langs: Vec<DetectedLanguage> = {
        let mut seen = std::collections::HashSet::new();
        let mut all = Vec::new();
        for root in &scip_workspace_roots {
            for lang in detect_languages(root) {
                if seen.insert(lang.id) {
                    all.push(lang);
                }
            }
        }
        all
    };

    if langs.is_empty() {
        // Pre-code soft path (step 2b): when the user has a DESIGN.md
        // but no source yet, that's a legitimate state — they're
        // about to iterate on their design with the agent before
        // writing a single file. Don't bail; skip indexing, still
        // register with the daemon, and emit a friendly status line.
        //
        // Stateless: derived from DESIGN.md presence each run. No
        // `pre_code` lifecycle flag — see ARCH_PRINCIPLES.md §7 on
        // structural over stateful.
        if design_exists {
            println!();
            println!("    \u{2026} Pre-code project — indexing deferred. Re-run `sovereign project init`");
            println!("      once source lands, or start with `sovereign project design` now.");
        } else if !no_scip {
            // True empty directory: observation report already flagged
            // "no supported languages" as actionable. Repeat only the
            // bail path, but point at the design-first alternative.
            println!();
            println!("    Pass --no-scip to skip indexing and write agent configs anyway,");
            println!("    or run `sovereign project design --import <path-to-doc>` to start");
            println!("    from an existing design document.");
            return 1;
        } else {
            println!();
            println!("    Continuing without indexing (--no-scip).");
        }
    }
    if workspace_root_arg.is_some() {
        println!();
        println!(
            "    Monorepo mode ({} workspace{})",
            scip_workspace_roots.len(),
            if scip_workspace_roots.len() == 1 { "" } else { "s" }
        );
        for root in &scip_workspace_roots {
            println!(
                "          {}",
                root.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            );
        }
    }

    // ── Step 2: Index symbols ───────────────────────────────────
    println!();
    println!("  Indexing symbols...");

    // Remove existing index so re-init is idempotent. The ingest pipeline
    // creates tables from scratch and fails with "table already exists" if
    // a previous completed index is present.
    let existing_index = data_dir.join(&corpus_id);
    if existing_index.exists() {
        // Preserve the scip_graph.db — it's expensive to rebuild and
        // the user may only want to re-index symbols, not re-export SCIP.
        let scip_backup = existing_index.join("scip_graph.db");
        let has_scip_backup = scip_backup.exists();
        let scip_tmp = data_dir.join(format!(".{corpus_id}_scip_graph.db.bak"));
        if has_scip_backup {
            let _ = std::fs::rename(&scip_backup, &scip_tmp);
        }
        if let Err(e) = std::fs::remove_dir_all(&existing_index) {
            eprintln!("    \u{26a0} Cannot remove old index: {e}");
        }
        // Restore SCIP graph after clearing.
        if has_scip_backup {
            let _ = std::fs::create_dir_all(&existing_index);
            let _ = std::fs::rename(&scip_tmp, &scip_backup);
        }
    }

    let recipe_toml = format!(
        r#"[corpus]
id = "{corpus_id}"
name = "{corpus_id}"
description = "Code corpus generated by `sovereign project init`"
license = "private"
mesh_sharing = false
size_compressed_gb = 0
size_indexed_gb = 0

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "code"
context_lines = 3
max_lines_per_chunk = 150

[chunk]
type = "passthrough"

[index]
fts = true
vector = false
"#,
        corpus_id = corpus_id,
        path = abs_path.display(),
    );

    let tempdir = match tempfile_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("    \u{2717} Cannot create temp dir: {e}");
            return 1;
        }
    };
    let recipe_path = tempdir.join(format!("{corpus_id}.toml"));
    if let Err(e) = std::fs::write(&recipe_path, &recipe_toml) {
        eprintln!("    \u{2717} Cannot write recipe: {e}");
        return 1;
    }

    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
    });
    let recipes_dir = tempdir.clone();
    let engine = CorpusEngine::new(recipes_dir, data_dir.clone(), embed)
        .with_embedding_model(&configured_embed_model_name());

    // Progress callback — inline progress bar.
    let progress: corpus_engine::ProgressCallback = Box::new(|p| match p {
        IngestProgress::Extracting { documents_processed } => {
            eprint!(
                "\r    Extracting... {documents_processed} files    "
            );
        }
        IngestProgress::Embedding {
            chunks_embedded,
            total,
            ..
        } => {
            if total > 0 {
                let pct = (chunks_embedded as f32 / total as f32 * 100.0).min(100.0);
                let filled = (pct / 5.0) as usize;
                let empty = 20usize.saturating_sub(filled);
                eprint!(
                    "\r    {}{} {:3.0}%  {} symbols embedded   ",
                    "\u{2588}".repeat(filled),
                    "\u{2591}".repeat(empty),
                    pct,
                    chunks_embedded,
                );
            } else {
                eprint!("\r    Embedding... {chunks_embedded} symbols   ");
            }
        }
        IngestProgress::Indexing {
            chunks_indexed,
            total,
        } => {
            if total > 0 {
                let pct = (chunks_indexed as f32 / total as f32 * 100.0).min(100.0);
                let filled = (pct / 5.0) as usize;
                let empty = 20usize.saturating_sub(filled);
                eprint!(
                    "\r    {}{} {:3.0}%  {} symbols indexed    ",
                    "\u{2588}".repeat(filled),
                    "\u{2591}".repeat(empty),
                    pct,
                    chunks_indexed,
                );
            }
        }
        IngestProgress::Complete {
            total_chunks,
            duration_secs,
        } => {
            eprintln!(
                "\r    \u{2713} {} symbols indexed in {}s                ",
                total_chunks, duration_secs
            );
        }
        _ => {}
    });

    let spec = CorpusSpec::RecipePath(recipe_path);
    match engine.ingest(&spec, Some(progress)).await {
        Ok(result) => {
            // Complete variant already printed by the callback, but
            // if it wasn't triggered, print the summary now.
            if result.chunks_created > 0 {
                eprintln!(
                    "\r    \u{2713} {} symbols indexed                           ",
                    result.chunks_created
                );
            }
        }
        Err(e) => {
            eprintln!();
            eprintln!("    \u{2717} Indexing failed: {e}");
            return 1;
        }
    }

    // ── Step 3: Build call graph ────────────────────────────────
    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");
    let scip_output_dir = tempdir.join("scip");

    if !no_scip {
        println!();
        println!("  Building call graph...");

        let exporter_check =
            corpus_engine::scip_export::check_exporters(&scip_workspace_roots);

        // Surface missing exporters with actionable install instructions.
        for m in &exporter_check.missing {
            println!(
                "    \u{26a0} {} exporter ({}) not found in PATH",
                m.language_id, m.command
            );
            println!("        {}", m.install_hint);
        }

        if exporter_check.available.is_empty() {
            println!("    \u{26a0} No SCIP exporters available — find_callers / find_callees will not work");
            println!("      Install the exporter(s) above then run: sovereign project refresh");
        } else {
            for exporter in &exporter_check.available {
                println!("    Using {}", exporter.command);
            }

            let graph = match corpus_engine::ScipGraph::open(&scip_graph_path, &corpus_id) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("    \u{2717} Cannot open SCIP graph: {e}");
                    return 1;
                }
            };

            let progress_fn = |p: corpus_engine::scip_export::ScipProgress<'_>| match p {
                corpus_engine::scip_export::ScipProgress::Exporting { language } => {
                    eprint!("\r    Exporting {language}...      ");
                }
                corpus_engine::scip_export::ScipProgress::Ingested {
                    language,
                    symbols,
                    refs,
                } => {
                    eprintln!(
                        "\r    \u{2713} {language}: {} symbols, {} references    ",
                        symbols, refs
                    );
                }
                corpus_engine::scip_export::ScipProgress::Skipped { language, reason } => {
                    eprintln!(
                        "\r    \u{26a0} {language}: skipped ({reason})    "
                    );
                }
            };

            match corpus_engine::scip_export::export_all(
                &abs_path,
                &scip_output_dir,
                &graph,
                Some(&scip_workspace_roots),
                &progress_fn,
            )
            .await
            {
                Ok(summary) => {
                    println!(
                        "    \u{2713} {} symbols, {} call edges",
                        summary.total_symbols, summary.total_refs
                    );
                }
                Err(e) => {
                    eprintln!("    \u{2717} SCIP export failed: {e}");
                    eprintln!("      Call graph tools will not be available.");
                    eprintln!("      Run `sovereign project refresh` after fixing the issue.");
                }
            }
        }
    }

    // ── Step 4: Write project files ─────────────────────────────
    println!();
    println!("  Writing project files...");

    // .sovereign/SOVEREIGN.md
    let sovereign_dir = repo_root.join(".sovereign");
    if let Err(e) = std::fs::create_dir_all(&sovereign_dir) {
        eprintln!("    \u{2717} Cannot create .sovereign/: {e}");
        return 1;
    }

    let lang_names: Vec<&str> = langs.iter().map(|l| l.id).collect();
    let md_content = generate_sovereign_md(&corpus_id, port, &lang_names, !no_scip);
    if let Err(e) = std::fs::write(sovereign_dir.join("SOVEREIGN.md"), &md_content) {
        eprintln!("    \u{2717} Cannot write SOVEREIGN.md: {e}");
        return 1;
    }
    println!("    \u{2713} .sovereign/SOVEREIGN.md");

    // .sovereign/project.json — stores init config for status/refresh
    // workspace_roots is only written when --workspace-root was given (monorepo
    // mode). When absent, refresh falls back to auto-detecting from the git root.
    let workspace_roots_json: serde_json::Value = if workspace_root_arg.is_some() {
        serde_json::Value::Array(
            scip_workspace_roots
                .iter()
                .map(|p| serde_json::Value::String(p.to_string_lossy().into_owned()))
                .collect(),
        )
    } else {
        serde_json::Value::Null
    };
    let mut project_config = serde_json::json!({
        "corpus_id": corpus_id,
        "port": port,
        "data_dir": data_dir.to_string_lossy(),
        "scip_enabled": !no_scip,
        "hooks_installed": !no_hooks && has_git && !no_scip,
        "claude_config_written": !no_claude_config,
    });
    if !workspace_roots_json.is_null() {
        project_config["workspace_roots"] = workspace_roots_json;
    }
    if let Err(e) = std::fs::write(
        sovereign_dir.join("project.json"),
        serde_json::to_string_pretty(&project_config).unwrap_or_default(),
    ) {
        eprintln!("    \u{2717} Cannot write project.json: {e}");
        // Non-fatal — status/refresh will just need flags.
    }

    // project.toml was written earlier in the flow (right after
    // observation) so its contents survive even if indexing fails.
    // Surface it here alongside the other config artifacts for
    // discoverability.
    println!("    \u{2713} .sovereign/project.toml");

    // .sovereign/sovereign.toml — starter config for background watchers.
    // Only written if the file doesn't already exist (preserve user edits).
    let toml_path = sovereign_dir.join("sovereign.toml");
    if !toml_path.exists() {
        let abs_root = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.clone());
        let toml_stub = format!(
            "# sovereign.toml — background watcher config for `sovereign project serve`.\n\
             #\n\
             # Uncomment and fill in test_runner / lint_runner to enable the\n\
             # test_status, run_tests, lint_status MCP tools.\n\
             # Commands must emit Tier 2 JSONL on stdout (see SOVEREIGN.md).\n\
             \n\
             # [test_runner]\n\
             # command = \"cargo test 2>&1\"\n\
             # working_dir = \"{root}\"\n\
             # timeout_secs = 300\n\
             # debounce_ms = 2000\n\
             \n\
             # [lint_runner]\n\
             # command = \"cargo check --message-format json 2>&1 | sovereign-cargo-check-adapter\"\n\
             # working_dir = \"{root}\"\n\
             # timeout_secs = 120\n\
             # debounce_ms = 1000\n",
            root = abs_root.display()
        );
        if let Err(e) = std::fs::write(&toml_path, &toml_stub) {
            eprintln!("    \u{26a0} Could not write sovereign.toml: {e}");
        } else {
            println!("    \u{2713} .sovereign/sovereign.toml (starter config)");
        }
    }

    // Auto-detect which AI coding assistants are present. If none, we write
    // nothing — no clutter for non-AI-coding projects. If detected, we ask
    // once per harness whether to write its config.
    let detected = detect_harnesses(&repo_root);
    let write_claude = detected.claude_code
        && !no_claude_config
        && confirm_write_config("Claude Code");
    let write_opencode = detected.opencode && confirm_write_config("opencode");

    // Commonwealth URL: after `sovereign setup`, the local daemon always
    // lives at http://localhost:9741 and serves both /v1 and /mcp. Users
    // who want to point at a remote Commonwealth can override via the
    // legacy `[commonwealth]` section in sovereign.toml.
    let commonwealth_url: Option<String> = {
        let toml_path = repo_root.join(".sovereign/sovereign.toml");
        let from_toml = std::fs::read_to_string(&toml_path)
            .ok()
            .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
            .and_then(|v| {
                v.get("commonwealth")
                    .and_then(|c| c.get("url"))
                    .and_then(|u| u.as_str())
                    .map(str::to_owned)
            });
        from_toml.or_else(|| write_opencode.then(|| "http://localhost:9741".to_string()))
    };

    // Probe OICP capabilities to enumerate real model IDs. Falls back to []
    // gracefully if Commonwealth is not running at init time.
    let commonwealth_models: Vec<String> = if let Some(ref url) = commonwealth_url {
        fetch_commonwealth_models(url).await
    } else {
        vec![]
    };

    // .claude/settings.json + .claude/hooks/inject-notes.sh
    if write_claude {
        let claude_dir = repo_root.join(".claude");
        if let Err(e) = std::fs::create_dir_all(&claude_dir) {
            eprintln!("    \u{2717} Cannot create .claude/: {e}");
            return 1;
        }

        // Write the UserPromptSubmit hook script. It fetches active invariants
        // and decisions from the sovereign MCP server and injects them as
        // context before every Claude response — no manual read_notes call needed.
        let hooks_dir = claude_dir.join("hooks");
        if let Err(e) = std::fs::create_dir_all(&hooks_dir) {
            eprintln!("    \u{2717} Cannot create .claude/hooks/: {e}");
        } else {
            let hook_path = hooks_dir.join("inject-notes.sh");
            let hook_script = generate_inject_notes_script(port);
            match std::fs::write(&hook_path, &hook_script) {
                Ok(()) => {
                    // Make executable.
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = std::fs::set_permissions(
                            &hook_path,
                            std::fs::Permissions::from_mode(0o755),
                        );
                    }
                    println!("    \u{2713} .claude/hooks/inject-notes.sh");
                }
                Err(e) => eprintln!("    \u{2717} Cannot write inject-notes.sh: {e}"),
            }
        }

        let settings_path = claude_dir.join("settings.json");
        let generated = generate_claude_settings(port, &corpus_id, has_git, !no_scip);

        let final_content = if settings_path.exists() {
            match std::fs::read_to_string(&settings_path) {
                Ok(existing) => merge_claude_settings(&existing, &generated),
                Err(_) => generated,
            }
        } else {
            generated
        };

        if let Err(e) = std::fs::write(&settings_path, &final_content) {
            eprintln!("    \u{2717} Cannot write settings.json: {e}");
            return 1;
        }
        println!("    \u{2713} .claude/settings.json");
    }

    // .opencode/config.json + AGENTS.md
    if write_opencode {
        let opencode_dir = repo_root.join(".opencode");
        if let Err(e) = std::fs::create_dir_all(&opencode_dir) {
            eprintln!("    \u{26a0} Cannot create .opencode/: {e}");
        } else {
            let config_path = opencode_dir.join("config.json");
            let generated =
                generate_opencode_config(port, commonwealth_url.as_deref(), &commonwealth_models);
            let final_content = if config_path.exists() {
                match std::fs::read_to_string(&config_path) {
                    Ok(existing) => merge_opencode_config(&existing, &generated),
                    Err(_) => generated,
                }
            } else {
                generated
            };
            match std::fs::write(&config_path, &final_content) {
                Ok(()) => println!("    \u{2713} .opencode/config.json"),
                Err(e) => eprintln!("    \u{26a0} Cannot write .opencode/config.json: {e}"),
            }

            // ATOS opencode plugin — the binary embeds the
            // canonical source and writes it here with a
            // versioned header. Upgrades land with `sovereign
            // atos install-plugin` or a subsequent `project init`.
            match crate::atos_plugin::install_plugin(&repo_root) {
                Ok(crate::atos_plugin::InstallOutcome::Installed) => {
                    println!(
                        "    \u{2713} {} (v{})",
                        crate::atos_plugin::plugin_rel_path(),
                        crate::atos_plugin::PLUGIN_VERSION
                    );
                }
                Ok(crate::atos_plugin::InstallOutcome::UpToDate) => {
                    println!(
                        "    \u{2713} {} (up to date at v{})",
                        crate::atos_plugin::plugin_rel_path(),
                        crate::atos_plugin::PLUGIN_VERSION
                    );
                }
                Ok(crate::atos_plugin::InstallOutcome::Replaced { prior_version }) => {
                    println!(
                        "    \u{2713} {} (v{} → v{})",
                        crate::atos_plugin::plugin_rel_path(),
                        prior_version.as_deref().unwrap_or("unversioned"),
                        crate::atos_plugin::PLUGIN_VERSION
                    );
                }
                Err(e) => {
                    eprintln!(
                        "    \u{26a0} Cannot write {}: {e}",
                        crate::atos_plugin::plugin_rel_path()
                    );
                }
            }
        }

        // AGENTS.md — only write if absent; it's project-specific and users edit it.
        let agents_path = repo_root.join("AGENTS.md");
        if !agents_path.exists() {
            let content =
                generate_agents_md(&corpus_id, port, !no_scip, commonwealth_url.as_deref());
            match std::fs::write(&agents_path, &content) {
                Ok(()) => println!("    \u{2713} AGENTS.md"),
                Err(e) => eprintln!("    \u{26a0} Cannot write AGENTS.md: {e}"),
            }
        }
    }

    // .gitignore
    if has_git {
        if let Err(e) = update_gitignore(&repo_root) {
            eprintln!("    \u{2717} Cannot update .gitignore: {e}");
            // Non-fatal.
        } else {
            println!("    \u{2713} .gitignore updated");
        }
    }

    // ── Step 5: Legacy git-hook cleanup ─────────────────────────
    //
    // Earlier sovereign versions installed a post-commit hook that
    // shelled out to `sovereign project refresh`. The daemon now
    // owns freshness (FS watcher + git HEAD poll + startup
    // catch-up), so the hook is redundant and has been a common
    // source of silent staleness when the binary path drifted.
    // Remove any legacy hook we find; never install a new one.
    if has_git {
        match remove_legacy_hook(&repo_root) {
            Ok(true) => {
                println!();
                println!("  Cleaned up legacy post-commit hook — the daemon now keeps");
                println!("  the graph fresh automatically.");
            }
            Ok(false) => { /* nothing to remove */ }
            Err(e) => {
                eprintln!("    \u{26a0} Could not clean up legacy hook: {e}");
            }
        }
        let _ = no_hooks;
    }

    // ── Step 6: Register with the running daemon ────────────────
    //
    // The daemon's Reindexer picks this up immediately — no
    // restart needed. If the daemon isn't running we fall through
    // silently; the next `sovereign daemon restart` (or startup)
    // will pick up the registry entry via `Registry::load()`.
    if !no_scip {
        println!();
        println!("  Registering with daemon...");
        let register_body = serde_json::json!({
            "corpus_id": corpus_id,
            "root": abs_path.display().to_string(),
        });
        match daemon_post("/v1/projects/register", register_body).await {
            Ok(_) => {
                println!("    \u{2713} Daemon is now watching this project");
            }
            Err(e) => {
                println!("    \u{26a0} Could not reach the daemon ({e}).");
                println!("      The registry entry was still written; the daemon will");
                println!("      pick it up on next start. Try `sovereign daemon status`.");
            }
        }
    }

    // ── Step 7: MCP server check ────────────────────────────────
    println!();
    println!("  MCP server...");

    let mcp_url = format!("http://localhost:{port}/mcp");
    if check_mcp_server(&mcp_url).await {
        println!("    \u{2713} {mcp_url}");
    } else {
        println!("    \u{26a0} Not running at {mcp_url}");
        println!("      Start with: sovereign daemon restart");
    }

    // ── Done ────────────────────────────────────────────────────
    println!();
    println!("  {}", "─".repeat(54));
    println!("  Ready. Open Claude Code in this directory.");
    println!();
    println!("  Quick check:");
    println!("    sovereign project status");
    println!("    sovereign project watch status");
    println!();

    // Design-first nudge (step 2c). The user just got a tool set up;
    // point them at the next natural step — DESIGN.md-driven agent
    // collaboration — without making it feel mandatory. Only surface
    // when there's genuinely no DESIGN.md yet and the project isn't
    // already founded (no point suggesting `project design` to
    // someone who's already past that stage).
    if !design_exists && !project_toml.lifecycle.founded {
        println!(
            "  Next: `sovereign project design` — I'll work with the agent on your DESIGN.md."
        );
        println!(
            "        Bring a path to an existing doc with `--import <path>`, or start blank."
        );
        println!();
    }

    0
}

// ─── Design session ──────────────────────────────────────────
//
// Step 4 of the ATOS onboarding redesign. `cmd_design` is the
// agent-collaborative main event — init scaffolds the project;
// design is where the user + agent iterate on DESIGN.md together.
//
// Most of the work lives in `crate::design_session`; `cmd_design`
// parses args, resolves the repo root + project id, and hands off.
pub(crate) async fn cmd_design(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_DESIGN);
        return 0;
    }
    let mut import_path: Option<PathBuf> = None;
    let mut port: u16 = 9741;
    let mut transport = crate::design_session::TransportChoice::Default;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--import" => {
                i += 1;
                import_path = args.get(i).map(PathBuf::from);
            }
            "--via" => {
                i += 1;
                transport = match args.get(i).map(String::as_str) {
                    Some("opencode") => crate::design_session::TransportChoice::Opencode,
                    Some("claude-code") => crate::design_session::TransportChoice::ClaudeCode,
                    Some("cursor") => {
                        eprintln!("warning: --via cursor is not yet implemented; falling back to default.");
                        crate::design_session::TransportChoice::Default
                    }
                    Some(other) => {
                        eprintln!("error: --via expects opencode | claude-code | cursor; got `{other}`");
                        return 1;
                    }
                    None => {
                        eprintln!("error: --via requires an agent name");
                        return 1;
                    }
                };
            }
            "--solo" => transport = crate::design_session::TransportChoice::Solo,
            "--stopgap" => transport = crate::design_session::TransportChoice::Stopgap,
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<u16>() {
                        Ok(p) => port = p,
                        Err(_) => {
                            eprintln!("error: --port must be a number");
                            return 1;
                        }
                    }
                }
            }
            flag if flag.starts_with("--") => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let project_id = derive_project_id(&repo_root);

    let req = crate::design_session::SessionRequest {
        repo_root,
        transport,
        import_path,
        daemon_port: port,
        project_id,
    };

    crate::design_session::run(req).await
}

// ─── Plan ────────────────────────────────────────────────────
//
// Step 6 of the ATOS onboarding redesign. `cmd_plan` reads
// DESIGN.md + OPEN_QUESTIONS.md, composes IMPLEMENTATION_PLAN.md at
// repo root, and upserts plan_items rows into .sovereign/plan.db.
// Composition lives in `crate::plan_composer` (pure); this handler
// does the IO, ordering, and indexing.
pub(crate) async fn cmd_plan(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_PLAN);
        return 0;
    }

    let mut allow_open = false;
    for arg in args {
        match arg.as_str() {
            "--allow-open" => allow_open = true,
            flag if flag.starts_with("--") => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    // Preflight: DESIGN.md must exist. An absent DESIGN.md means the
    // user hasn't run `project design` yet — point them at it
    // rather than silently conjuring a plan from thin air.
    let design_path = repo_root.join("DESIGN.md");
    let design_md = match std::fs::read_to_string(&design_path) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("  \u{2717} No DESIGN.md at repo root ({}).", design_path.display());
            eprintln!("    Run `sovereign project design` first to author or import one.");
            return 2;
        }
    };
    if design_md.trim().is_empty() {
        eprintln!("  \u{2717} DESIGN.md exists but is empty. Write some content first.");
        return 2;
    }

    // Parse OPEN_QUESTIONS.md (absent is fine — treated as "no
    // outstanding questions"; the plan will contain only
    // non-attached risks, none).
    let oq_path = repo_root.join("OPEN_QUESTIONS.md");
    let oq_text = std::fs::read_to_string(&oq_path).unwrap_or_default();
    let oqs = crate::plan_composer::parse_open_questions(&oq_text);

    // Unanswered-OQ gate. Default behavior is to block, because a
    // plan that papers over known load-bearing gaps is a worse
    // artifact than no plan — the user should see the gaps and
    // either answer them (best) or explicitly --allow-open (if
    // they're accepting the risk knowingly).
    let unanswered: Vec<_> = oqs.iter().filter(|o| !o.is_answered()).collect();
    if !unanswered.is_empty() && !allow_open {
        eprintln!();
        eprintln!("  \u{26a0} {} unanswered question(s) in OPEN_QUESTIONS.md:", unanswered.len());
        for oq in &unanswered {
            eprintln!("    · {} ({})", oq.id, oq.anchor);
        }
        eprintln!();
        eprintln!("    Answer them inline, or re-run with --allow-open to surface them");
        eprintln!("    as `Open risks` on the matching phase.");
        return 2;
    }

    // Resolve project id from repo root (same logic `found` uses).
    let project_id = derive_project_id(&repo_root);

    // Primary language: read from project.toml's observation section
    // so `plan` agrees with `init`/`found` without re-scanning. If
    // project.toml isn't present (user ran plan outside an init'd
    // repo), fall through with None — Phase 0 gets a generic stop.
    let project_toml_path = repo_root.join(".sovereign").join("project.toml");
    let primary_language: Option<String> =
        crate::project_toml::ProjectTomlFile::read(&project_toml_path)
            .ok()
            .and_then(|t| t.observation.languages.into_iter().next())
            .map(|l| l.id);

    // Compose (pure).
    let signals = corpus_engine::design_signals::extract(&design_md);
    let today = today_iso();
    let composed = crate::plan_composer::compose_plan(&crate::plan_composer::ComposeInputs {
        project_id: &project_id,
        design_md: &design_md,
        signals: &signals,
        open_questions: &oqs,
        primary_language: primary_language.as_deref(),
        today: &today,
    });

    // Persist plan_items rows. Open a plan.db under .sovereign/.
    // The store tolerates running before .sovereign/ exists (creates
    // parent dirs) but in practice init has already built the tree.
    let plan_db = repo_root.join(".sovereign").join("plan.db");
    let store = match corpus_engine::plan_items::PlanStore::open(&plan_db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  \u{2717} could not open plan.db: {e}");
            return 1;
        }
    };
    let now = unix_now_secs();
    // Defer stale rows from prior generations (different design_hash)
    // BEFORE writing fresh ones, so a new plan never collides with
    // open items from a prior DESIGN.md state.
    match store.defer_stale(&composed.design_hash, now).await {
        Ok(n) if n > 0 => {
            eprintln!("    \u{2026} {n} plan_item(s) from an older DESIGN.md state deferred.");
        }
        Ok(_) => {}
        Err(e) => eprintln!("    \u{26a0} defer_stale warning: {e}"),
    }
    for item in &composed.items {
        let depends_on = Vec::<String>::new();
        let stored = corpus_engine::plan_items::PlanItem {
            id: item.id.clone(),
            phase: item.phase,
            title: item.title.clone(),
            body: item.body.clone(),
            realizes: item.realizes.clone(),
            depends_on,
            stop_hint: item.stop_hint.clone(),
            state: corpus_engine::plan_items::PlanItemState::Open,
            design_hash: composed.design_hash.clone(),
            created_at: now,
            updated_at: now,
        };
        if let Err(e) = store.upsert(&stored).await {
            eprintln!("    \u{26a0} plan_items upsert failed for {}: {e}", item.id);
        }
    }

    // Write IMPLEMENTATION_PLAN.md at repo root (discoverable from
    // the repo listing + GitHub file tree, same convention as
    // DESIGN.md and OPEN_QUESTIONS.md).
    let plan_md_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if let Err(e) = std::fs::write(&plan_md_path, &composed.markdown) {
        eprintln!("  \u{2717} could not write IMPLEMENTATION_PLAN.md: {e}");
        return 1;
    }

    // Index the plan into ProjectDocsStore so `project_context`
    // queries surface it. Best-effort — an index failure shouldn't
    // abort; the markdown is the source of truth, the index is a
    // query acceleration.
    let docs_db_path = repo_root.join(".sovereign").join("project_docs.db");
    match corpus_engine::ProjectDocsStore::open(&docs_db_path) {
        Ok(docs) => {
            if let Err(e) = docs.index_file(&plan_md_path, &repo_root).await {
                eprintln!("    \u{26a0} project_docs index failed: {e}");
            }
        }
        Err(e) => eprintln!("    \u{26a0} could not open project_docs.db: {e}"),
    }

    // Summary.
    eprintln!();
    eprintln!(
        "  \u{2713} IMPLEMENTATION_PLAN.md written ({} phase(s), DESIGN.md sha=`{}`).",
        composed.items.len(),
        composed.design_hash
    );
    eprintln!(
        "    plan.db: {} row(s) upserted at {}",
        composed.items.len(),
        plan_db.display()
    );
    if !unanswered.is_empty() {
        eprintln!(
            "    ({} unanswered OPEN_QUESTIONS entry(s) surfaced as open risks via --allow-open.)",
            unanswered.len()
        );
    }
    eprintln!();
    eprintln!("    Next: iterate on DESIGN.md; re-run `sovereign project plan` to regenerate.");
    0
}

// ─── Amend routing ─────────────────────────────────────────────

/// Amend DESIGN.md: open in $EDITOR, diff H2 sections against the
/// curated catalog (`design.anchors` / `design.data-interfaces` /
/// `design.open-questions`), ask the matching adversarial question
/// for each changed section, and append the Q&A to
/// `## Amendment log` inside DESIGN.md itself.
///
/// Does NOT bump any lifecycle flags — DESIGN.md is iterative by
/// design (pun intended). The provenance is the log + git history.
async fn cmd_amend_design(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("sovereign project amend design");
        println!();
        println!("Opens <repo>/DESIGN.md in $EDITOR. On save, section-level diff");
        println!("against the curated DESIGN catalog. For each changed section,");
        println!("asks one adversarial question. Appends the Q&A to DESIGN.md's");
        println!("`## Amendment log` section (creating it if absent).");
        return 0;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let path = crate::amend::design_md_path(&repo_root);
    let old_text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("  \u{2717} No DESIGN.md at repo root ({}).", path.display());
            eprintln!("    Run `sovereign project design` first.");
            return 2;
        }
    };

    // Snapshot BEFORE opening the editor so we can diff after.
    let old_signals = corpus_engine::design_signals::extract(&old_text);
    let old_hash = crate::found::hash_charter(&old_text);

    // Hand off to $EDITOR. `invoke_editor` returns the re-read file
    // contents on a clean exit, None if the editor failed — in
    // which case we exit without writing anything.
    let new_text = match crate::amend::invoke_editor(&path) {
        Some(s) => s,
        None => {
            eprintln!("  \u{2717} editor did not return cleanly — no amendment recorded.");
            return 1;
        }
    };
    if new_text == old_text {
        eprintln!("  \u{2026} DESIGN.md unchanged — nothing to amend.");
        return 0;
    }

    let new_signals = corpus_engine::design_signals::extract(&new_text);
    let new_hash = crate::found::hash_charter(&new_text);
    let changed = crate::amend::changed_design_sections(&old_signals, &new_signals);

    // Build adversarial Q&A — but only for curated sections. Non-
    // curated edits still get logged (the diff is real) but without
    // a forced question set. Keeps the catalog precise.
    let mut interlocutor = crate::amend::StdinAmendmentInterlocutor::new();
    let questions = crate::amend::questions_for(&changed);
    let mut qa: Vec<(crate::amend::AdversarialQuestion, String)> =
        Vec::with_capacity(questions.len());
    if !questions.is_empty() {
        eprintln!();
        eprintln!(
            "  {} curated section{} changed — adversarial review:",
            questions.len(),
            if questions.len() == 1 { "" } else { "s" }
        );
        for q in &questions {
            let answer =
                crate::amend::AmendmentInterlocutor::ask_adversarial(&mut interlocutor, q);
            qa.push((q.clone(), answer));
        }
    } else {
        eprintln!();
        eprintln!(
            "  \u{2026} DESIGN.md edited but no curated sections changed — logging the \
             diff without a Q&A (anchors/data-interfaces/open-questions untouched)."
        );
    }

    // Render the amendment-log entry and splice it into the
    // DESIGN.md we're about to persist. Newest entries at the top
    // of the log — matches the append_design_amendment_log
    // implementation and the convention in CHARTER.md's log.
    let today = today_iso();
    let entry =
        crate::amend::render_design_amendment_entry(&today, &qa, &old_hash, &new_hash);
    let final_md = crate::amend::append_design_amendment_log(&new_text, &entry);

    if let Err(e) = std::fs::write(&path, &final_md) {
        eprintln!("  \u{2717} could not write {}: {e}", path.display());
        return 1;
    }

    // Best-effort re-index.
    let docs_db = repo_root.join(".sovereign").join("project_docs.db");
    match corpus_engine::ProjectDocsStore::open(&docs_db) {
        Ok(docs) => {
            if let Err(e) = docs.index_file(&path, &repo_root).await {
                eprintln!("    \u{26a0} project_docs index failed: {e}");
            }
        }
        Err(e) => eprintln!("    \u{26a0} could not open project_docs.db: {e}"),
    }

    eprintln!(
        "  \u{2713} DESIGN.md amended ({} section{} with Q&A; old sha=`{}` → new sha=`{}`).",
        qa.len(),
        if qa.len() == 1 { "" } else { "s" },
        &old_hash[..old_hash.len().min(12)],
        &new_hash[..new_hash.len().min(12)],
    );
    eprintln!("    Amendment log updated inline at top of the section.");
    eprintln!("    Next: re-run `sovereign project plan` if DESIGN.md structure changed.");
    0
}

// ─── Charter ─────────────────────────────────────────────────
//
// Step 7 of the ATOS onboarding redesign. `cmd_charter` is the
// free-form culture/governance doc — distinct from DESIGN.md, which
// is the technical design artifact. First invocation writes a
// minimal skeleton and opens $EDITOR; subsequent runs just open
// the existing file.
//
// The existing canonical path (`.sovereign/CHARTER.md`) is
// preserved so `sovereign project amend` and its drift detection
// stay wired up. (The plan's longer-term repo-root-move is a
// separate migration that affects amend + drift detection and
// hasn't landed yet.)
pub(crate) async fn cmd_charter(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_CHARTER);
        return 0;
    }

    let mut print_only = false;
    for arg in args {
        match arg.as_str() {
            "--print" => print_only = true,
            flag if flag.starts_with("--") => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let charter_path = crate::amend::charter_path(&repo_root);
    let sovereign_dir = match charter_path.parent() {
        Some(p) => p.to_path_buf(),
        None => {
            eprintln!("  \u{2717} could not resolve .sovereign/ parent of {}", charter_path.display());
            return 1;
        }
    };

    // Ensure .sovereign/ exists — init creates it, but cmd_charter
    // might be invoked in a repo where init was skipped. Safe/cheap.
    if let Err(e) = std::fs::create_dir_all(&sovereign_dir) {
        eprintln!("  \u{2717} could not create {}: {e}", sovereign_dir.display());
        return 1;
    }

    let created_fresh = !charter_path.exists();
    if created_fresh {
        if let Err(e) = std::fs::write(&charter_path, CHARTER_SKELETON) {
            eprintln!("  \u{2717} could not write {}: {e}", charter_path.display());
            return 1;
        }
        eprintln!(
            "  \u{2713} CHARTER.md skeleton written at {}.",
            charter_path.display()
        );
    }

    if print_only {
        let text = std::fs::read_to_string(&charter_path).unwrap_or_default();
        println!("{text}");
        return 0;
    }

    // Open in $EDITOR. `crate::amend::invoke_editor` already owns
    // the shell-escape + waitpid dance and is exactly what amend
    // uses when the user edits the charter — one code path, one
    // set of quirks.
    let _ = crate::amend::invoke_editor(&charter_path);

    // Post-edit: re-hash the file, index it, and persist the hash
    // into project.toml so amend's drift detection has a baseline.
    let charter_text = match std::fs::read_to_string(&charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  \u{2717} could not re-read {}: {e}", charter_path.display());
            return 1;
        }
    };
    let new_hash = crate::found::hash_charter(&charter_text);

    let project_toml_path = sovereign_dir.join("project.toml");
    if let Ok(mut tf) = crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        tf.lifecycle.charter_hash = new_hash.clone();
        if let Err(e) = tf.write(&project_toml_path) {
            eprintln!("    \u{26a0} could not persist charter_hash to project.toml: {e}");
        }
    }

    // Index via ProjectDocsStore so `project_context("who we are")`
    // surfaces charter content. Best-effort — the markdown is the
    // source of truth; the index is a query accelerator.
    let docs_db = sovereign_dir.join("project_docs.db");
    match corpus_engine::ProjectDocsStore::open(&docs_db) {
        Ok(docs) => {
            if let Err(e) = docs.index_file(&charter_path, &repo_root).await {
                eprintln!("    \u{26a0} project_docs index failed: {e}");
            }
        }
        Err(e) => eprintln!("    \u{26a0} could not open project_docs.db: {e}"),
    }

    eprintln!(
        "  \u{2713} charter saved ({} bytes, sha=`{}`).",
        charter_text.len(),
        &new_hash[..new_hash.len().min(12)]
    );
    if created_fresh {
        eprintln!("    Next: fill in the Onboarding pointers section — new teammates read this first.");
    }
    0
}

// ─── Status ──────────────────────────────────────────────────

pub(crate) async fn cmd_status(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_STATUS);
        return 0;
    }
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    // Load project config if available.
    let config = load_project_config(&repo_root);
    let corpus_id = config
        .as_ref()
        .and_then(|c| c["corpus_id"].as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string()
        });
    let port = config
        .as_ref()
        .and_then(|c| c["port"].as_u64())
        .unwrap_or(9741) as u16;

    println!();
    println!("  {corpus_id}");
    println!("  {}", "─".repeat(50));
    println!();

    // Index
    let index_path = data_dir.join(&corpus_id);
    if index_path.exists() {
        match corpus_engine::CorpusIndex::open(&index_path).await {
            Ok(idx) => match idx.info().await {
                Ok(info) => {
                    let age = format_age(info.last_updated);
                    println!(
                        "  Index         \u{2713} {} symbols  last updated {age}",
                        info.chunk_count
                    );
                }
                Err(_) => {
                    println!("  Index         \u{2713} present (cannot read stats)");
                }
            },
            Err(_) => {
                println!("  Index         \u{2717} corrupt or unreadable");
            }
        }
    } else {
        println!("  Index         \u{2717} not found");
        println!("                  Run: sovereign project init");
    }

    // Call graph
    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");
    if scip_graph_path.exists() {
        match corpus_engine::ScipGraph::open(&scip_graph_path, &corpus_id) {
            Ok(graph) => {
                let sym_count = graph.symbol_count().await;
                let ref_count = graph.ref_count().await;
                let stale_count = graph.stale_file_count().await;
                if stale_count > 0 {
                    println!(
                        "  Call graph    \u{26a0} {} symbols, {} edges  ({stale_count} files modified since last export)",
                        sym_count, ref_count
                    );
                    println!("                  Run: sovereign project refresh");
                } else {
                    println!(
                        "  Call graph    \u{2713} {} symbols, {} edges",
                        sym_count, ref_count
                    );
                }
            }
            Err(_) => {
                println!("  Call graph    \u{2717} corrupt or unreadable");
            }
        }
    } else {
        let scip_enabled = config
            .as_ref()
            .and_then(|c| c["scip_enabled"].as_bool())
            .unwrap_or(true);
        if scip_enabled {
            println!("  Call graph    \u{2717} not exported");
            println!("                  Run: sovereign project refresh");
        } else {
            println!("  Call graph    \u{2500} disabled (--no-scip)");
        }
    }

    // MCP server
    let mcp_url = format!("http://localhost:{port}/mcp");
    if check_mcp_server(&mcp_url).await {
        println!("  MCP server    \u{2713} {mcp_url}");
    } else {
        println!("  MCP server    \u{2717} not running");
        println!("                  Run: sovereign-server --config <config.toml>");
    }

    // SOVEREIGN.md
    let sovereign_md = repo_root.join(".sovereign").join("SOVEREIGN.md");
    if sovereign_md.exists() {
        println!("  SOVEREIGN.md  \u{2713} .sovereign/SOVEREIGN.md");
    } else {
        println!("  SOVEREIGN.md  \u{2717} not found");
    }

    // Claude config
    let claude_settings = repo_root.join(".claude").join("settings.json");
    if claude_settings.exists() {
        // Check if it has sovereign MCP config.
        let has_sovereign = std::fs::read_to_string(&claude_settings)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["mcpServers"]["sovereign"].as_object().cloned())
            .is_some();
        if has_sovereign {
            println!("  Claude config \u{2713} .claude/settings.json");
        } else {
            println!("  Claude config \u{26a0} .claude/settings.json (no sovereign MCP entry)");
        }
    } else {
        println!("  Claude config \u{2717} not found");
    }

    // Git hook
    let hook_path = repo_root.join(".git").join("hooks").join("post-commit");
    if hook_path.exists() {
        let contents = std::fs::read_to_string(&hook_path).unwrap_or_default();
        if contents.contains(SOVEREIGN_HOOK_MARKER) {
            println!("  Git hook      \u{2713} installed (v3: symbols + SCIP)");
        } else if contents.contains("sovereign") && contents.contains("project refresh") {
            println!(
                "  Git hook      \u{26a0} prior version (refreshes SCIP only) — run \
                 `sovereign project install-hooks` to upgrade"
            );
        } else {
            println!("  Git hook      \u{2717} exists but missing sovereign refresh");
        }
    } else if repo_root.join(".git").exists() {
        println!("  Git hook      \u{2717} not installed");
    }

    // Tools available
    println!();
    println!("  Tools available:");
    print!("    symbol_lookup    recent_changes    code_search");
    let scip_available = scip_graph_path.exists();
    if scip_available {
        println!();
        println!("    find_callers     find_callees");
    } else {
        println!();
    }

    println!();
    println!("  Run `sovereign project refresh` to update the call graph.");
    println!();

    0
}

// ─── Refresh ─────────────────────────────────────────────────

pub(crate) async fn cmd_refresh(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_REFRESH);
        return 0;
    }
    let mut quiet = false;
    let mut local = false;
    let mut data_dir: Option<PathBuf> = None;
    let mut explicit_name: Option<String> = None;
    // `--rebuild-index` forces a LanceDB rebuild unconditionally.
    // Without it, the LanceDB rebuild only runs when an embed-
    // model mismatch is detected (see `needs_lancedb_rebuild`).
    let mut force_rebuild_index = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--quiet" | "-q" => quiet = true,
            // Escape hatch: run the full in-process export instead
            // of nudging the daemon. Useful when the daemon is
            // down or the user is debugging the exporter itself.
            "--local" => local = true,
            "--rebuild-index" => force_rebuild_index = true,
            "--name" => {
                i += 1;
                explicit_name = args.get(i).cloned();
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    // Default path: nudge the running daemon so its Reindexer
    // handles the rebuild in-process. Coalesces with any FS /
    // git-poll signals that might have fired concurrently; keeps
    // the CLI decoupled from the exporter plumbing. Falls back to
    // the legacy in-process path when --local is set or the
    // daemon is unreachable.
    if !local {
        let corpus_id = explicit_name
            .clone()
            .unwrap_or_else(|| derive_corpus_id(&repo_root));
        match daemon_post(
            &format!("/v1/projects/{corpus_id}/rebuild"),
            serde_json::json!({ "reason": "cli refresh" }),
        )
        .await
        {
            Ok(_) => {
                if !quiet {
                    println!("  \u{2713} Rebuild nudged for \"{corpus_id}\".");
                    println!("    Check progress with `sovereign project watch status {corpus_id}`.");
                }
                // SCIP is nudged; now gate the LanceDB corpus
                // rebuild on either the explicit `--rebuild-index`
                // flag or a detected embed-model mismatch. Common
                // case on an already-migrated installation: no
                // mismatch → no-op → `refresh` stays fast.
                let data_dir_for_rebuild = data_dir
                    .clone()
                    .or_else(default_data_dir)
                    .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));
                let abs_repo = repo_root
                    .canonicalize()
                    .unwrap_or_else(|_| repo_root.clone());
                return maybe_rebuild_lancedb_corpus(
                    &abs_repo,
                    &corpus_id,
                    &data_dir_for_rebuild,
                    force_rebuild_index,
                    quiet,
                )
                .await;
            }
            Err(e) => {
                if !quiet {
                    eprintln!("  \u{26a0} Daemon nudge failed: {e}");
                    eprintln!("    Falling back to local in-process rebuild.");
                }
                // Fall through to legacy path below.
            }
        }
    }
    let _ = explicit_name;

    let abs_path = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    let config = load_project_config(&repo_root);
    let corpus_id = config
        .as_ref()
        .and_then(|c| c["corpus_id"].as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string()
        });

    // Workspace roots: read from project.json if stored (monorepo mode set at init).
    // None means export_all will auto-detect from the git root (single-repo default).
    let scip_workspace_roots: Option<Vec<PathBuf>> = config
        .as_ref()
        .and_then(|c| c["workspace_roots"].as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(PathBuf::from))
                .collect()
        });

    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");

    if !quiet {
        eprintln!("  Refreshing call graph...");
    }

    // Surface missing exporters before running so the user gets actionable
    // guidance rather than a silent empty call graph.
    {
        let check_roots: Vec<PathBuf> = scip_workspace_roots
            .as_deref()
            .map(|r| r.to_vec())
            .unwrap_or_else(|| vec![abs_path.clone()]);
        let check = corpus_engine::scip_export::check_exporters(&check_roots);
        for m in &check.missing {
            if !quiet {
                eprintln!(
                    "  \u{26a0} {} exporter ({}) not found in PATH",
                    m.language_id, m.command
                );
                eprintln!("    {}", m.install_hint);
                eprintln!("    Install it and re-run `sovereign project refresh`");
            }
        }
    }

    // Use the integrity-checking open so a v1 / corrupt DB left over
    // from a past schema is self-healed into a fresh v2 DB rather than
    // wedging on `no such column: corpus_id` at index-creation time —
    // which is exactly what `sovereign doctor`'s `scip_integrity`
    // repair hint used to run into. Mirrors `Reindexer::register`.
    let graph = match corpus_engine::ScipGraph::open_with_integrity(
        &scip_graph_path,
        &corpus_id,
    ) {
        Ok(g) => g,
        Err(corpus_engine::OpenError::Corrupt { moved_to }) => {
            if !quiet {
                eprintln!(
                    "  \u{26a0} SCIP DB was corrupt; quarantined to {}",
                    moved_to.display()
                );
                eprintln!("    Rebuilding from scratch.");
            }
            match corpus_engine::ScipGraph::open_with_integrity(
                &scip_graph_path,
                &corpus_id,
            ) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!(
                        "error: cannot open SCIP graph after quarantine: {e}"
                    );
                    return 1;
                }
            }
        }
        Err(corpus_engine::OpenError::SchemaMismatch { found, expected }) => {
            if !quiet {
                eprintln!(
                    "  \u{26a0} SCIP DB schema v{found} is stale (current: v{expected}); resetting."
                );
            }
            if let Err(e) = reset_scip_db(&scip_graph_path) {
                eprintln!(
                    "error: cannot reset stale SCIP DB at {}: {e}",
                    scip_graph_path.display()
                );
                return 1;
            }
            match corpus_engine::ScipGraph::open_with_integrity(
                &scip_graph_path,
                &corpus_id,
            ) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!(
                        "error: cannot open SCIP graph after reset: {e}"
                    );
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("error: cannot open SCIP graph: {e}");
            eprintln!("Run `sovereign project init` first.");
            return 1;
        }
    };

    // Get pre-refresh counts for delta display.
    let prev_symbols = graph.symbol_count().await;
    let prev_refs = graph.ref_count().await;

    let tempdir = match tempfile_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot create temp dir: {e}");
            return 1;
        }
    };
    let scip_output_dir = tempdir.join("scip");

    let progress_fn = |p: corpus_engine::scip_export::ScipProgress<'_>| {
        if quiet {
            return;
        }
        match p {
            corpus_engine::scip_export::ScipProgress::Exporting { language } => {
                eprint!("\r    Exporting {language}...      ");
            }
            corpus_engine::scip_export::ScipProgress::Ingested {
                language,
                symbols,
                refs,
            } => {
                eprintln!(
                    "\r    \u{2713} {language}: {} symbols, {} references    ",
                    symbols, refs
                );
            }
            corpus_engine::scip_export::ScipProgress::Skipped { language, reason } => {
                eprintln!("\r    \u{26a0} {language}: skipped ({reason})    ");
            }
        }
    };

    let start = std::time::Instant::now();
    match corpus_engine::scip_export::export_all(
        &abs_path,
        &scip_output_dir,
        &graph,
        scip_workspace_roots.as_deref(),
        &progress_fn,
    )
    .await
    {
        Ok(summary) => {
            let elapsed = start.elapsed().as_secs();
            let sym_delta = summary.total_symbols as i64 - prev_symbols as i64;
            let ref_delta = summary.total_refs as i64 - prev_refs as i64;

            if !quiet {
                eprintln!(
                    "    \u{2713} {} symbols ({}{})",
                    summary.total_symbols,
                    if sym_delta >= 0 { "+" } else { "" },
                    sym_delta
                );
                eprintln!(
                    "    \u{2713} {} edges ({}{})",
                    summary.total_refs,
                    if ref_delta >= 0 { "+" } else { "" },
                    ref_delta
                );
                eprintln!("    \u{2713} Done in {elapsed} seconds");
            }
            // Fall through to the LanceDB rebuild gate — see the
            // matching branch in the daemon-nudge path above for
            // the rationale.
            maybe_rebuild_lancedb_corpus(
                &abs_path,
                &corpus_id,
                &data_dir,
                force_rebuild_index,
                quiet,
            )
            .await
        }
        Err(e) => {
            eprintln!("error: SCIP export failed: {e}");
            1
        }
    }
}

/// Decide whether the LanceDB corpus index needs to be rebuilt,
/// then do it (or skip explaining why).
///
/// Rebuild triggers, in precedence order:
///   1. `--rebuild-index` on the command line (operator override).
///   2. The on-disk `_corpus_meta.json.embedding_model` differs from
///      the daemon's currently-configured embed model stem. This is
///      the common "I changed embed models, reindex" path and the
///      one that rescues the historical 768-dim-zero-vector code
///      corpora.
///   3. No `_corpus_meta.json` exists — corpus isn't indexed yet;
///      treat the first refresh as an initial build.
///
/// On skip, prints a one-liner so the user sees the branch was
/// considered rather than silently bypassed (glassbox principle).
async fn maybe_rebuild_lancedb_corpus(
    abs_repo: &Path,
    corpus_id: &str,
    data_dir: &Path,
    force: bool,
    quiet: bool,
) -> i32 {
    let meta_path = data_dir.join(corpus_id).join("_corpus_meta.json");
    let current_embed_stem = sovereign_core::setup_config::SetupConfig::load()
        .ok()
        .and_then(|c| {
            c.models
                .embed
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        });

    let reason = lancedb_rebuild_reason(force, &meta_path, current_embed_stem.as_deref());
    let reason = match reason {
        Some(r) => r,
        None => {
            if !quiet {
                eprintln!(
                    "  \u{2713} LanceDB index current — skipping (pass --rebuild-index to force)."
                );
            }
            return 0;
        }
    };

    if !quiet {
        eprintln!();
        eprintln!("  Rebuilding LanceDB corpus index: {reason}");
    }
    match crate::code_cmd::rebuild_code_corpus(abs_repo, corpus_id, data_dir).await {
        Ok(stats) => {
            if !quiet {
                eprintln!(
                    "  \u{2713} LanceDB: {} chunks in {}s ({} KB)",
                    stats.chunks_created,
                    stats.duration_secs,
                    stats.index_size_bytes / 1024,
                );
            }
            0
        }
        Err(e) => {
            eprintln!("  \u{2717} LanceDB rebuild failed: {e}");
            1
        }
    }
}

/// Returns `Some(human-readable reason)` when the LanceDB index
/// should be rebuilt, `None` when it can be left alone. Pure for
/// testability — no filesystem mutations, no daemon calls.
///
/// Deliberately does NOT do a string comparison between the
/// on-disk `embedding_model` and the daemon's model stem. The
/// corpus-engine ingest path today writes its recipe-level default
/// (`qwen3-embedding-0.6b`) to `_corpus_meta.json` regardless of
/// which model actually produced the vectors, so the names drift
/// even when the embeddings are fully compatible. Dim-based checks
/// are the reliable truth — a 1024-dim query against a 1024-dim
/// index works regardless of model-name cosmetics; a 768-dim
/// index is always the legacy zero-vector artefact and needs
/// rebuilding.
///
/// `current_embed_stem` is accepted for future use (when corpus-
/// engine's ingest is fixed to record the real model name) but is
/// currently unused. The compiler won't flag it unused because we
/// accept it by value.
fn lancedb_rebuild_reason(
    force: bool,
    meta_path: &Path,
    _current_embed_stem: Option<&str>,
) -> Option<String> {
    if force {
        return Some("--rebuild-index flag set".into());
    }
    let meta_bytes = match std::fs::read_to_string(meta_path) {
        Ok(b) => b,
        Err(_) => {
            return Some(format!(
                "no existing index at {} — first build",
                meta_path.display()
            ));
        }
    };
    let meta: serde_json::Value = match serde_json::from_str(&meta_bytes) {
        Ok(v) => v,
        Err(e) => {
            return Some(format!(
                "could not parse existing {}: {e}",
                meta_path.display()
            ));
        }
    };
    if meta
        .get("embedding_dimensions")
        .and_then(|v| v.as_u64())
        == Some(768)
    {
        return Some("on-disk index is 768-dim (legacy zero-vector code index)".into());
    }
    None
}

#[cfg(test)]
mod lancedb_rebuild_tests {
    use super::*;

    #[test]
    fn force_flag_always_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        assert!(lancedb_rebuild_reason(true, &missing, Some("any")).is_some());
    }

    #[test]
    fn missing_meta_triggers_first_build() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("_corpus_meta.json");
        let r = lancedb_rebuild_reason(false, &missing, Some("qwen-embedding-0.6b")).unwrap();
        assert!(r.contains("no existing index"));
    }

    #[test]
    fn matching_embed_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("_corpus_meta.json");
        std::fs::write(
            &p,
            r#"{"embedding_model":"qwen-embedding-0.6b","embedding_dimensions":1024}"#,
        )
        .unwrap();
        assert!(lancedb_rebuild_reason(false, &p, Some("qwen-embedding-0.6b")).is_none());
    }

    #[test]
    fn name_difference_alone_does_not_trigger_rebuild() {
        // corpus-engine's ingest writes `qwen3-embedding-0.6b` as
        // a recipe-default cosmetic label even when the real
        // embed model is `qwen-embedding-0.6b`. If the dims
        // match, the vectors are compatible; name drift should
        // NOT force a pointless reindex.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("_corpus_meta.json");
        std::fs::write(
            &p,
            r#"{"embedding_model":"qwen3-embedding-0.6b","embedding_dimensions":1024}"#,
        )
        .unwrap();
        assert!(lancedb_rebuild_reason(false, &p, Some("qwen-embedding-0.6b")).is_none());
    }

    #[test]
    fn legacy_768_dim_triggers_rebuild_even_with_matching_name() {
        // Historical case: code indexes wrote `qwen-embedding-0.6b`
        // as the model name but hardcoded 768-dim zero vectors.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("_corpus_meta.json");
        std::fs::write(
            &p,
            r#"{"embedding_model":"qwen-embedding-0.6b","embedding_dimensions":768}"#,
        )
        .unwrap();
        let r = lancedb_rebuild_reason(false, &p, Some("qwen-embedding-0.6b")).unwrap();
        assert!(r.contains("768-dim"));
    }
}

/// Remove a SCIP graph DB file and its SQLite journal sidecars so
/// a subsequent `open_with_integrity` starts from a clean slate.
/// Missing files are not errors — a partial reset is still a reset.
fn reset_scip_db(db_path: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let p = if suffix.is_empty() {
            db_path.to_path_buf()
        } else {
            let name = db_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("scip_graph.db");
            db_path.with_file_name(format!("{name}{suffix}"))
        };
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ─── Serve ───────────────────────────────────────────────────

pub(crate) async fn cmd_serve(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_SERVE);
        return 0;
    }

    // Check whether the daemon already owns :9741. If so, running
    // the legacy in-process server on top collides (silent bind
    // failure) and degrades the user's MCP surface. Refuse to
    // start and redirect to the daemon workflow.
    if daemon_is_running().await {
        eprintln!();
        eprintln!("  `sovereign project serve` is superseded.");
        eprintln!();
        eprintln!("  The running sovereign daemon already serves MCP on :9741 and");
        eprintln!("  owns freshness (FS watcher + git HEAD poll + startup catch-up).");
        eprintln!("  There's no need to run a second server on top of it.");
        eprintln!();
        eprintln!("  To have the daemon watch this project:");
        eprintln!("    sovereign project register");
        eprintln!();
        eprintln!("  To inspect watcher state:");
        eprintln!("    sovereign project watch status");
        eprintln!();
        eprintln!("  If you really want the legacy in-process server (e.g. the daemon");
        eprintln!("  is broken and you need a fallback), stop the daemon first:");
        eprintln!("    launchctl stop com.sovereign.daemon   # macOS");
        eprintln!("    systemctl --user stop sovereign       # Linux");
        return 1;
    }

    let mut port: u16 = 9741;
    let mut data_dir: Option<PathBuf> = None;
    let mut sovereign_dir_arg: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<u16>() {
                        Ok(p) => port = p,
                        Err(_) => {
                            eprintln!("error: --port must be a number");
                            return 1;
                        }
                    }
                }
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            "--sovereign-dir" => {
                i += 1;
                sovereign_dir_arg = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    if !data_dir.exists() {
        eprintln!("error: index directory does not exist: {}", data_dir.display());
        eprintln!("Run `sovereign project init` in at least one project first.");
        return 1;
    }

    eprintln!("  Sovereign Code Intelligence MCP Server");
    eprintln!("  {}", "─".repeat(54));

    // ── Build CorpusEngine (zero-vector, no model) ──────────────

    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
    });
    let recipes_dir = data_dir.clone();
    let engine = Arc::new(
        CorpusEngine::new(recipes_dir, data_dir.clone(), embed)
            .with_embedding_model(&configured_embed_model_name()),
    );

    // List discovered indexes.
    match engine.installed_indexes().await {
        Ok(indexes) => {
            let code_indexes: Vec<_> = indexes
                .iter()
                // Accept any index with content — the model string is
                // informational after setup no longer locks everyone to a
                // single default.
                .filter(|i| i.chunk_count > 0)
                .collect();
            if code_indexes.is_empty() {
                eprintln!("  warning: no indexes found in {}", data_dir.display());
            } else {
                eprintln!("  Corpora:");
                for info in &code_indexes {
                    eprintln!("    \u{2713} {} ({} symbols)", info.corpus_id, info.chunk_count);
                }
            }
        }
        Err(e) => {
            eprintln!("  warning: could not list indexes: {e}");
        }
    }

    // ── Discover and merge SCIP graphs ──────────────────────────

    eprintln!();
    eprintln!("  Call graph:");

    let (initial_graph, _summary) = load_merged_graph(&data_dir, true).await;
    let merged_graph: sovereign_tools::ScipGraphHandle =
        Arc::new(ArcSwap::from_pointee(initial_graph));
    let health_checker = Arc::new(sovereign_tools::IndexHealthChecker::new(Arc::clone(&merged_graph)));

    // Spawn the background reloader: every 30s, stat each scip_graph.db,
    // and if any mtime changed (or a file appeared/disappeared) rebuild the
    // merged graph and swap it in atomically. Tools grab `load_full()` per
    // query so the swap is lock-free.
    {
        let handle = Arc::clone(&merged_graph);
        let dir = data_dir.clone();
        tokio::spawn(async move {
            scip_graph_reloader(handle, dir).await;
        });
    }

    // ── Repo root + sovereign config ────────────────────────────
    //
    // Priority: nearest ancestor with .sovereign/ > git root > cwd.
    // This allows `sovereign project serve` to be launched from a monorepo
    // root that is not itself a git repository.

    let cwd = std::env::current_dir().ok().unwrap_or_else(|| PathBuf::from("."));
    let sovereign_dir = sovereign_dir_arg
        .map(|p| if p.is_absolute() { p } else { cwd.join(p) })
        .or_else(|| find_sovereign_dir(&cwd))
        .or_else(|| find_repo_root().map(|r| r.join(".sovereign")))
        .unwrap_or_else(|| cwd.join(".sovereign"));
    let repo_root = sovereign_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| cwd.clone());
    let sovereign_cfg = corpus_engine::SovereignConfig::load_or_default(&sovereign_dir);

    // ── Open result stores (SQLite, always-on) ──────────────────
    eprintln!();
    eprintln!("  Stores:");

    let test_store = match corpus_engine::TestResultStore::open(
        &data_dir.join("test_results.db"),
    ) {
        Ok(s) => {
            eprintln!("  test_results.db  ✓");
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("  warning: could not open test results DB: {e}");
            Arc::new(
                corpus_engine::TestResultStore::open(std::path::Path::new(":memory:"))
                    .expect("in-memory test store"),
            )
        }
    };

    let lint_store = match corpus_engine::LintResultStore::open(
        &data_dir.join("lint_results.db"),
    ) {
        Ok(s) => {
            eprintln!("  lint_results.db  ✓");
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("  warning: could not open lint results DB: {e}");
            Arc::new(
                corpus_engine::LintResultStore::open(std::path::Path::new(":memory:"))
                    .expect("in-memory lint store"),
            )
        }
    };

    // ── Notes store ─────────────────────────────────────────────

    let notes_db_path = sovereign_dir.join("notes.db");
    let notes_store = match corpus_engine::NoteStore::open(&notes_db_path) {
        Ok(s) => {
            eprintln!("  notes.db         ✓");
            // Write a pointer file so `sovereign reflect` can find this
            // database from any working directory, regardless of where the
            // user invokes it from.
            let pointer_dir = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".sovereign");
            let _ = std::fs::create_dir_all(&pointer_dir);
            let _ = std::fs::write(
                pointer_dir.join("active_notes_db"),
                notes_db_path.to_string_lossy().as_bytes(),
            );
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("  warning: could not open notes DB: {e}");
            Arc::new(
                corpus_engine::NoteStore::open(std::path::Path::new(":memory:"))
                    .expect("in-memory notes store"),
            )
        }
    };

    // ── Feature store (ATOS charters + milestones) ─────────────
    let features_db_path = sovereign_dir.join("features.db");
    let features_store = match corpus_engine::FeatureStore::open(&features_db_path) {
        Ok(s) => {
            eprintln!("  features.db      ✓");
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("  warning: could not open features DB: {e}");
            Arc::new(
                corpus_engine::FeatureStore::open(std::path::Path::new(":memory:"))
                    .expect("in-memory features store"),
            )
        }
    };

    // Print any open todos from previous sessions at startup.
    if let Ok(todos) = notes_store.open_todos(5).await {
        if !todos.is_empty() {
            eprintln!();
            eprintln!(
                "  {} open todo{} from previous sessions:",
                todos.len(),
                if todos.len() == 1 { "" } else { "s" }
            );
            for t in &todos {
                let preview: String = t.content.chars().take(80).collect();
                eprintln!("    [todo] {preview}");
            }
            eprintln!("  Use read_notes to retrieve full context.");
        }
    }

    // ── Project docs store ───────────────────────────────────────

    let docs_store = match corpus_engine::ProjectDocsStore::open(
        &data_dir.join("project_docs.db"),
    ) {
        Ok(s) => {
            let store = Arc::new(s);
            // Index on first run without blocking serve startup.
            if store.is_empty().await.unwrap_or(true) {
                let s2 = Arc::clone(&store);
                let root = repo_root.clone();
                tokio::spawn(async move {
                    let files = corpus_engine::find_markdown_files(&root);
                    let mut count = 0usize;
                    for f in &files {
                        count += s2.index_file(f, &root).await.unwrap_or(0);
                    }
                    if count > 0 {
                        tracing::info!(
                            "indexed {} doc chunks from {} md files",
                            count,
                            files.len()
                        );
                    }
                });
            }
            eprintln!("  project_docs.db  ✓");
            Some(store)
        }
        Err(e) => {
            eprintln!("  warning: could not open project docs DB: {e}");
            None
        }
    };

    // ── Build background watchers ───────────────────────────────

    let test_watcher: Option<Arc<corpus_engine::TestWatcher>> =
        sovereign_cfg.test_runner.as_ref().map(|cfg| {
            let working_dir = cfg.working_dir.as_ref().map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() { p } else { repo_root.join(p) }
            });
            eprintln!(
                "  test_runner      ✓  {}",
                cfg.command.chars().take(60).collect::<String>()
            );
            Arc::new(corpus_engine::TestWatcher::new(
                &cfg.command,
                working_dir,
                cfg.timeout_secs.unwrap_or(300),
                Arc::clone(&test_store),
            ))
        });

    let lint_watcher: Option<Arc<corpus_engine::LintWatcher>> =
        sovereign_cfg.lint_runner.as_ref().map(|cfg| {
            let working_dir = cfg.working_dir.as_ref().map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() { p } else { repo_root.join(p) }
            });
            eprintln!(
                "  lint_runner      ✓  {}",
                cfg.command.chars().take(60).collect::<String>()
            );
            Arc::new(corpus_engine::LintWatcher::new(
                &cfg.command,
                working_dir,
                cfg.timeout_secs.unwrap_or(120),
                Arc::clone(&lint_store),
            ))
        });

    if test_watcher.is_none() && lint_watcher.is_none() {
        eprintln!(
            "  warning: no watchers configured — add [test_runner] / [lint_runner] \
             to {}",
            sovereign_dir.join("sovereign.toml").display()
        );
    }

    // Scope strings for lint/test status tools — shown to agents so they can
    // confirm the watcher covers the crates they just edited.
    let test_watched_scope: Option<String> = sovereign_cfg
        .test_runner
        .as_ref()
        .map(|c| c.command.clone());
    let lint_watched_scope: Option<String> = sovereign_cfg
        .lint_runner
        .as_ref()
        .map(|c| c.command.clone());

    // Shared flag: set to true after coordinator.start() succeeds. Tools expose
    // this as watcher_active so agents know the FS watcher is live.
    let watcher_active_flag = std::sync::Arc::new(
        std::sync::atomic::AtomicBool::new(false),
    );

    // ── Register tools ──────────────────────────────────────────

    let mut tools = sovereign_core::ToolRegistry::new();
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
        Arc::clone(&engine),
    )));
    tools.register(Box::new(
        sovereign_tools::CodeSearchTool::new(Arc::clone(&engine)),
    ));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&engine),
    )));
    tools.register(Box::new(sovereign_tools::FindCalleesTool::new(
        Arc::clone(&engine),
        Arc::clone(&merged_graph),
    ).with_health_checker(Arc::clone(&health_checker))));
    tools.register(Box::new(sovereign_tools::FindCallersTool::new(
        Arc::clone(&engine),
        Arc::clone(&merged_graph),
    ).with_health_checker(Arc::clone(&health_checker))));

    // ── Test / lint watcher tools ───────────────────────────────

    {
        let mut tool = sovereign_tools::TestStatusTool::new(Arc::clone(&test_store))
            .with_watcher_active(Arc::clone(&watcher_active_flag));
        if let Some(scope) = test_watched_scope {
            tool = tool.with_watched_scope(scope);
        }
        tools.register(Box::new(tool));
    }
    if let Some(ref watcher) = test_watcher {
        tools.register(Box::new(sovereign_tools::RunTestsTool::new(
            Arc::clone(watcher),
        )));
    }
    tools.register(Box::new(sovereign_tools::GetRunOutputTool::new(
        Arc::clone(&test_store),
    )));

    {
        let mut tool = sovereign_tools::LintStatusTool::new(Arc::clone(&lint_store))
            .with_watcher_active(Arc::clone(&watcher_active_flag));
        if let Some(scope) = lint_watched_scope {
            tool = tool.with_watched_scope(scope);
        }
        tools.register(Box::new(tool));
    }
    tools.register(Box::new(sovereign_tools::GetLintOutputTool::new(
        Arc::clone(&lint_store),
    )));

    // ── Agent partnership tools (notes, blast radius, project context) ──

    tools.register(Box::new(sovereign_tools::WriteNoteTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::ReadNotesTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(
        sovereign_tools::BlastRadiusTool::new(Arc::clone(&merged_graph))
            .with_project_root(repo_root.clone())
            .with_health_checker(Arc::clone(&health_checker)),
    ));
    if let Some(ref ds) = docs_store {
        tools.register(Box::new(
            sovereign_tools::ProjectContextTool::new(Arc::clone(ds))
                .with_features(Arc::clone(&features_store)),
        ));
    }

    // ── ATOS feature management ─────────────────────────────────
    tools.register(Box::new(sovereign_tools::ProvisionFeatureTool::new(
        Arc::clone(&features_store),
    )));
    tools.register(Box::new(sovereign_tools::ArchiveFeatureTool::new(
        Arc::clone(&features_store),
    )));
    tools.register(Box::new(sovereign_tools::ReadNoteByIdTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::PromoteNoteTool::new(
        Arc::clone(&notes_store),
    )));
    // ReadNoteDigestTool runs in fallback (header-only) mode here —
    // `sovereign project serve` doesn't load a model, so the Fast-slot
    // summarization path is unavailable. The banner in the fallback
    // digest makes the degraded state visible to agents. The daemon
    // binary wires inference in via `.with_inference(...)`.
    tools.register(Box::new(sovereign_tools::ReadNoteDigestTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::RecordAtosEventTool::new(
        Arc::clone(&features_store),
    )));
    tools.register(Box::new(sovereign_tools::WriteRedteamFindingTool::new(
        Arc::clone(&notes_store),
    )));

    // ── Session reflection (feedback loop) ─────────────────────────────
    tools.register(Box::new(sovereign_tools::SessionReflectionTool::new(
        Arc::clone(&notes_store),
    )));

    // ── Doc path checker ────────────────────────────────────────────────
    tools.register(Box::new(
        sovereign_tools::CheckDocPathsTool::new()
            .with_project_root(repo_root.clone()),
    ));

    // ── DESIGN.md structural signals ────────────────────────────────────
    //
    // Project-scoped: bound to this repo's DESIGN.md by default, so the
    // agent can call `design_signals_extract()` with no args and get the
    // right file. Absolute paths still work — the tool resolves them
    // verbatim, bypassing project_root.
    tools.register(Box::new(
        sovereign_tools::DesignSignalsExtractTool::new()
            .with_project_root(repo_root.clone()),
    ));

    // ── Start watcher coordinator ───────────────────────────────

    let debounce_ms = sovereign_cfg
        .test_runner
        .as_ref()
        .and_then(|c| c.debounce_ms)
        .or_else(|| sovereign_cfg.lint_runner.as_ref().and_then(|c| c.debounce_ms))
        .unwrap_or(500);

    let mut coordinator = corpus_engine::WatcherCoordinator::new(debounce_ms);
    if let Some(ref w) = test_watcher {
        coordinator.register(Arc::clone(w) as Arc<dyn corpus_engine::BackgroundWatcher>);
    }
    if let Some(ref w) = lint_watcher {
        coordinator.register(Arc::clone(w) as Arc<dyn corpus_engine::BackgroundWatcher>);
    }
    if let Some(ref ds) = docs_store {
        let pw = corpus_engine::ProjectIndexWatcher::new(
            Arc::clone(ds),
            repo_root.clone(),
        );
        coordinator.register(Arc::new(pw) as Arc<dyn corpus_engine::BackgroundWatcher>);
    }

    let _coordinator_handle = if !coordinator.registered_ids().is_empty() {
        match coordinator.start(vec![repo_root.clone()]).await {
            Ok(handle) => {
                eprintln!("  Watcher started (watching {})", repo_root.display());
                watcher_active_flag.store(true, std::sync::atomic::Ordering::Release);
                Some(handle)
            }
            Err(e) => {
                eprintln!("  warning: could not start watcher: {e}");
                None
            }
        }
    } else {
        None
    };

    let tools = Arc::new(tools);
    eprintln!();
    eprintln!("  Tools: {} registered", tools.count());

    // ── Start MCP HTTP server ───────────────────────────────────

    let bind_addr = format!("127.0.0.1:{port}");
    eprintln!("  Listening on http://{bind_addr}/mcp");
    eprintln!();
    eprintln!("  {}", "─".repeat(54));
    eprintln!("  Ready. Configure Claude Code with:");
    eprintln!();
    eprintln!("    {{");
    eprintln!("      \"mcpServers\": {{");
    eprintln!("        \"sovereign\": {{");
    eprintln!("          \"type\": \"http\",");
    eprintln!("          \"url\": \"http://localhost:{port}/mcp\"");
    eprintln!("        }}");
    eprintln!("      }}");
    eprintln!("    }}");
    eprintln!();

    // Stable session ID for this server run — used by tool_call_log to group
    // calls from the same sovereign project serve invocation.
    let mcp_session_id = format!("serve-{}", uuid::Uuid::new_v4());

    // Phase 5: standalone `sovereign serve` always knows its project
    // root (resolved above as `repo_root`). Pass it as the
    // FeatureRoot so `tools/list` consults
    // `.sovereign/features/*/spec.md` and only advertises the
    // spec-gated tools (`spec`, `drift`) when a spec exists. Cache
    // is process-global so repeated `tools/list` calls amortise the
    // stat against a 1-second TTL.
    let feature_root = sovereign_mesh::mcp_router::FeatureRoot::new(Some(repo_root.clone()));
    let app = sovereign_mesh::mcp_router::mcp_router(
        tools,
        Arc::clone(&notes_store),
        mcp_session_id,
        feature_root,
    );

    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind to {bind_addr}: {e}");
            return 1;
        }
    };

    let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    if let Err(e) = axum::serve(listener, service).await {
        eprintln!("error: server failed: {e}");
        return 1;
    }

    0
}

// ─── SCIP graph loading & hot-reload ──────────────────────────

/// Summary returned by [`load_merged_graph`] — aggregated counts for the
/// startup banner and structured logging.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MergedGraphSummary {
    #[allow(dead_code)]
    graphs_found: usize,
    #[allow(dead_code)]
    total_symbols: usize,
    #[allow(dead_code)]
    total_refs: usize,
}

/// Walk `data_dir/*/scip_graph.db` and merge each into a fresh in-memory
/// ScipGraph. If `verbose`, prints a per-graph line to stderr (used for
/// the startup banner); reloads pass `false`.
///
/// `pub(crate)` so `tools_cmd::registry` can reuse it without
/// duplicating the discovery / merge logic.
pub(crate) async fn load_merged_graph(
    data_dir: &Path,
    verbose: bool,
) -> (corpus_engine::ScipGraph, MergedGraphSummary) {
    let merged = corpus_engine::ScipGraph::open_in_memory("merged")
        .expect("in-memory ScipGraph");

    let mut summary = MergedGraphSummary::default();

    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let scip_path = path.join("scip_graph.db");
            if !scip_path.exists() {
                continue;
            }
            let corpus_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");
            match merged.import_from_path(&scip_path).await {
                Ok((syms, refs)) => {
                    if syms > 0 || refs > 0 {
                        if verbose {
                            eprintln!(
                                "    \u{2713} {corpus_name}: {} symbols, {} edges",
                                syms, refs
                            );
                        }
                        summary.total_symbols += syms;
                        summary.total_refs += refs;
                        summary.graphs_found += 1;
                    }
                }
                Err(e) => {
                    if verbose {
                        eprintln!("    \u{2717} {corpus_name}: {e}");
                    } else {
                        tracing::warn!(
                            corpus = %corpus_name,
                            error = %e,
                            "scip reload: import_from_path failed"
                        );
                    }
                }
            }
        }
    }

    if verbose {
        if summary.graphs_found == 0 {
            eprintln!("    (none — run `sovereign project init` with SCIP exporters)");
        } else {
            eprintln!(
                "    Total: {} symbols, {} edges across {} projects",
                summary.total_symbols, summary.total_refs, summary.graphs_found
            );
        }
    }

    (merged, summary)
}

/// Collect the current mtimes of every `scip_graph.db` file under `data_dir`.
/// Missing files are simply not represented in the map. A reload is
/// triggered whenever this map changes (a key appears, disappears, or its
/// mtime advances).
fn snapshot_graph_mtimes(data_dir: &Path) -> HashMap<PathBuf, SystemTime> {
    let mut out = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let scip_path = path.join("scip_graph.db");
            if let Ok(md) = std::fs::metadata(&scip_path) {
                if let Ok(mtime) = md.modified() {
                    out.insert(scip_path, mtime);
                }
            }
        }
    }
    out
}

/// Poll `data_dir` for SCIP graph file changes every 30 seconds. On any
/// change, rebuild the merged graph out-of-band and atomically swap it
/// into `handle`. Tools (FindCalleesTool, FindCallersTool) pick up the
/// new graph on their next `load_full()`.
async fn scip_graph_reloader(
    handle: sovereign_tools::ScipGraphHandle,
    data_dir: PathBuf,
) {
    const POLL_INTERVAL: Duration = Duration::from_secs(30);

    let mut last_seen = snapshot_graph_mtimes(&data_dir);
    tracing::debug!(
        watched = last_seen.len(),
        "scip reloader: polling scip_graph.db files every 30s"
    );

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        let current = snapshot_graph_mtimes(&data_dir);
        if current == last_seen {
            continue;
        }

        tracing::info!(
            prev_graphs = last_seen.len(),
            current_graphs = current.len(),
            "scip reloader: change detected, rebuilding merged graph"
        );

        let (fresh, summary) = load_merged_graph(&data_dir, false).await;
        handle.store(Arc::new(fresh));
        last_seen = current;

        tracing::info!(
            graphs = summary.graphs_found,
            symbols = summary.total_symbols,
            edges = summary.total_refs,
            "scip reloader: merged graph swapped"
        );
    }
}


// ─── sovereign project found (M6.3) ──────────────────────────
//
// Structured founding conversation. M6.3 ships Stage 1 only:
// Understanding. Later milestones layer stages 2-4 on top.
//
// Flow:
//   1. Read .sovereign/project.toml. If `lifecycle.founded == true`,
//      refuse — founding is once per project (amendments come via
//      `sovereign project amend`, M6.7).
//   2. Load a design doc if --design <path> given. Missing path is
//      treated the same as "no design doc" — we elicit instead.
//   3. Run Stage 1 (see `found::run_stage1`): curated-catalog
//      question selection filtered by observation + design presence,
//      each Q&A persisted as a decision-kind note in Global scope.
//   4. Print a summary with the count of recorded decisions and a
//      pointer to where stage 2 picks up (when M6.4 lands).
//
// Every Q&A lives in the NoteStore permanently. A sessions six
// weeks later reading notes by symbol or kind can see "this was
// asked, answered, and committed to."

// ─── Orchestrator path (step 8, opt-in via `--orchestrate`) ──
//
// Replaces the Stage-1 / Stage-2 questionnaire with a sequencing
// check: DESIGN.md + CHARTER.md + IMPLEMENTATION_PLAN.md must
// already exist (produced by `project design`, `project charter`,
// `project plan`). Skipping the questionnaire is the point — the
// design session already captured everything it could, and the
// plan composer already derived the phase skeleton from DESIGN.md.
//
// This path IS additive — default `project found` behavior is
// unchanged. The existing byte-exact charter-composition test
// suite stays green because we don't touch `compose_charter`.
// A future session can retire the questionnaire path in a
// dedicated pass.
async fn cmd_found_orchestrate(design_flag: Option<&Path>) -> i32 {
    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let sovereign_dir = repo_root.join(".sovereign");
    let project_toml_path = sovereign_dir.join("project.toml");
    if !project_toml_path.exists() {
        eprintln!();
        eprintln!(
            "  sovereign project found --orchestrate: no .sovereign/project.toml found.\n\
             Run `sovereign project init` first."
        );
        return 1;
    }
    let mut project_toml = match crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  orchestrator: cannot read project.toml: {e}");
            return 1;
        }
    };
    if project_toml.lifecycle.founded {
        eprintln!();
        eprintln!(
            "  \u{2713} already founded (charter_version={}, current_phase={}).",
            project_toml.lifecycle.charter_version, project_toml.lifecycle.current_phase
        );
        eprintln!("    Re-run edits via `sovereign project amend design` / `amend charter`.");
        return 0;
    }

    eprintln!();
    eprintln!("  Sovereign Project Founding — orchestrated mode");
    eprintln!("  {}", "─".repeat(54));

    // Gate 1: DESIGN.md. If `--design <path>` was passed, import
    // it through the existing onboarding helper so the diff-confirm
    // path is consistent.
    let design_md_path = repo_root.join("DESIGN.md");
    if let Some(src) = design_flag {
        match crate::design_onboarding::import_design(&repo_root, src) {
            crate::design_onboarding::OnboardOutcome::Cancelled => {
                eprintln!("  \u{2717} --design import cancelled; aborting.");
                return 1;
            }
            _ => {}
        }
    }
    if !design_md_path.exists() {
        eprintln!();
        eprintln!("  \u{2717} No DESIGN.md at repo root.");
        eprintln!(
            "    Run `sovereign project design` first (agent-collaborative) or\n\
             `sovereign project design --solo` (CLI prompts)."
        );
        return 2;
    }
    let design_text = match std::fs::read_to_string(&design_md_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  \u{2717} cannot read DESIGN.md: {e}");
            return 1;
        }
    };
    eprintln!(
        "    \u{2713} DESIGN.md ({} line(s), sha=`{}`)",
        design_text.lines().count(),
        {
            let h = crate::found::hash_charter(&design_text);
            h[..h.len().min(12)].to_string()
        }
    );

    // Gate 2: OPEN_QUESTIONS.md — all blocking questions must be
    // answered. Orchestrator inherits the same contract as
    // `project plan`: unanswered gaps are load-bearing, don't
    // paper over them.
    let oq_path = repo_root.join("OPEN_QUESTIONS.md");
    let oq_text = std::fs::read_to_string(&oq_path).unwrap_or_default();
    let oqs = crate::plan_composer::parse_open_questions(&oq_text);
    let unanswered: Vec<_> = oqs.iter().filter(|o| !o.is_answered()).collect();
    if !unanswered.is_empty() {
        eprintln!();
        eprintln!(
            "  \u{26a0} {} unanswered OPEN_QUESTIONS.md entry(s):",
            unanswered.len()
        );
        for oq in &unanswered {
            eprintln!("    · {} ({})", oq.id, oq.anchor);
        }
        eprintln!();
        eprintln!(
            "    Founding requires these resolved — edit OPEN_QUESTIONS.md inline,\n\
             then re-run `sovereign project plan` before founding again."
        );
        return 2;
    }
    eprintln!(
        "    \u{2713} OPEN_QUESTIONS.md ({} answered / 0 open)",
        oqs.len()
    );

    // Gate 3: IMPLEMENTATION_PLAN.md.
    let plan_md_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_md_path.exists() {
        eprintln!();
        eprintln!("  \u{2717} No IMPLEMENTATION_PLAN.md at repo root.");
        eprintln!("    Run `sovereign project plan` first.");
        return 2;
    }
    let plan_text = std::fs::read_to_string(&plan_md_path).unwrap_or_default();
    let plan_hash = crate::found::hash_charter(&plan_text);
    eprintln!(
        "    \u{2713} IMPLEMENTATION_PLAN.md (sha=`{}`)",
        &plan_hash[..plan_hash.len().min(12)]
    );

    // Gate 4: CHARTER.md. Distinct from the questionnaire path —
    // CHARTER.md is the user-authored culture doc, NOT auto-composed.
    let charter_path = crate::amend::charter_path(&repo_root);
    if !charter_path.exists() {
        eprintln!();
        eprintln!("  \u{2717} No CHARTER.md at {}.", charter_path.display());
        eprintln!("    Run `sovereign project charter` first to author it.");
        return 2;
    }
    let charter_text = match std::fs::read_to_string(&charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  \u{2717} cannot read CHARTER.md: {e}");
            return 1;
        }
    };
    let charter_hash = crate::found::hash_charter(&charter_text);
    eprintln!(
        "    \u{2713} CHARTER.md (sha=`{}`)",
        &charter_hash[..charter_hash.len().min(12)]
    );

    // Gate 5: the one question `found` legitimately still needs
    // from the human — the Phase-1 stop condition. Plan items are
    // derived from DESIGN.md sections; Phase-1's stop is the
    // concrete "when is feature-foundation done?" line that only
    // the human knows.
    eprintln!();
    eprintln!("  Phase-1 stop condition — what proves the first real end-to-end path works?");
    eprintln!("  Examples: `cargo test --test e2e_ingest`, `curl localhost:8080/health returns 200`.");
    eprint!("    > ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let phase1_stop = crate::found::stdin_read_line();
    let phase1_stop = phase1_stop.trim().to_string();
    if phase1_stop.is_empty() {
        eprintln!();
        eprintln!("  \u{2717} A Phase-1 stop condition is required; aborting.");
        return 2;
    }

    // Compose PHASES.md. Reuses the existing compose_phases for
    // back-compat with `project phase pass N` (which parses PHASES.md
    // today). Phase-0 stop comes from observation; Phase-1 from the
    // user; Phase-2 degraded condition inferred from observation.
    let observation = crate::observation::observe(&repo_root);
    let today = today_iso();
    let project_id = derive_project_id(&repo_root);
    let phases_md = crate::found::compose_phases(&crate::found::FoundingInputs {
        project_id: &project_id,
        founded_date: &today,
        design: Some(&design_text),
        observation: &observation,
        stage1_answers: &[],
        stage2_outcomes: &[],
        phase1_stop_condition: &phase1_stop,
    });

    let phases_path = sovereign_dir.join("PHASES.md");
    if let Err(e) = std::fs::write(&phases_path, &phases_md) {
        eprintln!("  \u{2717} could not write PHASES.md: {e}");
        return 1;
    }

    // Flip the lifecycle.
    project_toml.lifecycle.founded = true;
    project_toml.lifecycle.charter_version = 1;
    project_toml.lifecycle.current_phase = 0;
    project_toml.lifecycle.charter_hash = charter_hash.clone();
    if let Err(e) = project_toml.write(&project_toml_path) {
        eprintln!("  \u{2717} could not persist project.toml: {e}");
        return 1;
    }

    eprintln!();
    eprintln!(
        "  \u{2713} Founded. charter_version=1, current_phase=0, charter_hash=`{}`.",
        &charter_hash[..charter_hash.len().min(12)]
    );
    eprintln!("    Artifacts at repo root: DESIGN.md, OPEN_QUESTIONS.md, IMPLEMENTATION_PLAN.md, CHARTER.md.");
    eprintln!("    PHASES.md written at {}.", phases_path.display());
    eprintln!("    Next: `sovereign project phase pass 0` when Phase 0's stop condition is green.");
    0
}

async fn cmd_found(args: &[String]) -> i32 {
    let mut design_path: Option<PathBuf> = None;
    let mut orchestrate = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--design" => {
                i += 1;
                design_path = args.get(i).map(PathBuf::from);
            }
            "--orchestrate" => orchestrate = true,
            "--help" | "-h" => {
                println!("sovereign project found [--design <path>] [--orchestrate]");
                println!();
                println!("Once per project: the founding conversation that produces");
                println!("CHARTER.md + PHASES.md.");
                println!();
                println!("  (default)      Classic Stage 1 + Stage 2 questionnaire; auto-composes");
                println!("                 CHARTER.md from answers. Stage-1/2 questions are");
                println!("                 signal-gated against DESIGN.md when present.");
                println!("  --orchestrate  Orchestrator mode: require DESIGN.md + CHARTER.md +");
                println!("                 IMPLEMENTATION_PLAN.md at repo root (run the respective");
                println!("                 subcommands first), then flip the lifecycle and compose");
                println!("                 PHASES.md from plan_items. Skips the questionnaire — the");
                println!("                 design session + plan composer already captured what we'd");
                println!("                 have asked. Elicits only the Phase-1 stop condition.");
                return 0;
            }
            flag if flag.starts_with("--") => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
        i += 1;
    }
    if orchestrate {
        return cmd_found_orchestrate(design_path.as_deref()).await;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let sovereign_dir = repo_root.join(".sovereign");
    let project_toml_path = sovereign_dir.join("project.toml");

    // Gate 1: project.toml must exist — `project init` is the
    // prerequisite. A helpful message, not an opaque "file not found."
    if !project_toml_path.exists() {
        eprintln!();
        eprintln!(
            "  sovereign project found: no .sovereign/project.toml found.\n\
             \n\
             Run `sovereign project init` first so we have an observation\n\
             of the project to anchor the founding conversation to."
        );
        return 1;
    }

    let mut project_toml = match crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  sovereign project found: cannot read project.toml: {e}");
            return 1;
        }
    };

    // Gate 2: once-per-project. Amendments go through
    // `sovereign project amend` (M6.7).
    if project_toml.lifecycle.founded {
        eprintln!();
        eprintln!(
            "  sovereign project found: this project was already founded\n\
             (charter_version={}, current_phase={}).\n\
             \n\
             To revise the charter, use `sovereign project amend` (M6.7).",
            project_toml.lifecycle.charter_version, project_toml.lifecycle.current_phase,
        );
        return 1;
    }

    // Re-observe: project.toml has a snapshot but it may be stale.
    // Founding is a small moment; pay the <200ms cost for a fresh
    // read rather than trust a potentially-days-old observation.
    let observation = crate::observation::observe(&repo_root);

    // Design doc handling.
    let design_text: Option<String> = match design_path.as_deref() {
        Some(p) => match crate::found::load_design(p) {
            Some(text) => {
                println!();
                println!("  Using design document: {}", p.display());
                let preview = crate::found::design_preview(&text);
                if !preview.is_empty() {
                    println!("    \u{2192} {}", preview);
                }
                Some(text)
            }
            None => {
                eprintln!(
                    "  sovereign project found: could not read --design {} — falling back to elicitation.",
                    p.display()
                );
                None
            }
        },
        None => None,
    };

    println!();
    println!("  Sovereign Project Founding — Stage 1: Understanding");
    println!("  {}", "─".repeat(54));
    println!();
    println!(
        "  A few questions — each one is material (it changes what ends up in the charter)."
    );
    println!("  Hit Enter to skip any question; you can amend later.");

    // Persistence: open the project's NoteStore under the
    // canonical .sovereign/notes.db path.
    let notes_path = sovereign_dir.join("notes.db");
    let note_store = match corpus_engine::NoteStore::open(&notes_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  sovereign project found: cannot open notes.db: {e}");
            return 1;
        }
    };
    let session_id = format!("found-{}", unix_now_secs());

    // Handle is needed so the blocking recorder can reuse the
    // current multi-thread runtime for its writes.
    let rt_handle = tokio::runtime::Handle::current();
    let mut recorder = crate::found::NoteStoreDecisionWriter {
        store: &note_store,
        session_id: &session_id,
        written: Vec::new(),
        rt: rt_handle,
    };
    let mut interlocutor = crate::found::StdinFoundInterlocutor::new();

    // Signals are extracted from the design doc when one is present.
    // Step-3 wiring: we compute them once and thread them into both
    // Stage 1 (catalog predicates) and Stage 2 (fault-line
    // predicates). Step 4's `cmd_design` flow will likely have
    // already computed these; recomputing here is cheap (O(n) over
    // the doc) and keeps this code path self-contained.
    let design_signals: Option<corpus_engine::design_signals::DesignSignals> =
        design_text.as_deref().map(corpus_engine::design_signals::extract);

    let answers = crate::found::run_stage1(
        &observation,
        design_text.as_deref(),
        design_signals.as_ref(),
        &mut interlocutor,
        &mut recorder,
    );

    // Stage 1 summary.
    let recorded_s1 = recorder.written.len();
    let answered: usize = answers.iter().filter(|a| !a.skipped).count();
    let skipped: usize = answers.iter().filter(|a| a.skipped).count();
    println!();
    println!("  Stage 1 complete.");
    println!(
        "    {recorded_s1} decision note{} written ({answered} answered, {skipped} skipped).",
        if recorded_s1 == 1 { "" } else { "s" },
    );

    // Stage 2 — Fault lines. The stage-1 answers become input
    // signals for fault-line selection.
    let rt_handle_2 = tokio::runtime::Handle::current();
    let mut fault_recorder = crate::found::NoteStoreFaultLineWriter {
        store: &note_store,
        session_id: &session_id,
        written: Vec::new(),
        outcomes: Vec::new(),
        rt: rt_handle_2,
    };
    let mut fault_interlocutor = crate::found::StdinFaultLineInterlocutor::new();
    let selected_faults =
        crate::found::select_fault_lines(&observation, &answers, design_signals.as_ref());
    let stage2_summary = if selected_faults.is_empty() {
        println!();
        println!("  Sovereign Project Founding — Stage 2: Fault lines");
        println!("  {}", "─".repeat(54));
        println!();
        println!("  No fault lines fired for this project's shape — moving on.");
        crate::found::Stage2Summary::default()
    } else {
        println!();
        println!("  Sovereign Project Founding — Stage 2: Fault lines");
        println!("  {}", "─".repeat(54));
        println!();
        println!(
            "  {} genuine disagreement{} in your domain. These aren't recommendations —",
            selected_faults.len(),
            if selected_faults.len() == 1 { "" } else { "s" }
        );
        println!("  reasonable people pick different sides. Decide now, leave open, or skip.");
        crate::found::run_stage2(
            &observation,
            &answers,
            design_signals.as_ref(),
            &mut fault_interlocutor,
            &mut fault_recorder,
        )
    };

    println!();
    println!("  Stage 2 complete.");
    println!(
        "    {} resolved → decision notes, {} open → uncertainty notes, {} skipped (no note).",
        stage2_summary.resolved, stage2_summary.open, stage2_summary.skipped,
    );
    println!(
        "    Read them back with:  sovereign read-notes --session {session_id}",
    );

    // Persist the observation refresh before we head into the
    // write-on-approval stages — if the user cancels in Stage 3/4,
    // we still want the observation update to stick.
    project_toml.update_observation(&observation, &project_toml_path);
    if let Err(e) = project_toml.write(&project_toml_path) {
        eprintln!("    \u{2717} Could not persist observation refresh: {e}");
    }

    // ── Stage 2.5 — Documentation URLs (M6.6) ───────────────────
    //
    // One question, asked once. Drop URLs → fetch + index into
    // ProjectDocsStore. Empty answer → decision note explaining
    // the runtime fallback. Either way, the user isn't asked this
    // again inside a founding session.
    let docs_prompt = crate::found::render_docs_prompt(&observation);
    let mut docs_interlocutor = crate::found::StdinDocsInterlocutor::new();
    let raw_urls = crate::found::DocsInterlocutor::ask_docs_urls(
        &mut docs_interlocutor,
        &docs_prompt,
    );
    let urls = crate::doc_fetcher::parse_urls(&raw_urls);

    if !urls.is_empty() {
        // Open ProjectDocsStore alongside the daemon's canonical
        // path (.sovereign/project_docs.db). init already created
        // .sovereign/ when it wrote project.toml.
        let docs_db = sovereign_dir.join("project_docs.db");
        match corpus_engine::ProjectDocsStore::open(&docs_db) {
            Ok(store) => {
                let rt = tokio::runtime::Handle::current();
                let fetcher = crate::doc_fetcher::ProjectDocsFetcher {
                    http: crate::doc_fetcher::reqwest_http(rt.clone()),
                    store: &store,
                    repo_root: repo_root.clone(),
                    rt,
                };
                println!();
                println!("  Fetching {} documentation URL{}…", urls.len(), if urls.len() == 1 { "" } else { "s" });
                let summaries = fetcher.fetch_many(&urls);
                let mut ok_count = 0usize;
                let mut err_count = 0usize;
                for s in &summaries {
                    match &s.outcome {
                        crate::honesty::FetchOutcome::Ok { bytes_indexed } => {
                            ok_count += 1;
                            println!(
                                "    \u{2713} {} ({} chunk{})",
                                s.url,
                                bytes_indexed,
                                if *bytes_indexed == 1 { "" } else { "s" }
                            );
                        }
                        crate::honesty::FetchOutcome::Err(e) => {
                            err_count += 1;
                            println!("    \u{2717} {}  ({e})", s.url);
                        }
                    }
                }
                println!(
                    "    → {} indexed, {} failed. Re-fetch failures later by editing\n      `.sovereign/docs/` and re-running `sovereign project refresh`.",
                    ok_count, err_count
                );
            }
            Err(e) => {
                eprintln!("    \u{2717} Could not open project_docs.db: {e}");
                eprintln!("    URLs recorded but not indexed. You can retry later.");
            }
        }
    } else if !raw_urls.trim().is_empty() {
        println!();
        println!(
            "    No HTTP(S) URLs parsed from your answer. Nothing fetched; \
             runtime honesty prompts will surface gaps as they come up."
        );
    } else {
        println!();
        println!(
            "    No URLs provided — runtime honesty prompts will surface gaps \
             as they come up."
        );
    }

    // Durable record: Stage-2.5 decision note regardless of the
    // answer shape. Empty answer is itself a decision.
    let docs_body = crate::found::render_docs_decision_body(&docs_prompt, &urls);
    let rt_for_docs = tokio::runtime::Handle::current();
    let _ = tokio::task::block_in_place(|| {
        rt_for_docs.block_on(note_store.write_note_scoped(
            "decision",
            &docs_body,
            Vec::new(),
            Vec::new(),
            &session_id,
            corpus_engine::NoteScope::Global,
            None,
        ))
    });

    // ── Stages 3 + 4 — CHARTER.md + PHASES.md ───────────────────
    //
    // Collect the stage-2 outcomes for composition. The runner
    // already recorded them; we need the parallel (fault, outcome)
    // tuples here for rendering. We rebuild them by re-running the
    // selection over the observation+stage1 answers and re-asking
    // — but that'd be a second interactive pass. Instead, we track
    // outcomes from the stage 2 loop we just ran, via the
    // fault_recorder's side channel.
    //
    // The simplest correct path is to re-query the notes we just
    // wrote in stage 2, since `NoteStoreFaultLineWriter` persists
    // decision + uncertainty notes with stable id prefixes. But
    // that's a round-trip through SQLite just to re-derive
    // in-memory data. So here: we change the fault recorder to
    // ALSO retain the (fault, outcome) pairs and expose them.
    //
    // That change is minimal — see `NoteStoreFaultLineWriter.outcomes`.

    let mut approval_interlocutor = crate::found::StdinApprovalInterlocutor::new();

    let phase1_stop =
        crate::found::ApprovalInterlocutor::ask_phase1_stop(&mut approval_interlocutor);
    if phase1_stop.trim().is_empty() {
        eprintln!();
        eprintln!(
            "  sovereign project found: a Phase 1 stop condition is required.\n\
             Stages 1 + 2 decision notes are already recorded — re-run\n\
             `sovereign project found` when you have a concrete answer."
        );
        return 0; // cancel, not error — stage 1+2 work is preserved
    }

    let founded_date = today_iso();
    let project_id = derive_project_id(&repo_root);
    let stage2_pairs = fault_recorder.outcomes.clone();
    let founding_inputs = crate::found::FoundingInputs {
        project_id: &project_id,
        founded_date: &founded_date,
        observation: &observation,
        design: design_text.as_deref(),
        stage1_answers: &answers,
        stage2_outcomes: &stage2_pairs,
        phase1_stop_condition: &phase1_stop,
    };

    let charter = crate::found::compose_charter(&founding_inputs);
    let phases = crate::found::compose_phases(&founding_inputs);

    println!();
    println!("  Sovereign Project Founding — Stage 3 + 4: Charter + Phases");
    println!("  {}", "─".repeat(54));

    // Drafts live in .sovereign/.pending/ during the edit loop so
    // the user can $EDITOR them directly. Cleaned up after
    // approval; retained on cancel so the user can inspect.
    let pending_dir = sovereign_dir.join(".pending");
    if let Err(e) = std::fs::create_dir_all(&pending_dir) {
        eprintln!("    \u{2717} Could not create .pending directory: {e}");
        return 1;
    }

    let outcome = crate::found::run_stage34(
        charter,
        phases,
        &pending_dir,
        &mut approval_interlocutor,
    );

    match outcome {
        crate::found::FoundingApproval::Approved {
            charter: final_charter,
            phases: final_phases,
        } => {
            let charter_out = sovereign_dir.join("CHARTER.md");
            let phases_out = sovereign_dir.join("PHASES.md");
            if let Err(e) = std::fs::write(&charter_out, &final_charter) {
                eprintln!("    \u{2717} Could not write CHARTER.md: {e}");
                return 1;
            }
            if let Err(e) = std::fs::write(&phases_out, &final_phases) {
                eprintln!("    \u{2717} Could not write PHASES.md: {e}");
                return 1;
            }
            println!();
            println!("    \u{2713} {}", charter_out.display());
            println!("    \u{2713} {}", phases_out.display());

            // Flip lifecycle + record charter hash.
            project_toml.lifecycle.founded = true;
            project_toml.lifecycle.charter_version = 1;
            project_toml.lifecycle.current_phase = 0;
            project_toml.lifecycle.charter_hash = crate::found::hash_charter(&final_charter);
            if let Err(e) = project_toml.write(&project_toml_path) {
                eprintln!("    \u{2717} Could not flip lifecycle.founded in project.toml: {e}");
                return 1;
            }
            println!(
                "    \u{2713} project.toml: founded=true, charter_version=1, current_phase=0"
            );

            // Clean up the pending drafts — the canonical files are
            // the output now.
            let _ = std::fs::remove_file(pending_dir.join("CHARTER.md"));
            let _ = std::fs::remove_file(pending_dir.join("PHASES.md"));
            let _ = std::fs::remove_dir(&pending_dir);

            println!();
            println!("  Project founded. Phase 0 begins.");
            0
        }
        crate::found::FoundingApproval::Cancelled => {
            println!();
            println!("  Founding cancelled. Stage 1 + 2 decision notes are preserved;");
            println!(
                "  draft CHARTER.md + PHASES.md remain under {} for you to review.",
                pending_dir.display()
            );
            println!("  Re-run `sovereign project found` when you're ready to approve.");
            0
        }
    }
}

// ─── sovereign project phase (M7.1) ──────────────────────────
//
// Phase progression: parse PHASES.md, run or manually verify a
// phase's stop condition, write phase-N.md, advance
// lifecycle.current_phase, write a decision note. The artifact
// trail closes the "was Phase N actually verified?" gap.

pub(crate) async fn cmd_phase(args: &[String]) -> i32 {
    let Some(sub) = args.first().cloned() else {
        eprintln!("phase: missing subcommand (status | pass)");
        return 2;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "status" => cmd_phase_status(rest).await,
        "pass" => cmd_phase_pass(rest).await,
        "--help" | "-h" => {
            println!("sovereign project phase <status|pass [N]>");
            println!();
            println!("status       Show current phase and what's next per PHASES.md");
            println!("pass [N]     Run (or manually confirm) Phase N's stop condition,");
            println!("             write phase-N.md, advance lifecycle.current_phase.");
            println!("             Default N = current_phase + 1.");
            0
        }
        other => {
            eprintln!("phase: unknown subcommand '{other}'");
            2
        }
    }
}

pub(crate) async fn cmd_phase_status(_args: &[String]) -> i32 {
    let (repo_root, project_toml, _) = match load_phase_context() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let phases_path = crate::phases::phases_md_path(&repo_root);
    let md = match std::fs::read_to_string(&phases_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("phase status: cannot read PHASES.md: {e}");
            return 1;
        }
    };
    let phases = crate::phases::parse_phases(&md);
    println!();
    println!("  Phase progression");
    println!("  {}", "─".repeat(54));
    println!();
    println!("  current_phase = {}", project_toml.lifecycle.current_phase);
    println!();
    for p in &phases {
        let marker = if p.deferred {
            "⋯"
        } else if p.ordinal < project_toml.lifecycle.current_phase {
            "✓"
        } else if p.ordinal == project_toml.lifecycle.current_phase {
            "▶"
        } else {
            " "
        };
        println!("  {marker} {}", p.heading);
        if !p.stop_text.is_empty() {
            println!("      stop: {}", p.stop_text);
        }
    }
    println!();
    let next = phases
        .iter()
        .find(|p| !p.deferred && p.ordinal > project_toml.lifecycle.current_phase);
    match next {
        Some(p) => println!(
            "  Next: `sovereign project phase pass {}`",
            p.ordinal
        ),
        None => println!("  All numbered phases complete."),
    }
    0
}

pub(crate) async fn cmd_phase_pass(args: &[String]) -> i32 {
    let (repo_root, mut project_toml, project_toml_path) = match load_phase_context() {
        Ok(c) => c,
        Err(code) => return code,
    };

    let phases_path = crate::phases::phases_md_path(&repo_root);
    let md = match std::fs::read_to_string(&phases_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("phase pass: cannot read PHASES.md: {e}");
            return 1;
        }
    };
    let phases = crate::phases::parse_phases(&md);

    // Resolve which phase to pass. Positional arg OR default to
    // current_phase + 1 (since `found` seeds current_phase = 0,
    // the first pass targets Phase 0 only once — after that we
    // advance linearly).
    let target_ordinal: u32 = if let Some(s) = args.first() {
        match s.parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("phase pass: ordinal must be an integer, got '{s}'");
                return 2;
            }
        }
    } else if project_toml.lifecycle.current_phase == 0 && !already_passed(&repo_root, 0) {
        // Haven't passed phase 0 yet.
        0
    } else {
        project_toml.lifecycle.current_phase + 1
    };

    let phase = match phases.iter().find(|p| !p.deferred && p.ordinal == target_ordinal) {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "phase pass: no numbered Phase {target_ordinal} in PHASES.md. \
                 Deferred phases (3+) aren't passed via this command — add them \
                 via `sovereign project amend` first, or use a different ordinal."
            );
            return 1;
        }
    };

    // Decide execution mode. Heuristic guard: the naive "single
    // backticked block" parse mis-extracts a quoted dep name
    // (`reqwest`) from prose. Treat any extracted "command" that
    // doesn't contain a space OR a shell operator as suspicious
    // and drop to manual confirmation.
    let executable = phase.stop_command.as_ref().filter(|c| looks_shell_runnable(c));

    println!();
    println!("  Phase pass: {}", phase.heading);
    println!("  {}", "─".repeat(54));
    println!();
    if phase.stop_text.is_empty() {
        println!("  No stop condition recorded for this phase.");
    } else {
        println!("  Stop condition: {}", phase.stop_text);
    }
    println!();

    let outcome = match executable {
        Some(cmd) => {
            println!("  Running: {cmd}");
            let out = crate::phases::run_stop_command(cmd);
            println!(
                "  {} exit={} duration={}ms",
                if out.passed { "\u{2713} PASSED" } else { "\u{2717} FAILED" },
                out.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
                out.duration_ms
            );
            out
        }
        None => {
            // Manual confirmation path.
            println!(
                "  No single unambiguous shell command extracted from the stop text."
            );
            println!("  Manual verification — run the stop condition yourself, then answer here.");
            print!("  Did it pass? [y/N] ");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let answer = crate::found::stdin_read_line().to_lowercase();
            let passed = matches!(answer.chars().next(), Some('y'));
            crate::phases::PhasePassOutcome {
                passed,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: 0,
                verification: crate::phases::Verification::ManualConfirm,
            }
        }
    };

    let date = today_iso();
    let committer = git_committer_identity_for_amend(&repo_root)
        .unwrap_or_else(|| "<unknown committer>".to_string());
    let report = crate::phases::render_phase_report(&phase, &outcome, &date, &committer);
    let report_path = crate::phases::phase_report_path(&repo_root, phase.ordinal);
    if let Err(e) = std::fs::write(&report_path, &report) {
        eprintln!("phase pass: could not write {}: {e}", report_path.display());
        return 1;
    }
    println!("    \u{2713} {}", report_path.display());

    // Durable decision note. Captures enough for `audit` to build
    // a rollup without re-parsing the phase artifact.
    let sovereign_dir = repo_root.join(".sovereign");
    if let Ok(note_store) = corpus_engine::NoteStore::open(&sovereign_dir.join("notes.db")) {
        let verdict = if outcome.passed { "PASSED" } else { "FAILED" };
        let body = format!(
            "Phase {} · {}\n\nVerdict: {}\nVerification: {}\nDate: {}\nCommitter: {}\n\nStop condition:\n{}\n",
            phase.ordinal,
            phase.heading,
            verdict,
            match &outcome.verification {
                crate::phases::Verification::RanCommand { command } => {
                    format!("ran `{command}` (exit {})", outcome.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()))
                }
                crate::phases::Verification::ManualConfirm => "manually confirmed".into(),
            },
            date,
            committer,
            if phase.stop_text.is_empty() {
                "_(none)_"
            } else {
                phase.stop_text.as_str()
            },
        );
        let rt = tokio::runtime::Handle::current();
        let _ = tokio::task::block_in_place(|| {
            rt.block_on(note_store.write_note_scoped(
                "decision",
                &body,
                Vec::new(),
                Vec::new(),
                &format!("phase-{}-pass", phase.ordinal),
                corpus_engine::NoteScope::Global,
                None,
            ))
        });
    }

    // Advance current_phase ONLY when the run passed. Failing a
    // phase is a recorded artifact (phase-N.md exists) but
    // doesn't advance the counter — the operator retries.
    if outcome.passed {
        if phase.ordinal >= project_toml.lifecycle.current_phase {
            project_toml.lifecycle.current_phase = phase.ordinal + 1;
            if let Err(e) = project_toml.write(&project_toml_path) {
                eprintln!("phase pass: could not update project.toml: {e}");
                return 1;
            }
            println!(
                "    \u{2713} project.toml: current_phase = {}",
                project_toml.lifecycle.current_phase
            );
        }
        0
    } else {
        println!();
        println!("  Phase {} FAILED — current_phase unchanged.", phase.ordinal);
        println!("  Read `{}` for the captured output.", report_path.display());
        1
    }
}

/// Load the usual project triple: repo root, parsed project.toml,
/// project.toml path. Returns a reusable `Err(i32)` exit code
/// when any precondition fails.
fn load_phase_context(
) -> Result<(PathBuf, crate::project_toml::ProjectTomlFile, PathBuf), i32> {
    let repo_root = find_repo_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let project_toml_path = repo_root.join(".sovereign").join("project.toml");
    if !project_toml_path.exists() {
        eprintln!();
        eprintln!(
            "  sovereign project phase: no .sovereign/project.toml found.\n\
             Run `sovereign project init` then `sovereign project found` first."
        );
        return Err(1);
    }
    let project_toml = match crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  sovereign project phase: cannot read project.toml: {e}");
            return Err(1);
        }
    };
    if !project_toml.lifecycle.founded {
        eprintln!();
        eprintln!(
            "  sovereign project phase: this project hasn't been founded yet.\n\
             Run `sovereign project found` first — PHASES.md is produced at founding."
        );
        return Err(1);
    }
    Ok((repo_root, project_toml, project_toml_path))
}

/// Heuristic: does this extracted "command" look like a shell
/// invocation, or is it likely a quoted identifier from prose?
/// A real command usually has a space (for args), a shell
/// operator (`&&`, `||`, `;`, `|`, `>`, `$`), or a path hint.
fn looks_shell_runnable(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    t.contains(' ')
        || t.contains("&&")
        || t.contains("||")
        || t.contains(';')
        || t.contains('|')
        || t.contains('>')
        || t.contains('$')
        || t.starts_with("./")
        || t.starts_with('/')
        || t.starts_with("cargo ")
        || t.starts_with("npm ")
        || t.starts_with("go ")
        || t.starts_with("python ")
        || t.starts_with("pytest")
        || t.starts_with("make")
}

fn already_passed(repo_root: &Path, ordinal: u32) -> bool {
    crate::phases::phase_report_path(repo_root, ordinal).exists()
}

// ─── sovereign project audit (M7.3) ──────────────────────────
//
// Reviewer-ready rollup. Read-only scan of everything a reviewer
// would otherwise have to assemble by hand: the lifecycle state,
// phase progression, notes grouped by kind, drift status, feature
// summary, red-team findings. One markdown page printed to stdout
// so it can be piped to a file, a PR description, or a GitHub
// issue.

pub(crate) async fn cmd_audit(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("sovereign project audit");
        println!();
        println!("Prints a reviewer-ready rollup of project state to stdout.");
        println!("Pipe it to a file or a PR description: `sovereign project audit > audit.md`.");
        return 0;
    }
    let repo_root = find_repo_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let sov = repo_root.join(".sovereign");
    let project_toml_path = sov.join("project.toml");
    if !project_toml_path.exists() {
        eprintln!(
            "  sovereign project audit: no .sovereign/project.toml found. \
             Run `sovereign project init` first."
        );
        return 1;
    }
    let project_toml = match crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  sovereign project audit: cannot read project.toml: {e}");
            return 1;
        }
    };

    let report = build_audit_report(&repo_root, &project_toml).await;
    println!("{report}");
    0
}

/// Compose the audit markdown. Pure with respect to the filesystem
/// + DBs (reads only, no writes) so tests can drive it with a
/// seeded repo and assert the output directly.
async fn build_audit_report(
    repo_root: &Path,
    project_toml: &crate::project_toml::ProjectTomlFile,
) -> String {
    let sov = repo_root.join(".sovereign");
    let project_id = derive_project_id(repo_root);
    let now = today_iso();

    let mut out = String::new();
    out.push_str(&format!("# {project_id} — Project audit\n\n"));
    out.push_str(&format!("_Generated: {now}._\n\n"));

    // ── Lifecycle ──────────────────────────────────────────────
    out.push_str("## Lifecycle\n\n");
    out.push_str(&format!(
        "- **Founded:** {}\n- **Charter version:** {}\n- **Current phase:** {}\n",
        if project_toml.lifecycle.founded { "yes" } else { "no" },
        project_toml.lifecycle.charter_version,
        project_toml.lifecycle.current_phase,
    ));
    // Charter drift detection — recompute vs. recorded hash.
    let charter_path = sov.join("CHARTER.md");
    let drift_line = if charter_path.exists() && !project_toml.lifecycle.charter_hash.is_empty() {
        match std::fs::read_to_string(&charter_path) {
            Ok(text) => {
                let current = crate::found::hash_charter(&text);
                if current == project_toml.lifecycle.charter_hash {
                    "- **Charter drift:** none (on-disk CHARTER.md matches recorded hash)".into()
                } else {
                    format!(
                        "- **Charter drift:** ⚠ on-disk hash `{}` differs from recorded `{}`",
                        &current[..8.min(current.len())],
                        &project_toml.lifecycle.charter_hash
                            [..8.min(project_toml.lifecycle.charter_hash.len())]
                    )
                }
            }
            Err(_) => "- **Charter drift:** unknown (CHARTER.md unreadable)".into(),
        }
    } else if !charter_path.exists() {
        "- **Charter drift:** n/a (no CHARTER.md — project not founded)".into()
    } else {
        "- **Charter drift:** n/a (no recorded hash)".into()
    };
    out.push_str(&drift_line);
    out.push_str("\n\n");

    // ── Phases ─────────────────────────────────────────────────
    let phases_path = crate::phases::phases_md_path(repo_root);
    out.push_str("## Phases\n\n");
    if phases_path.exists() {
        let md = std::fs::read_to_string(&phases_path).unwrap_or_default();
        let phases = crate::phases::parse_phases(&md);
        if phases.is_empty() {
            out.push_str("_(PHASES.md exists but no phases parsed.)_\n\n");
        } else {
            out.push_str("| Phase | Status | Stop condition |\n|---|---|---|\n");
            for p in &phases {
                let status = if p.deferred {
                    "deferred".into()
                } else if p.ordinal < project_toml.lifecycle.current_phase {
                    let artifact = crate::phases::phase_report_path(repo_root, p.ordinal);
                    let verdict = read_phase_verdict(&artifact).unwrap_or("passed".into());
                    format!("{verdict} → `{}`", relative(&artifact, repo_root))
                } else if p.ordinal == project_toml.lifecycle.current_phase {
                    "current".into()
                } else {
                    "not yet".into()
                };
                let stop = if p.stop_text.is_empty() {
                    "_(none)_".into()
                } else {
                    p.stop_text.replace('|', "\\|")
                };
                out.push_str(&format!("| {} | {status} | {stop} |\n", p.heading));
            }
            out.push('\n');
        }
    } else {
        out.push_str("_(no PHASES.md — project not founded)_\n\n");
    }

    // ── Notes by kind ──────────────────────────────────────────
    out.push_str("## Notes by kind\n\n");
    let notes_db = sov.join("notes.db");
    let (note_counts, recent_open_questions, recent_deviations) = if notes_db.exists() {
        match corpus_engine::NoteStore::open(&notes_db) {
            Ok(store) => gather_note_summary(&store).await,
            Err(_) => (Default::default(), Vec::new(), Vec::new()),
        }
    } else {
        (Default::default(), Vec::new(), Vec::new())
    };
    if note_counts.is_empty() {
        out.push_str("_(no notes recorded)_\n\n");
    } else {
        out.push_str("| Kind | Count |\n|---|---|\n");
        for (kind, count) in &note_counts {
            out.push_str(&format!("| {kind} | {count} |\n"));
        }
        out.push('\n');
    }

    if !recent_open_questions.is_empty() {
        out.push_str("### Open questions\n\n");
        for (id, first_line) in &recent_open_questions {
            out.push_str(&format!("- `[note:{id}]` {first_line}\n"));
        }
        out.push('\n');
    }
    if !recent_deviations.is_empty() {
        out.push_str("### Deviations (accepted drift + spec changes)\n\n");
        for (id, first_line) in &recent_deviations {
            out.push_str(&format!("- `[note:{id}]` {first_line}\n"));
        }
        out.push('\n');
    }

    // ── Features ───────────────────────────────────────────────
    out.push_str("## Features\n\n");
    let features_db = sov.join("features.db");
    if features_db.exists() {
        match corpus_engine::FeatureStore::open(&features_db) {
            Ok(store) => match store.list(true).await {
                Ok(features) if !features.is_empty() => {
                    out.push_str("| Feature | State | Auto red-team |\n|---|---|---|\n");
                    for f in &features {
                        out.push_str(&format!(
                            "| {} | {} | {} |\n",
                            f.id,
                            f.state,
                            if f.auto_redteam { "yes" } else { "no" }
                        ));
                    }
                    out.push('\n');
                }
                _ => {
                    out.push_str("_(no features provisioned yet)_\n\n");
                }
            },
            Err(_) => {
                out.push_str("_(features.db unreadable)_\n\n");
            }
        }
    } else {
        out.push_str("_(no features.db yet)_\n\n");
    }

    // ── Artifact inventory ─────────────────────────────────────
    out.push_str("## Artifact inventory\n\n");
    let artifacts = collect_artifact_inventory(&sov);
    if artifacts.is_empty() {
        out.push_str("_(no ATOS artifacts found)_\n\n");
    } else {
        for a in &artifacts {
            out.push_str(&format!("- `{a}`\n"));
        }
        out.push('\n');
    }

    // ── Footer ────────────────────────────────────────────────
    out.push_str("---\n\n");
    out.push_str(
        "_Generated by `sovereign project audit`. Re-run to refresh; this document is not committed automatically._\n",
    );

    // ── Publish-recipe nudge (Phase 7) ─────────────────────────
    // Fires once when a user-authored recipe has been driven to
    // findings via `sovereign enrich investigation build`. The
    // condition gate suppresses the nudge for already-published
    // and explicitly-dismissed entries; it costs ~one filesystem
    // read per investigation corpus and never fires for recipes
    // sourced from the upstream registry.
    if let Some(nudge) = compose_publish_recipe_nudge() {
        out.push('\n');
        out.push_str(&nudge);
    }
    out
}

/// Compose a publish-recipe nudge if any investigation pipeline has
/// produced findings on a user-authored (local) recipe that hasn't
/// been published yet AND hasn't been explicitly dismissed.
/// Returns `None` when nothing to nudge about.
///
/// The nudge text is formatted as a horizontal-rule-delimited
/// blockquote so it survives copy-paste into a PR description or
/// gets ignored when piped to a tool that strips trailing
/// whitespace.
fn compose_publish_recipe_nudge() -> Option<String> {
    use corpus_engine::RecipeRegistry;

    // Resolve the local recipes dir + the indexes dir. Bail
    // silently when HOME isn't set — the nudge is best-effort.
    let local_recipes_dir = RecipeRegistry::default_local_recipes_dir()?;
    let indexes_dir = crate::util::dirs::sovereign_indexes();
    if !indexes_dir.is_dir() {
        return None;
    }

    // Build the registry once so `is_local_entry` is cheap to call.
    let mut registry = RecipeRegistry::from_bundled(Some(local_recipes_dir.clone()));
    registry =
        registry.with_local_registry(&local_recipes_dir.join("registry.toml"));

    // Read the publish + dismissal markers.
    let sovereign_root = crate::util::dirs::sovereign_root();
    let published: std::collections::BTreeMap<String, serde_json::Value> =
        std::fs::read_to_string(sovereign_root.join("published_recipes.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    let dismissed: Vec<String> =
        std::fs::read_to_string(sovereign_root.join("dismissed_nudges.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    if dismissed.iter().any(|d| d == "recipe-publish") {
        return None;
    }

    // Walk every installed corpus and look for an
    // `investigation/pattern_findings.json` with at least one row.
    let mut candidates: Vec<(String, usize)> = Vec::new();
    let entries = std::fs::read_dir(&indexes_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let corpus_id = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let findings_path =
            path.join("investigation").join("pattern_findings.json");
        if !findings_path.is_file() {
            continue;
        }
        let raw = match std::fs::read_to_string(&findings_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let arr: Vec<serde_json::Value> =
            match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
        if arr.is_empty() {
            continue;
        }
        // Filters: local recipe, not published, not dismissed-by-id.
        if !registry.is_local_entry(&corpus_id) {
            continue;
        }
        if published.contains_key(&corpus_id) {
            continue;
        }
        if dismissed
            .iter()
            .any(|d| d == &format!("recipe-publish:{corpus_id}"))
        {
            continue;
        }
        candidates.push((corpus_id, arr.len()));
    }

    if candidates.is_empty() {
        return None;
    }

    let mut nudge = String::new();
    nudge.push_str("\n---\n\n");
    nudge.push_str("## 💡 Share your recipe\n\n");
    if candidates.len() == 1 {
        let (id, n) = &candidates[0];
        nudge.push_str(&format!(
            "Your locally-authored recipe `{id}` produced {n} pattern finding(s) \
             during this audit. Publishing it would let others run the same \
             investigation without rebuilding the recipe from scratch.\n\n"
        ));
        nudge.push_str(&format!(
            "  sovereign recipe publish ~/.sovereign/recipes/{id}/recipe.toml\n\n"
        ));
    } else {
        nudge.push_str(
            "These locally-authored recipes produced findings during this audit. \
             Publishing each lets others run the same investigations.\n\n",
        );
        for (id, n) in &candidates {
            nudge.push_str(&format!("- `{id}` ({n} finding(s))\n"));
        }
        nudge.push('\n');
        nudge.push_str(
            "  sovereign recipe publish ~/.sovereign/recipes/<id>/recipe.toml\n\n",
        );
    }
    nudge.push_str(
        "_Shown once. Dismiss forever: `sovereign nudge dismiss recipe-publish` \
         — or per-recipe: `sovereign nudge dismiss recipe-publish:<id>`._\n",
    );
    Some(nudge)
}

/// Scan a phase-N.md artifact for its verdict line. Defensive —
/// if the header line doesn't match the expected shape, returns
/// `None` and the caller falls back to a generic label.
fn read_phase_verdict(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let first = text.lines().next()?;
    if first.contains("PASSED") {
        Some("✓ passed".into())
    } else if first.contains("FAILED") {
        Some("✗ failed".into())
    } else {
        None
    }
}

/// Count notes by kind and pull up to 10 open-question + 10
/// deviation notes. Uses the broad Global + Feature scope
/// combined — auditor wants the whole picture.
async fn gather_note_summary(
    store: &corpus_engine::NoteStore,
) -> (
    std::collections::BTreeMap<String, u32>,
    Vec<(String, String)>,
    Vec<(String, String)>,
) {
    let filter = corpus_engine::ScopeFilter {
        scopes: vec![
            corpus_engine::NoteScope::Global,
            corpus_engine::NoteScope::Feature,
        ],
        feature_id: None,
    };
    let rows = store
        .read_notes_scoped(None, &[], &[], &[], 500, false, &filter)
        .await
        .unwrap_or_default();

    let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
    let mut opens = Vec::new();
    let mut devs = Vec::new();
    for n in rows {
        *counts.entry(n.kind.clone()).or_insert(0) += 1;
        let first_line = n
            .content
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        if n.kind == "uncertainty" && opens.len() < 10 {
            opens.push((n.id.clone(), first_line.clone()));
        }
        if n.kind == "deviation" && devs.len() < 10 {
            devs.push((n.id.clone(), first_line));
        }
    }
    (counts, opens, devs)
}

fn collect_artifact_inventory(sov: &Path) -> Vec<String> {
    let mut out = Vec::new();
    // Top-level markdown + toml artifacts.
    for name in ["CHARTER.md", "PHASES.md", "project.toml"] {
        if sov.join(name).exists() {
            out.push(format!(".sovereign/{name}"));
        }
    }
    // Phase reports.
    if let Ok(entries) = std::fs::read_dir(sov) {
        let mut phase_names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                (n.starts_with("phase-") && n.ends_with(".md")).then_some(n)
            })
            .collect();
        phase_names.sort();
        for n in phase_names {
            out.push(format!(".sovereign/{n}"));
        }
    }
    // Feature artifacts.
    let feats = sov.join("features");
    if feats.exists() {
        if let Ok(entries) = std::fs::read_dir(&feats) {
            let mut dirs: Vec<PathBuf> =
                entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            dirs.sort();
            for dir in dirs {
                let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let mut items = Vec::new();
                for f in ["spec.md", "red-team.md", "epistemic-report.md"] {
                    if dir.join(f).exists() {
                        items.push(f.to_string());
                    }
                }
                if let Ok(ents) = std::fs::read_dir(&dir) {
                    let mut ms: Vec<String> = ents
                        .filter_map(|e| e.ok())
                        .filter_map(|e| {
                            let n = e.file_name().to_string_lossy().into_owned();
                            (n.starts_with("milestone-") && n.ends_with(".md"))
                                .then_some(n)
                        })
                        .collect();
                    ms.sort();
                    items.extend(ms);
                }
                for i in items {
                    out.push(format!(".sovereign/features/{name}/{i}"));
                }
            }
        }
    }
    // Fetched docs.
    let docs = sov.join("docs");
    if docs.exists() {
        if let Ok(entries) = std::fs::read_dir(&docs) {
            let mut fnames: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            fnames.sort();
            for n in fnames {
                out.push(format!(".sovereign/docs/{n}"));
            }
        }
    }
    out
}

fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.display().to_string())
}

// ─── sovereign project amend (M6.7) ──────────────────────────

/// Post-founding charter edit flow with adversarial review. See
/// `crate::amend` for the policy; this function owns the I/O.
///
/// Flow:
/// 1. Refuse unless founded.
/// 2. Read current CHARTER.md; check hash against `charter_hash`.
///    If drifted, ask the user to fold the drift in (y/N).
/// 3. Spawn `$EDITOR` on CHARTER.md.
/// 4. Diff → adversarial Q&A → preview → approve.
/// 5. On approve: write CHARTER.md, bump `charter_version`,
///    update `charter_hash`, persist a decision note with the full
///    Q&A so readers six weeks later can find "why".
pub(crate) async fn cmd_amend(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("sovereign project amend [design|charter]");
        println!();
        println!("Edit with an adversarial review. Every amendment is logged —");
        println!("what changed, what arguments the system raised, and your");
        println!("responses.");
        println!();
        println!("  amend           (no target) — alias for `amend charter`");
        println!("  amend charter   — edit .sovereign/CHARTER.md; bumps charter_version");
        println!("  amend design    — edit DESIGN.md; appends `## Amendment log` entry");
        println!();
        println!("`amend charter` requires the project to be founded");
        println!("(`sovereign project found`); `amend design` works on any repo");
        println!("with a DESIGN.md at repo root.");
        return 0;
    }

    // Subcommand routing: `amend design` goes to the DESIGN.md flow;
    // bare `amend` and `amend charter` fall through to the existing
    // CHARTER.md flow (preserved verbatim — back-compat). The charter
    // flow doesn't parse flags further so we don't rebind args here.
    if matches!(args.first().map(String::as_str), Some("design")) {
        return cmd_amend_design(&args[1..]).await;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let sovereign_dir = repo_root.join(".sovereign");
    let project_toml_path = sovereign_dir.join("project.toml");
    if !project_toml_path.exists() {
        eprintln!();
        eprintln!(
            "  sovereign project amend: no .sovereign/project.toml found.\n\
             Run `sovereign project init` first, then `sovereign project found`."
        );
        return 1;
    }
    let mut project_toml = match crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  sovereign project amend: cannot read project.toml: {e}");
            return 1;
        }
    };
    if !project_toml.lifecycle.founded {
        eprintln!();
        eprintln!(
            "  sovereign project amend: this project hasn't been founded yet.\n\
             Run `sovereign project found` first — the charter it produces is \
             what `amend` edits."
        );
        return 1;
    }

    let charter_path = crate::amend::charter_path(&repo_root);
    let old_charter = match std::fs::read_to_string(&charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  sovereign project amend: cannot read CHARTER.md: {e}");
            return 1;
        }
    };

    // Drift detection.
    let disk_hash = crate::found::hash_charter(&old_charter);
    let mut interlocutor = crate::amend::StdinAmendmentInterlocutor::new();
    if disk_hash != project_toml.lifecycle.charter_hash {
        let hint = crate::amend::drift_summary(&project_toml.lifecycle.charter_hash, &old_charter);
        if !crate::amend::AmendmentInterlocutor::confirm_drift(&mut interlocutor, &hint) {
            eprintln!();
            eprintln!(
                "  Amendment cancelled. To discard the drift first:\n    git checkout -- {}\n  Then re-run `sovereign project amend`.",
                charter_path.display()
            );
            return 0;
        }
    }

    // Editor.
    println!();
    println!("  Opening CHARTER.md in your editor… save and exit when done.");
    let edited_charter = match crate::amend::invoke_editor(&charter_path) {
        Some(c) => c,
        None => {
            eprintln!("  Amendment cancelled (editor did not complete).");
            return 1;
        }
    };

    let committer = git_committer_identity_for_amend(&repo_root)
        .unwrap_or_else(|| "<unknown committer>".to_string());
    let next_version = project_toml.lifecycle.charter_version.saturating_add(1);
    let date = today_iso();

    let outcome = crate::amend::run_amend(
        &old_charter,
        &edited_charter,
        next_version,
        &date,
        &committer,
        &mut interlocutor,
    );

    match outcome {
        crate::amend::AmendOutcome::NoChange => {
            println!();
            println!("  No substantive changes detected — CHARTER.md left as-is.");
            0
        }
        crate::amend::AmendOutcome::Cancelled => {
            println!();
            println!(
                "  Amendment cancelled. Your editor changes are still in {} —\n\
                 re-open and re-run when ready, OR discard with `git checkout --`.",
                charter_path.display()
            );
            0
        }
        crate::amend::AmendOutcome::Approved {
            new_charter,
            entry,
        } => {
            if let Err(e) = std::fs::write(&charter_path, &new_charter) {
                eprintln!("  Could not write CHARTER.md: {e}");
                return 1;
            }
            project_toml.lifecycle.charter_version = entry.version;
            project_toml.lifecycle.charter_hash = entry.new_charter_hash.clone();
            if let Err(e) = project_toml.write(&project_toml_path) {
                eprintln!("  Could not update project.toml: {e}");
                return 1;
            }

            // Decision-kind note mirrors the amendment log entry so
            // `read_notes --kind decision` surfaces it without
            // parsing CHARTER.md.
            let notes_path = sovereign_dir.join("notes.db");
            if let Ok(note_store) = corpus_engine::NoteStore::open(&notes_path) {
                let body = crate::amend::render_amendment_note_body(&entry);
                let session_id = format!("amend-v{}", entry.version);
                let rt = tokio::runtime::Handle::current();
                let _ = tokio::task::block_in_place(|| {
                    rt.block_on(note_store.write_note_scoped(
                        "decision",
                        &body,
                        Vec::new(),
                        Vec::new(),
                        &session_id,
                        corpus_engine::NoteScope::Global,
                        None,
                    ))
                });
            }

            println!();
            println!("    \u{2713} CHARTER.md updated");
            println!(
                "    \u{2713} project.toml: charter_version={}, charter_hash={}",
                entry.version,
                &entry.new_charter_hash[..8.min(entry.new_charter_hash.len())]
            );
            println!("    \u{2713} decision note written (session=amend-v{})", entry.version);
            0
        }
    }
}

fn git_committer_identity_for_amend(repo_root: &Path) -> Option<String> {
    let name = std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    let email = std::process::Command::new("git")
        .args(["config", "user.email"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !name.status.success() || !email.status.success() {
        return None;
    }
    Some(format!(
        "{} <{}>",
        String::from_utf8_lossy(&name.stdout).trim(),
        String::from_utf8_lossy(&email.stdout).trim(),
    ))
}

fn today_iso() -> String {
    let secs = unix_now_secs();
    let days = secs / 86400;
    // Howard Hinnant's civil-from-days (same algorithm as
    // middleware/artifact_surface::rfc3339_to_unix, in reverse).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ─── Git auto-with-confirm (step 2a) ────────────────────────────────
//
// A fresh user who forgot to `git init` gets prompted once, kindly,
// with the reasons git unlocks value in the Sovereign workflow. Prior
// behavior was to silently treat git-absence as a "deferred" note in
// the observation report — they'd never see it until much later when
// an ATOS feature needed git to gate something. Making the prompt
// explicit up-front is the difference between "tool feels
// suffocating" and "tool is a collaborator."

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitOutcome {
    Present,
    InitializedNow,
    DeclinedByUser,
    DeclinedPreviously,
    NonInteractiveSkipped,
}

fn resolve_git(
    repo_root: &Path,
    has_git: bool,
    override_flag: Option<bool>,
    design_exists: bool,
    git_declined_previously: bool,
) -> GitOutcome {
    if has_git {
        return GitOutcome::Present;
    }

    match override_flag {
        Some(true) => {
            run_git_init(repo_root);
            return if repo_root.join(".git").exists() {
                GitOutcome::InitializedNow
            } else {
                eprintln!("    \u{2717} --yes-git set but `git init` did not create .git/");
                GitOutcome::NonInteractiveSkipped
            };
        }
        Some(false) => {
            println!();
            println!(
                "    \u{2026} git skipped (--no-git). ATOS features that need git stay disabled."
            );
            return GitOutcome::DeclinedByUser;
        }
        None => {}
    }

    // Respect a previous declination — don't re-badger the user on
    // every subsequent `init`. They already said no; it sticks.
    if git_declined_previously {
        return GitOutcome::DeclinedPreviously;
    }

    // Non-TTY stdin (piped / CI) without explicit flag: auto-init.
    // The rationale: scripts running `sovereign project init` in
    // fresh repos are almost always setting up a dev environment,
    // and git is what every downstream ATOS command assumes. If the
    // user truly wants no git, they pass --no-git.
    if !crate::util::prompts::stdin_is_tty() {
        println!();
        println!(
            "    No git repo; initializing (non-interactive default — pass --no-git to opt out)."
        );
        run_git_init(repo_root);
        return if repo_root.join(".git").exists() {
            GitOutcome::InitializedNow
        } else {
            GitOutcome::NonInteractiveSkipped
        };
    }

    // Interactive prompt. Kind, specific, and (when a design doc is
    // imminent) names the single most concrete win: per-revision
    // diffs of the DESIGN.md the user is about to author.
    eprintln!();
    eprintln!("  No git repo here yet. Sovereign works without one, but git unlocks:");
    if design_exists {
        eprintln!("    \u{00b7} per-revision diff of your DESIGN.md as you iterate with the agent");
    } else {
        eprintln!("    \u{00b7} per-revision diff of your DESIGN.md + CHARTER.md as they evolve");
    }
    eprintln!("    \u{00b7} `atos feature approve` gates");
    eprintln!("    \u{00b7} amendment history that survives machine changes");
    eprintln!();

    let accept = crate::util::prompts::confirm("  Run `git init` here now?", true);
    if accept {
        run_git_init(repo_root);
        if repo_root.join(".git").exists() {
            eprintln!("    \u{2713} Initialized git repo.");
            GitOutcome::InitializedNow
        } else {
            eprintln!("    \u{2717} `git init` did not create .git/ — continuing without git.");
            GitOutcome::NonInteractiveSkipped
        }
    } else {
        eprintln!("    \u{2026} git skipped. Run `git init` manually later if you change your mind.");
        GitOutcome::DeclinedByUser
    }
}

fn run_git_init(repo_root: &Path) {
    let status = std::process::Command::new("git")
        .arg("init")
        .current_dir(repo_root)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("    \u{2717} `git init` exited with status {s}");
        }
        Err(e) => {
            eprintln!("    \u{2717} could not spawn `git init`: {e} (is git installed?)");
        }
    }
}

fn derive_project_id(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ─── Observation report (M6.1) ──────────────────────────────
//
// Consumes `crate::observation::ProjectObservation` and renders it
// in three buckets (Ready / Actionable / Deferred-to-found) per the
// M6 requirements. Actionable items state the install command on
// its own line, copy-pasteable, unindented — the user can paste and
// run without editing.
/// Contextual flags the report uses to decide whether a missing
/// toolchain / git is ACTIONABLE (fix it now) or DEFERRED (we know
/// why it's fine). Keeps `print_observation_report` side-effect free
/// while letting `cmd_init` pass in what it knows.
struct ObservationReportContext {
    /// True when `<repo>/DESIGN.md` exists. A pre-code project with a
    /// design doc is a legitimate state — "no languages detected"
    /// becomes "indexing deferred" rather than an actionable error.
    design_exists: bool,
    /// User declined git (either this session or previously).
    /// Suppresses the "no git" deferred-bucket nudge — they already
    /// made the call.
    git_declined: bool,
}

fn print_observation_report(
    obs: &crate::observation::ProjectObservation,
    ctx: &ObservationReportContext,
) {
    use crate::observation::{DepKind, ScipTooling};

    let mut ready: Vec<String> = Vec::new();
    let mut actionable: Vec<(String, &'static str)> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // Languages & SCIP tooling. On a pre-code project with a design
    // doc present, "no languages" is expected — soft-path the
    // warning into the deferred bucket instead of treating it as a
    // gap the user must close right now.
    if obs.languages.is_empty() {
        if ctx.design_exists {
            deferred.push(
                "Pre-code project (DESIGN.md present, no source yet). Language detection runs on the next init."
                    .into(),
            );
        } else {
            actionable.push((
                "No supported languages detected (Rust, TypeScript, JavaScript, Go, Python, Java)."
                    .into(),
                "",
            ));
        }
    } else {
        for lang in &obs.languages {
            match &lang.scip_tooling {
                ScipTooling::Available { binary } => {
                    ready.push(format!("{} ({binary} on PATH)", lang.display));
                }
                ScipTooling::NotRequired => {
                    ready.push(lang.display.clone());
                }
                ScipTooling::Missing {
                    binary,
                    install_cmd,
                } => {
                    actionable.push((
                        format!(
                            "{} detected. Call-graph navigation requires `{binary}`:",
                            lang.display
                        ),
                        *install_cmd,
                    ));
                }
            }
        }
    }

    if obs.has_git {
        ready.push("Git repository".into());
    } else if !ctx.git_declined {
        // Not an actionable gap in the strict sense — git is
        // optional for init — but worth surfacing so the user knows
        // approvals-via-git won't be available.
        //
        // When the user explicitly declined git (now or previously),
        // suppress this note — they've already seen the tradeoff.
        deferred.push("No git repository — `atos feature approve` covers the gap.".into());
    }

    if obs.embed_model_available {
        ready.push("Embed model".into());
    } else {
        actionable.push((
            "Embed model not found (documentation search will be degraded).".into(),
            "sovereign setup",
        ));
    }

    // External dependencies — noted for `project found` (Stage 2
    // fault lines draws on this list). Not resolved at init time.
    let direct_deps: Vec<&crate::observation::DetectedDependency> = obs
        .deps
        .iter()
        .filter(|d| d.kind == DepKind::Direct)
        .collect();
    if !direct_deps.is_empty() {
        let n = direct_deps.len();
        deferred.push(format!(
            "{n} direct external dependenc{y} detected — surfaced to `sovereign project found`.",
            y = if n == 1 { "y" } else { "ies" }
        ));
    }

    // Render.
    if !ready.is_empty() {
        println!();
        for r in &ready {
            println!("    \u{2713} {r}");
        }
    }

    if !actionable.is_empty() {
        println!();
        for (desc, cmd) in &actionable {
            println!("    \u{26a0} {desc}");
            if !cmd.is_empty() {
                println!();
                println!("{cmd}");
                println!();
            }
        }
    }

    if !deferred.is_empty() {
        println!();
        for d in &deferred {
            println!("    \u{2026} {d}");
        }
    }
}

// ─── Language detection ──────────────────────────────────────

/// Local-to-`project_cmd` language detection struct. Distinct from
/// `crate::observation::LanguageObservation` which carries the
/// human-readable `display` form used by `print_observation_report`;
/// here we only need the stable `id` to drive SCIP-tooling decisions.
struct DetectedLanguage {
    id: &'static str,
}

fn detect_languages(root: &Path) -> Vec<DetectedLanguage> {
    let mut found = Vec::new();

    // Rust: Cargo.toml
    if root.join("Cargo.toml").exists() {
        found.push(DetectedLanguage { id: "rust" });
    }

    // TypeScript/JavaScript: tsconfig.json or package.json
    if root.join("tsconfig.json").exists() || root.join("tsconfig.base.json").exists() {
        found.push(DetectedLanguage { id: "typescript" });
    } else if root.join("package.json").exists() {
        // Check if it's TS or JS.
        let is_ts = root.join("tsconfig.json").exists()
            || has_file_extension_recursive(root, "ts", 2);
        if is_ts {
            found.push(DetectedLanguage { id: "typescript" });
        } else {
            found.push(DetectedLanguage { id: "javascript" });
        }
    }

    // Go: go.mod
    if root.join("go.mod").exists() {
        found.push(DetectedLanguage { id: "go" });
    }

    // Python: pyproject.toml, setup.py, or requirements.txt
    if root.join("pyproject.toml").exists()
        || root.join("setup.py").exists()
        || root.join("requirements.txt").exists()
    {
        found.push(DetectedLanguage { id: "python" });
    }

    found
}

/// Check whether any file with the given extension exists within `max_depth`
/// directory levels under `root`. Quick heuristic — not exhaustive.
fn has_file_extension_recursive(root: &Path, ext: &str, max_depth: usize) -> bool {
    has_ext_inner(root, ext, 0, max_depth)
}

fn has_ext_inner(dir: &Path, ext: &str, depth: usize, max_depth: usize) -> bool {
    if depth > max_depth {
        return false;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == ext)
                .unwrap_or(false)
            {
                return true;
            }
        } else if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Skip hidden dirs, node_modules, target, etc.
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                if has_ext_inner(&path, ext, depth + 1, max_depth) {
                    return true;
                }
            }
        }
    }
    false
}

// ─── Git helpers ─────────────────────────────────────────────

/// Walk upward from `start` looking for the first directory that contains a
/// `.sovereign/` subdirectory. Returns the `.sovereign/` path if found.
pub(crate) fn find_sovereign_dir(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join(".sovereign");
        if candidate.is_dir() {
            return Some(candidate);
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => return None,
        }
    }
}

pub(crate) fn find_repo_root() -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout);
        Some(PathBuf::from(s.trim()))
    } else {
        None
    }
}

// ─── File generation ─────────────────────────────────────────

fn generate_sovereign_md(
    corpus_id: &str,
    port: u16,
    langs: &[&str],
    has_scip: bool,
) -> String {
    let call_graph_section = if has_scip {
        "## Call graph

`find_callers` and `find_callees` use the SCIP graph.
New symbols have no graph entries until the next git commit
(the post-commit hook keeps this current automatically).
To refresh manually: `sovereign project refresh`"
    } else {
        "## Call graph

Call graph tools are not available (no SCIP exporter found).
Install a SCIP exporter and run `sovereign project refresh` to enable."
    };

    format!(
        r#"<!-- Generated by sovereign project init. Safe to commit. -->
# Sovereign Code Intelligence

MCP server: http://localhost:{port}/mcp
Corpus: {corpus_id}
Languages: {langs}

## Two ways to reach the tools

**MCP (preferred when the daemon is running).** Clients like Claude Code
auto-discover the full tool set via `tools/list` against the server above.
Structured, fast, native.

**CLI fallback.** The same 24 tools are callable as shell commands:

```
sovereign tools list                           # manifest, grouped by Effect × Scope
sovereign tools describe <id>                  # full descriptor + parameters + output keys + examples
sovereign tools call <id> [--key=value ...]    # invoke, plain text or --format json output
```

`sovereign tools call symbol_lookup --name=Foo` runs the same
`Tool::execute()` as the MCP path — pick whichever is in front of you. Use
the CLI when the daemon isn't up, when scripting, or to see `--help`.

## Tools

| Tool | When to use | Notes |
|---|---|---|
| `symbol_lookup` | Know the exact name | Always correct |
| `code_search` | Know the concept, not the name | Approximate — verify with symbol_lookup |
| `recent_changes` | Session start / orientation | Always correct |
| `find_callers` | What calls this function? | SCIP-resolved, catches trait dispatch |
| `find_callees` | What does this function call? | SCIP-resolved |
| `blast_radius` | Transitive impact of a change | BFS up to depth 5 |
| `project_context` | Project conventions, architecture | FTS5 over markdown docs |
| `write_note` | Record decisions, invariants, todos | Persists across sessions |
| `read_notes` | Recall prior decisions | FTS or filter by symbol/file/kind |
| `delete_note` | Remove stale notes | By ID |
| `session_reflection` | End of significant task — record tool feedback | Feeds `sovereign reflect` |
| `test_status` | Last test run result | |
| `run_tests` | Trigger a test run | |
| `get_run_output` | Test run stdout/stderr | |
| `lint_status` | Last lint result | |
| `get_lint_output` | Lint stdout/stderr | |

## Session start — do these first

1. Read `SYSTEM_OVERVIEW.md` at the repo root — do this every session. It is the authoritative map of what exists and how the pieces connect.
2. `recent_changes(hours: 24)` — see which subsystems are active
3. `project_context("<your task>")` — pull relevant conventions and docs
4. `read_notes(query: "<task area>")` — surface prior decisions

## Precision rules — do not skip

**DO NOT read an entire file to find a type definition.**
Call `symbol_lookup("TypeName")` first. It returns the exact definition
with file path and line numbers in one call. Fall back to Read only when
you need the surrounding context after locating the symbol.

**DO NOT grep for callers.**
Call `find_callers("function_name")` — compiler-resolved (SCIP), catches
trait dispatch that grep misses entirely.

**DO NOT guess at fields or constructor arguments.**
Even during greenfield work, call `symbol_lookup` before assuming a type's
shape. The writing is new; the patterns you're matching are not.

## Decision matrix

| Situation | Tool |
|---|---|
| "What files exist in this module?" | Glob + Read |
| "Show me the Foo struct" | `symbol_lookup("Foo")` |
| "What calls reindex_file?" | `find_callers("reindex_file")` |
| "What does ingest() call?" | `find_callees("ingest")` |
| "How does checkpoint resume work?" | `code_search` → `symbol_lookup` on results |
| "What changed recently?" | `recent_changes(hours: 24)` |
| "What are the conventions for X?" | `project_context("X")` |
| "What decisions were made about Y?" | `read_notes(query: "Y")` |
| "How many things depend on this?" | `blast_radius("symbol_name")` |

## Mandatory pre-flight checks

**Before adding a method to a trait:**
`find_callers("TraitName")` — finds ALL implementors. Every impl must
be updated or the build breaks. Do this before writing a single line.

**Before modifying a function signature:**
`find_callers("function_name")` — 20 callers needs a different strategy
than 2. Know the impact before touching the signature.

**Before any non-trivial change to an existing function:**
`blast_radius("function_name", max_depth: 2)` — see the transitive
callers, split by production vs test, grouped by module.

**Before using a type from another crate:**
`symbol_lookup("TypeName")` — confirm it exists, see its fields.
Faster and more reliable than grepping Cargo.toml.

## Writing notes

Use `write_note` to leave durable context for future sessions.
Write at the moment of the decision, not at the end.

- **`decision`** — chose one approach over alternatives; include the reason
- **`invariant`** — a constraint that must never be violated
- **`todo`** — follow-up work outside the current session's scope
- **`attempt`** — an approach that was tried and failed; prevents repetition

## Session reflection — at task end

Use `session_reflection` when a significant task is complete (refactor lands, bug fixed, feature shipped). Be specific.

```
session_reflection(
  task_summary: "Refactored EmbedFn across 12 call sites",
  tool_name: "blast_radius",
  tools_that_helped: ["blast_radius", "lint_status"],
  manual_work_that_should_be_a_tool: "Had to grep for macro invocations blast_radius missed",
  wished_i_had_known: "EmbedFn is wrapped in a macro — blast_radius does not surface macro call sites"
)
```

**Before using `blast_radius` or `project_context` on a large task**, check for known limitations first:
`read_notes(kinds=["reflection"], query="<tool_name>")` — limitations disappear from results
once the developer retires them via `sovereign reflect --retire`.

When you see `[sovereign] N tool calls this session. Consider calling session_reflection…`
appended to a tool response, it is a nudge — write one when the work feels significant.

## Developer: reviewing reflections

`sovereign reflect` reads the accumulated backlog from any directory — it finds the active
database automatically via `~/.sovereign/active_notes_db`.

```bash
sovereign reflect                          # 30-day summary: signals, what helped, open todos
sovereign reflect --since 7d              # narrow the window
sovereign reflect --tool blast_radius     # focus on one tool
sovereign reflect --raw                   # full prose, ungrouped
sovereign reflect --todos                 # open todo notes
sovereign reflect --history               # include retired reflections
```

**Retiring a fixed limitation** — once resolved, retire the reflection so agents stop seeing
it as an active warning:

```bash
sovereign reflect --retire --tool blast_radius --reason "macro scan added in PR #88"
sovereign reflect --retire --id <uuid>    --reason "no longer relevant"  # add --yes to skip prompt
```

Retired reflections are hidden from agents but preserved in `--history` for audit.

## Compilation and test feedback

**DO NOT run `cargo build`, `cargo check`, or `cargo test` via Bash**
in this project. Running these directly contends with the background
watcher for the Cargo file lock — one blocks the other and you idle.

The watcher runs cargo check continuously on file changes. The result
is usually already cached by the time you finish an edit.

**"Does this compile?"** → `lint_status`
| Status | Meaning | Action |
|---|---|---|
| `fresh_passing` | Clean | Keep going |
| `fresh_failing` | Errors in response | Fix them |
| `stale` | Watcher queued | Call again in ~15s |
| `running` | In progress | Call again in ~15s |
| `never_run` | Watcher not configured | Fall back to Bash |

**"Do tests pass?"** → `test_status`
| Status | Meaning | Action |
|---|---|---|
| `fresh_passing` | All pass | Safe to proceed |
| `fresh_failing` | Failures in response | Fix them |
| `stale` | Files changed since last run | `run_tests`, then poll |
| `running` | In progress | Poll every ~30s |
| `never_run` | Watcher not configured | Fall back to Bash |

Call `get_lint_output` / `get_run_output` **only** when
`output_truncated: true`. The errors are already in the status response.
Never poll in a tight loop — use a 15-30s gap between checks.

## Server lifecycle

`sovereign project serve` hot-reloads SCIP every 30 seconds. Post-commit
hooks keep both the symbol index and call graph current automatically.
If something seems stale, check `~/.sovereign/hooks.log` and run
`sovereign project install-hooks` if the hook predates recent changes.

{call_graph_section}

## Project-specific invariants

Add invariants below this line. These are read by the coding agent
at session start and treated as authoritative guidance from the
architect.

---
"#,
        langs = langs.join(", "),
    )
}

/// Generate `.opencode/config.json` — registers the sovereign MCP server and,
/// when Commonwealth is configured, a custom OpenAI-compatible provider backed
/// by the OICP mesh. Models are populated from live OICP capabilities when
/// available; falls back to a single `"auto"` entry otherwise.
fn generate_opencode_config(
    port: u16,
    commonwealth_url: Option<&str>,
    commonwealth_models: &[String],
) -> String {
    let mut config = serde_json::json!({
        "mcp": {
            "servers": {
                "sovereign": {
                    "type": "http",
                    "url": format!("http://localhost:{port}/mcp")
                }
            }
        }
    });

    if let Some(url) = commonwealth_url {
        let base = format!(
            "{}/v1",
            url.trim_end_matches('/').replace(":9742", ":9741")
        );

        let mut models = serde_json::Map::new();
        for id in commonwealth_models {
            models.insert(id.clone(), serde_json::json!({ "name": id }));
        }
        if models.is_empty() {
            // Commonwealth not reachable at init time; "auto" routes at runtime.
            models.insert(
                "auto".into(),
                serde_json::json!({ "name": "Commonwealth (auto-routed)" }),
            );
        }

        config["provider"] = serde_json::json!({
            "commonwealth": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "Commonwealth Mesh",
                "options": { "baseURL": base },
                "models": models
            }
        });
    }

    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Merge a generated opencode config into an existing one without clobbering
/// other MCP servers or user settings. Only adds the `sovereign` server entry.
fn merge_opencode_config(existing: &str, generated: &str) -> String {
    let mut base: serde_json::Value =
        serde_json::from_str(existing).unwrap_or(serde_json::json!({}));
    let new: serde_json::Value = match serde_json::from_str(generated) {
        Ok(v) => v,
        Err(_) => return generated.to_string(),
    };

    if let Some(new_servers) = new
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .and_then(|s| s.as_object())
    {
        let mcp = base
            .as_object_mut()
            .unwrap()
            .entry("mcp")
            .or_insert_with(|| serde_json::json!({}));
        let servers = mcp
            .as_object_mut()
            .unwrap()
            .entry("servers")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(map) = servers.as_object_mut() {
            for (k, v) in new_servers {
                map.insert(k.clone(), v.clone());
            }
        }
    }

    // Merge provider.commonwealth — add or update, preserve other providers.
    if let Some(cw_provider) = new
        .get("provider")
        .and_then(|p| p.get("commonwealth"))
    {
        let provider = base
            .as_object_mut()
            .unwrap()
            .entry("provider")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(map) = provider.as_object_mut() {
            map.insert("commonwealth".into(), cw_provider.clone());
        }
    }

    serde_json::to_string_pretty(&base).unwrap_or_else(|_| generated.to_string())
}

/// Generate a starter AGENTS.md for projects that don't have one.
/// Opencode (and other agents that read AGENTS.md) use this as their
/// system-level instructions for the codebase.
fn generate_agents_md(corpus_id: &str, port: u16, has_scip: bool, commonwealth_url: Option<&str>) -> String {
    let scip_section = if has_scip {
        "\n## Pre-flight before editing\n\
         \n\
         - **Before changing a function signature**: `find_callers(\"fn\")` first — counts every call site.\n\
         - **Before adding a method to a trait**: `find_callers(\"TraitName\")` to find all implementors.\n\
         - **Before a non-trivial refactor**: `blast_radius(\"symbol\", max_depth: 2)`.\n"
    } else {
        "\n## Pre-flight before editing\n\
         \n\
         Call graph tools are not available (SCIP not enabled). Use `code_search` to find usage patterns.\n"
    };

    let inference_section = if let Some(url) = commonwealth_url {
        let base = url.trim_end_matches('/').replace(":9742", ":9741");
        format!(
            "\n## Inference\n\
             \n\
             Commonwealth mesh provider is configured at `{base}`.\n\
             Use model `commonwealth/<model-id>` in opencode to route through the mesh.\n\
             Run `GET {base}/v1/models` to list currently available models.\n"
        )
    } else {
        String::new()
    };

    format!(
        "# Agent instructions — {corpus_id}\n\
         \n\
         ## Code intelligence (MCP preferred; CLI fallback)\n\
         \n\
         A sovereign MCP server at `http://localhost:{port}/mcp` exposes compiler-resolved\n\
         tools for this codebase. **Use MCP tools before reading files.**\n\
         \n\
         When MCP isn't available (daemon stopped, scripting, ad-hoc shell use), the\n\
         same tools are reachable as `sovereign tools {{list|describe|call}} ...` —\n\
         plain-text output, `--help` on every subcommand. `sovereign tools list`\n\
         prints the live catalog grouped by Effect × Scope. Same `Tool::execute()`\n\
         underneath either path.\n\
         \n\
         | Tool | Purpose |\n\
         |---|---|\n\
         | `symbol_lookup` | Exact struct/trait/fn definition — use instead of reading files |\n\
         | `find_callers` | All call sites, compiler-resolved (catches trait dispatch) |\n\
         | `find_callees` | What a function calls |\n\
         | `blast_radius` | Transitive impact of changing a symbol |\n\
         | `code_search` | Semantic search across the codebase |\n\
         | `recent_changes` | Files changed in the last N hours |\n\
         | `project_context` | Architecture and conventions for a topic |\n\
         | `read_notes` | Decisions and invariants from prior sessions |\n\
         | `write_note` | Record a decision, invariant, todo, or failed attempt |\n\
         | `lint_status` | Check for compile errors (do not run `cargo check` via shell) |\n\
         | `test_status` | Check test results (do not run `cargo test` via shell) |\n\
         \n\
         ## Required session start\n\
         \n\
         1. `read_notes(query: \"active\")` — surface active invariants and todos\n\
         2. `project_context(\"<task area>\")` — pull relevant conventions\n\
         3. `recent_changes(hours: 24)` — see what's been touched\n\
         {scip_section}\n\
         ## Build and test feedback\n\
         \n\
         The sovereign watcher runs continuously in the background. **Do not run `cargo check`,\n\
         `cargo build`, `cargo test`, or `cargo clippy` directly** — they contend with the watcher\n\
         for the Cargo file lock and stall both processes.\n\
         \n\
         - Check compile status: `lint_status` — response includes `age_seconds`, `watched_scope`,\n\
           and `watcher_active` so you can confirm the result covers your changes.\n\
         - Check test status: `test_status` — if stale, call `run_tests` then poll.\n\
         - Only fall back to `cargo` commands when `lint_status` returns `watcher_active: false`.\n\
         \n\
         ## Session discipline\n\
         \n\
         - Call `write_note(kind: \"invariant\")` when you discover a constraint that must never be violated.\n\
         - Call `write_note(kind: \"decision\")` when you choose one approach over alternatives.\n\
         - Call `session_reflection` at the end of any significant task.\n\
         {inference_section}"
    )
}

/// Generate the inject-notes.sh hook script content for the given port.
///
/// The hook is ATOS-aware: when `$SOVEREIGN_FEATURE_ID` is set in the driver
/// environment (see `sovereign atos start-milestone`), the MCP call includes
/// `scope=["global","feature"]` plus that feature_id so the agent sees both
/// global invariants and the in-flight feature's decisions. Outside an ATOS
/// session the hook scopes to globals only — feature-specific chatter from
/// in-flight work does not leak into unrelated Claude sessions.
fn generate_inject_notes_script(port: u16) -> String {
    format!(
        r#"#!/bin/sh
# sovereign inject-notes — UserPromptSubmit hook for Claude Code.
# Fetches active invariants and decisions from the sovereign MCP server and
# prints them as context before every Claude response.
# Fails silently when the server is not running so offline work is unaffected.

PORT="${{SOVEREIGN_PORT:-{port}}}"

# ATOS scope-aware payload. When $SOVEREIGN_FEATURE_ID is set (by
# `sovereign atos start-milestone`), the query pulls global notes plus
# the active feature's notes. Otherwise only globals are injected.
if [ -n "${{SOVEREIGN_FEATURE_ID:-}}" ]; then
  PAYLOAD=$(printf '{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"read_notes","arguments":{{"kinds":["invariant","decision"],"scope":["global","feature"],"feature_id":"%s","limit":20}}}}}}' "$SOVEREIGN_FEATURE_ID")
else
  PAYLOAD='{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"read_notes","arguments":{{"kinds":["invariant","decision"],"scope":["global"],"limit":20}}}}}}'
fi

RESPONSE=$(curl -sf --max-time 2 \
  -X POST "http://localhost:${{PORT}}/mcp" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD" \
  2>/dev/null) || exit 0

[ -z "$RESPONSE" ] && exit 0

printf '%s' "$RESPONSE" | python3 -c "
import sys, os, json

try:
    outer = json.load(sys.stdin)
    inner_text = outer['result']['content'][0]['text']
    inner = json.loads(inner_text)
    notes = inner.get('notes', [])
    if not notes:
        sys.exit(0)
    fid = os.environ.get('SOVEREIGN_FEATURE_ID', '')
    header = '## Active sovereign notes (injected by hook)'
    if fid:
        header = header + ' (feature=' + fid + ')'
    print(header)
    print()
    for n in notes:
        kind = n.get('kind', 'note')
        scope = n.get('scope', 'global')
        content = n.get('content', '').strip()
        tag = kind if scope == 'global' else kind + '/' + scope
        print('[' + tag + '] ' + content)
        print()
except Exception:
    sys.exit(0)
" 2>/dev/null
"#
    )
}

fn generate_claude_settings(
    port: u16,
    corpus_id: &str,
    has_git: bool,
    has_scip: bool,
) -> String {
    let scip_instruction = if has_scip {
        "MANDATORY PRE-FLIGHT — before modifying any existing function: \
         call find_callers to count dependents; call blast_radius for \
         transitive impact. Before adding a method to a trait: call \
         find_callers on the trait name to find every implementor — \
         all must be updated or the build breaks. Before implementing \
         a new function: call find_callees on a similar function to \
         see the established pattern."
    } else {
        "Call graph tools are not available (no SCIP exporter). \
         Use code_search to find usage patterns."
    };

    let git_instruction = if has_git {
        " This is a git repository. recent_changes reflects \
         filesystem modification time, not git history."
    } else {
        ""
    };

    let system_prompt = format!(
        "You have access to Sovereign code intelligence for \
         the {corpus_id} codebase via MCP. \
         Read .sovereign/SOVEREIGN.md for the full tool reference \
         and project-specific invariants. When MCP isn't available \
         (daemon stopped, scripting, non-MCP client), the same tools \
         are reachable as `sovereign tools {{list|describe|call}}` — \
         see `.sovereign/SOVEREIGN.md` for the CLI section.\n\n\
         SESSION START — run these three calls before anything else:\n\
         1. recent_changes(hours: 24) — see which subsystems are active\n\
         2. project_context(\"<user task>\") — pull relevant conventions\n\
         3. read_notes(query: \"<task area>\") — recall prior decisions\n\n\
         PRECISION RULES — never skip:\n\
         - DO NOT read an entire file to find a type definition. \
           Call symbol_lookup(\"TypeName\") first. Only use Read when \
           you need the surrounding context after locating the symbol.\n\
         - DO NOT grep for callers. Call find_callers — it is \
           compiler-resolved (SCIP) and catches trait dispatch that \
           grep misses entirely.\n\
         - DO NOT guess at field names or constructor arguments. \
           Call symbol_lookup even during greenfield work.\n\n\
         {scip_instruction}\n\n\
         BUILD & TEST FEEDBACK — never run cargo via Bash in watched \
         projects; it contends for the Cargo lock and stalls both:\n\
         - 'Does this compile?' → lint_status (instant, pre-computed)\n\
           fresh_passing=clean; fresh_failing=errors in response; \
           stale=watcher queued, check again in ~15s; \
           running=check again in ~15s; \
           never_run=watcher not configured, THEN use Bash.\n\
         - 'Do tests pass?' → test_status (instant, pre-computed)\n\
           fresh_passing=safe; fresh_failing=failures in response; \
           stale=call run_tests then poll every ~30s; \
           running=poll every ~30s; \
           never_run=watcher not configured, THEN use Bash.\n\
         - Call get_lint_output / get_run_output only when \
           output_truncated is true. Never poll in a tight loop.\n\n\
         WRITING NOTES — use write_note at the moment of the decision:\n\
         - kind=decision: chose approach X over Y; include the reason\n\
         - kind=invariant: constraint that must never be violated\n\
         - kind=todo: follow-up outside this session's scope\n\
         - kind=attempt: tried and failed; prevents repetition\n\n\
         SESSION REFLECTION — at the end of a significant task call session_reflection: \
         record which tools helped, what you had to do manually that a tool should have \
         handled, and what you wished you had known earlier. Be specific. \
         Before using blast_radius or project_context on a large task: \
         read_notes(kinds=[\"reflection\"], query=\"<tool_name>\") to check for known \
         limitations recorded by previous sessions.\n\n\
         {git_instruction}"
    );

    serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "sovereign": {
                "type": "http",
                "url": format!("http://localhost:{port}/mcp")
            }
        },
        "systemPrompt": system_prompt,
        "hooks": {
            "UserPromptSubmit": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": "sh .claude/hooks/inject-notes.sh"
                        }
                    ]
                }
            ]
        }
    }))
    .unwrap_or_default()
}

/// Merge generated settings into existing .claude/settings.json without
/// clobbering user settings. Adds the sovereign MCP entry and prepends
/// the system prompt.
fn merge_claude_settings(existing: &str, generated: &str) -> String {
    let mut base: serde_json::Value =
        serde_json::from_str(existing).unwrap_or(serde_json::json!({}));
    let new: serde_json::Value = match serde_json::from_str(generated) {
        Ok(v) => v,
        Err(_) => return generated.to_string(),
    };

    // Merge mcpServers — add sovereign without removing others.
    if let Some(new_servers) = new.get("mcpServers").and_then(|v| v.as_object()) {
        let base_servers = base
            .as_object_mut()
            .unwrap()
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(base_map) = base_servers.as_object_mut() {
            for (k, v) in new_servers {
                base_map.insert(k.clone(), v.clone());
            }
        }
    }

    // Update the systemPrompt. If the existing prompt contains
    // non-Sovereign content, preserve it after the Sovereign block.
    if let Some(new_prompt) = new.get("systemPrompt").and_then(|v| v.as_str()) {
        if let Some(existing_prompt) = base.get("systemPrompt").and_then(|v| v.as_str()) {
            if existing_prompt.contains("Sovereign code intelligence") {
                // Replace the existing Sovereign block with the latest version.
                // If there's user content after the "---" separator, keep it.
                let user_section = existing_prompt
                    .split("\n\n---\n\n")
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                if user_section.is_empty() {
                    base["systemPrompt"] = serde_json::json!(new_prompt);
                } else {
                    base["systemPrompt"] =
                        serde_json::json!(format!("{new_prompt}\n\n---\n\n{user_section}"));
                }
            } else {
                // No Sovereign content yet — prepend.
                base["systemPrompt"] =
                    serde_json::json!(format!("{new_prompt}\n\n---\n\n{existing_prompt}"));
            }
        } else {
            base["systemPrompt"] = serde_json::json!(new_prompt);
        }
    }

    // Merge hooks — add the sovereign UserPromptSubmit hook without removing
    // other hooks the user may have configured. We identify our hook by its
    // command string and skip re-adding it if already present.
    if let Some(new_hooks) = new.get("hooks").and_then(|v| v.as_object()) {
        let base_hooks = base
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));
        if let (Some(base_hooks_map), Some(new_ups)) = (
            base_hooks.as_object_mut(),
            new_hooks.get("UserPromptSubmit").and_then(|v| v.as_array()),
        ) {
            let existing_ups = base_hooks_map
                .entry("UserPromptSubmit")
                .or_insert_with(|| serde_json::json!([]));
            if let Some(arr) = existing_ups.as_array_mut() {
                for new_entry in new_ups {
                    // Check if the exact command is already present.
                    let new_cmd = new_entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .and_then(|h| h.first())
                        .and_then(|h| h.get("command"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    let already_present = arr.iter().any(|e| {
                        e.get("hooks")
                            .and_then(|h| h.as_array())
                            .and_then(|h| h.first())
                            .and_then(|h| h.get("command"))
                            .and_then(|c| c.as_str())
                            .map(|c| c == new_cmd)
                            .unwrap_or(false)
                    });
                    if !already_present {
                        arr.push(new_entry.clone());
                    }
                }
            }
        }
    }

    serde_json::to_string_pretty(&base).unwrap_or_else(|_| generated.to_string())
}

// ─── .gitignore ──────────────────────────────────────────────

fn update_gitignore(root: &Path) -> std::io::Result<()> {
    let gitignore_path = root.join(".gitignore");
    let entries_to_add = [".sovereign/project.json", ".sovereign/scip/"];

    let existing = if gitignore_path.exists() {
        std::fs::read_to_string(&gitignore_path)?
    } else {
        String::new()
    };

    let mut additions = Vec::new();
    for entry in entries_to_add {
        if !existing.lines().any(|line| line.trim() == entry) {
            additions.push(entry);
        }
    }

    if additions.is_empty() {
        return Ok(());
    }

    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }

    if !content.is_empty() {
        content.push_str("\n# Sovereign code intelligence\n");
    } else {
        content.push_str("# Sovereign code intelligence\n");
    }
    for entry in additions {
        content.push_str(entry);
        content.push('\n');
    }

    std::fs::write(&gitignore_path, content)
}

// ─── Install hooks (standalone upgrade path) ──────────────────

/// Upgrade (or install) the post-commit hook in the current repo without
/// running the full `project init` pipeline. Safe to re-run; detects and
/// rewrites prior-version hook blocks in place.
async fn cmd_install_hooks(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_INSTALL_HOOKS);
        return 0;
    }
    // Deprecated. The daemon's Reindexer now keeps the graph fresh
    // via FS watcher + git HEAD poll + startup catch-up, so the old
    // post-commit hook is no longer useful (and its failure modes
    // were the reason for the rewrite). Remove any legacy hook we
    // find and tell the user why.
    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => {
            eprintln!("error: not inside a git repository");
            return 1;
        }
    };
    match remove_legacy_hook(&repo_root) {
        Ok(removed) => {
            if removed {
                println!(
                    "  \u{2713} Removed legacy post-commit hook from {}/.git/hooks/post-commit",
                    repo_root.display()
                );
            } else {
                println!("  No legacy sovereign hook found — nothing to do.");
            }
            println!(
                "\n  The daemon now owns freshness. Register this project with:\n\
                 \n    sovereign project register\n\n\
                 The FS watcher + git-HEAD poll keep the graph fresh without a hook.",
            );
            0
        }
        Err(e) => {
            eprintln!("error: could not clean up hook: {e}");
            1
        }
    }
}

// ─── Daemon-owned project lifecycle (register / list / watch) ───

/// Base URL the CLI uses to talk to the local daemon. Hardcoded to
/// `127.0.0.1:9741` — the freshness HTTP surface is loopback-only
/// by design, so we never talk to a remote host.
const DAEMON_BASE: &str = "http://127.0.0.1:9741";

pub(crate) async fn cmd_register(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        print_simple_help(
            "sovereign project register",
            "Register the current directory with the daemon's freshness pipeline.",
            &[
                "sovereign project register",
                "sovereign project register --root /path/to/repo",
                "sovereign project register --name my-monorepo",
            ],
        );
        return 0;
    }

    let mut root: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = args.get(i).map(PathBuf::from);
            }
            "--name" => {
                i += 1;
                name = args.get(i).cloned();
            }
            _ => {}
        }
        i += 1;
    }

    let root = root
        .or_else(find_repo_root)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let root = root.canonicalize().unwrap_or(root);
    let corpus_id = name.unwrap_or_else(|| derive_corpus_id(&root));

    let body = serde_json::json!({
        "corpus_id": corpus_id,
        "root": root.display().to_string(),
    });
    match daemon_post("/v1/projects/register", body).await {
        Ok(resp) => {
            let created = resp["created"].as_bool().unwrap_or(false);
            println!(
                "  \u{2713} {} project \"{}\" at {}",
                if created { "Registered" } else { "Updated" },
                corpus_id,
                root.display()
            );
            println!("    The daemon is now watching this project. Use `sovereign project watch status` to inspect.");
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            eprintln!("hint: is the daemon running? try `sovereign daemon status`.");
            1
        }
    }
}

pub(crate) async fn cmd_unregister(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        print_simple_help(
            "sovereign project unregister",
            "Stop the daemon from watching a project.",
            &["sovereign project unregister <corpus_id>"],
        );
        return 0;
    }
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: missing corpus_id. usage: sovereign project unregister <corpus_id>");
        return 1;
    };
    match daemon_post(&format!("/v1/projects/{corpus_id}/unregister"), serde_json::json!({}))
        .await
    {
        Ok(resp) => {
            let removed = resp["removed"].as_bool().unwrap_or(false);
            if removed {
                println!("  \u{2713} Unregistered \"{corpus_id}\".");
            } else {
                println!("  \"{corpus_id}\" was not registered — nothing to do.");
            }
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            1
        }
    }
}

pub(crate) async fn cmd_list(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        print_simple_help(
            "sovereign project list",
            "List every project the daemon is watching.",
            &["sovereign project list"],
        );
        return 0;
    }
    match daemon_get("/v1/projects").await {
        Ok(resp) => {
            let Some(projects) = resp["projects"].as_array() else {
                println!("  (empty response)");
                return 0;
            };
            if projects.is_empty() {
                println!("  No projects registered yet. Run `sovereign project register` in a repo to add one.");
                return 0;
            }
            println!("  Registered projects:");
            for p in projects {
                let id = p["corpus_id"].as_str().unwrap_or("?");
                let root = p["root"].as_str().unwrap_or("?");
                let age = p["graph_age_secs"].as_u64();
                let age_str = match age {
                    Some(s) => format_graph_age(s),
                    None => "never built".to_string(),
                };
                let in_flight = p["rebuild_in_flight"].as_bool().unwrap_or(false);
                println!(
                    "    {id}  ({age_str}){}",
                    if in_flight { "  [rebuilding]" } else { "" }
                );
                println!("      root: {root}");
            }
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            eprintln!("hint: is the daemon running? try `sovereign daemon status`.");
            1
        }
    }
}

pub(crate) async fn cmd_watch(args: &[String]) -> i32 {
    if args.is_empty() || crate::util::help::wants_help(args) {
        print_simple_help(
            "sovereign project watch",
            "Inspect or control per-project watchers.",
            &[
                "sovereign project watch status [<id>]",
                "sovereign project watch restart <id> [<watcher>]",
                "sovereign project watch logs <id> <watcher>",
            ],
        );
        return if args.is_empty() { 1 } else { 0 };
    }
    match args[0].as_str() {
        "status" => cmd_watch_status(&args[1..]).await,
        "restart" => cmd_watch_restart(&args[1..]).await,
        "logs" => cmd_watch_logs(&args[1..]).await,
        other => {
            eprintln!("Unknown watch subcommand: {other}");
            1
        }
    }
}

async fn cmd_watch_status(args: &[String]) -> i32 {
    let target = args.first().cloned();
    let resp = match daemon_get("/v1/projects").await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            return 1;
        }
    };
    let Some(projects) = resp["projects"].as_array() else {
        return 1;
    };
    let filtered: Vec<_> = projects
        .iter()
        .filter(|p| match target.as_deref() {
            Some(id) => p["corpus_id"].as_str() == Some(id),
            None => true,
        })
        .collect();
    if filtered.is_empty() {
        if let Some(id) = target {
            eprintln!("\"{id}\" is not registered. run `sovereign project list` to see registered projects.");
        } else {
            println!("  no projects registered yet.");
        }
        return 1;
    }
    for p in filtered {
        let id = p["corpus_id"].as_str().unwrap_or("?");
        println!("  {id}");
        let Some(status) = p["status"].as_object() else { continue };
        for (watcher, s) in status {
            let state = s["state"].as_str().unwrap_or("?");
            let extra = match state {
                "crashed" => {
                    let reason = s["reason"].as_str().unwrap_or("?");
                    let count = s["count"].as_u64().unwrap_or(0);
                    format!(" — {count} crashes, last: {reason}")
                }
                "disabled" => {
                    let reason = s["reason"].as_str().unwrap_or("?");
                    format!(" — {reason}")
                }
                _ => String::new(),
            };
            println!("    {watcher:8}  {state}{extra}");
        }
        if let Some(age) = p["graph_age_secs"].as_u64() {
            println!("    graph age: {}", format_graph_age(age));
        }
    }
    0
}

async fn cmd_watch_restart(args: &[String]) -> i32 {
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: usage: sovereign project watch restart <corpus_id>");
        return 1;
    };
    // For the MVP, "restart" just means "trigger a rebuild". A
    // full per-watcher restart (re-spawn a Disabled test runner,
    // for example) requires state plumbing that lands in a later
    // step; rebuild is the action users reach for 90% of the time.
    match daemon_post(
        &format!("/v1/projects/{corpus_id}/rebuild"),
        serde_json::json!({ "reason": "manual restart via CLI" }),
    )
    .await
    {
        Ok(_) => {
            println!("  \u{2713} Rebuild nudged for \"{corpus_id}\".");
            println!("    Check progress with `sovereign project watch status {corpus_id}`.");
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            1
        }
    }
}

async fn cmd_watch_logs(args: &[String]) -> i32 {
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: usage: sovereign project watch logs <corpus_id> [<watcher>]");
        return 1;
    };
    let watcher = args.get(1).cloned().unwrap_or_else(|| "scip".to_string());
    let log_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".sovereign")
        .join("logs")
        .join(format!("watch-{corpus_id}-{watcher}.log"));
    if !log_path.exists() {
        eprintln!(
            "no log file at {} — the daemon writes per-watcher logs here once the first cycle runs.",
            log_path.display()
        );
        return 1;
    }
    // Print the file contents. `tail -f` semantics would be nicer
    // but pulling in a tailer adds complexity; reading once and
    // exiting is predictable and scriptable.
    match std::fs::read_to_string(&log_path) {
        Ok(s) => {
            print!("{s}");
            0
        }
        Err(e) => {
            eprintln!("error: read {}: {e}", log_path.display());
            1
        }
    }
}

// ─── Small helpers used by the new subcommands ───────────────

/// Best-guess corpus id for a project root. Matches the logic
/// `cmd_init` uses so `register` and `init` produce the same
/// registration key by default.
fn derive_corpus_id(root: &Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

/// Render a duration-since in a compact, human-readable form.
/// Used by `project list` and `project watch status`. Named
/// `format_graph_age` to avoid colliding with the older helper
/// in this module that produces a different phrasing.
fn format_graph_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s old")
    } else if secs < 3600 {
        format!("{}m old", secs / 60)
    } else if secs < 86400 {
        format!("{}h old", secs / 3600)
    } else {
        format!("{}d old", secs / 86400)
    }
}

async fn daemon_post(
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("{DAEMON_BASE}{path}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {path}: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or(serde_json::json!({"error": "non-JSON response"}));
    if !status.is_success() {
        return Err(format!("{status}: {body}"));
    }
    Ok(body)
}

/// Cheap TCP + `GET /v1/models` probe. Matches what the desktop's
/// bootstrap does (see `sovereign-desktop/src-tauri/src/bootstrap.rs`).
/// Used by `cmd_serve` to decide whether to refuse the legacy path.
pub(crate) async fn daemon_is_running() -> bool {
    let tcp = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::net::TcpStream::connect(("127.0.0.1", 9741)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);
    if !tcp {
        return false;
    }
    match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
    {
        Ok(c) => c
            .get("http://127.0.0.1:9741/v1/models")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false),
        Err(_) => false,
    }
}

async fn daemon_get(path: &str) -> Result<serde_json::Value, String> {
    let url = format!("{DAEMON_BASE}{path}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {path}: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or(serde_json::json!({"error": "non-JSON response"}));
    if !status.is_success() {
        return Err(format!("{status}: {body}"));
    }
    Ok(body)
}

fn print_simple_help(command: &str, summary: &str, examples: &[&str]) {
    println!();
    println!("  {command}");
    println!("  {}", "─".repeat(50));
    println!("  {summary}");
    println!();
    println!("  Usage:");
    for ex in examples {
        println!("    {ex}");
    }
    println!();
}

/// Scan `.git/hooks/post-commit` for a `SOVEREIGN_HOOK_V*` marker
/// and remove the whole file (we were the sole owner). Returns
/// `Ok(true)` when a hook was removed, `Ok(false)` when none was
/// found. If the hook file contains both sovereign content and
/// other content, we leave it alone — the user is expected to
/// clean it up manually.
fn remove_legacy_hook(repo_root: &Path) -> std::io::Result<bool> {
    let hook_path = repo_root.join(".git/hooks/post-commit");
    if !hook_path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&hook_path)?;
    let is_sovereign_only = content
        .lines()
        .any(|l| l.starts_with("# SOVEREIGN_HOOK_V"))
        && !content.contains("# non-sovereign");
    if !is_sovereign_only {
        return Ok(false);
    }
    std::fs::remove_file(&hook_path)?;
    Ok(true)
}

// ─── Git hooks (deprecated installer — kept for migration tests only) ──
//
// The post-commit hook installer used to be wired into `cmd_init`, but
// freshness is now handled by the daemon's watcher (see `corpus-engine`
// `update::watcher` and the daemon's reindex loop). The CLI still
// recognizes `sovereign project install-hooks` as a deprecated
// subcommand that prints a migration hint, but no production code path
// installs a hook anymore.
//
// The installer + stripper functions below are retained because they
// still pin the hook-block format invariants in tests
// (`strip_prior_sovereign_block_*` etc.) — those tests run against a
// fixed string corpus so a regression in the format could still trip a
// real user with a legacy hook installed by an older binary.
#[allow(dead_code)]
const SOVEREIGN_HOOK_MARKER: &str = "# SOVEREIGN_HOOK_V3";

#[allow(dead_code)]
fn install_post_commit_hook(root: &Path, corpus_id: &str) -> std::io::Result<()> {
    let hook_path = root.join(".git/hooks/post-commit");
    let _ = corpus_id; // corpus_id resolved from project.json by refresh

    // Resolve the binary path: use the current executable if it exists,
    // otherwise fall back to "sovereign" on PATH. This way the hook
    // works both for developers running from a local build and for
    // global installs.
    let current_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());

    // The hook runs BOTH passes so every tool stays fresh:
    //   1. `project init --no-scip` — re-ingests symbols so symbol_lookup,
    //      code_search, and recent_changes return up-to-date results.
    //   2. `project refresh` — exports SCIP + call graph so find_callers
    //      and find_callees reflect the new commit.
    //
    // Output is redirected to ~/.sovereign/hooks.log so failures are
    // visible (a silent `&` swallows errors and leaves the user
    // wondering why MCP still serves stale data).
    //
    // We use a POSIX group command `{ ... } </dev/null &` rather than
    // `setsid` because setsid is Linux-only (util-linux) and not available
    // on macOS. The group command backgrounded with `&` is sufficient to
    // prevent git from waiting on the refresh, and `/dev/null` on stdin
    // prevents any accidental blocking reads.
    let hook_block = if let Some(ref exe) = current_exe {
        format!(
            r#"{marker}
# Sovereign: keep code intelligence fresh after each commit.
# Runs `project init --no-scip` (symbols) + `project refresh` (SCIP) in
# the background; output streams to ~/.sovereign/hooks.log.
LOG="$HOME/.sovereign/hooks.log"
mkdir -p "$(dirname "$LOG")"
SOVEREIGN="{exe}"
if [ ! -x "$SOVEREIGN" ]; then
  command -v sovereign >/dev/null 2>&1 || exit 0
  SOVEREIGN=sovereign
fi
{{
  printf "[%s] post-commit refresh (pid $$)\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$LOG"
  "$SOVEREIGN" project init --no-scip --no-hooks --no-claude-config >> "$LOG" 2>&1
  status_init=$?
  "$SOVEREIGN" project refresh --quiet >> "$LOG" 2>&1
  status_refresh=$?
  printf "[%s] done — init=%d refresh=%d\n\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$status_init" "$status_refresh" >> "$LOG"
}} </dev/null &
"#,
            marker = SOVEREIGN_HOOK_MARKER,
            exe = exe.display()
        )
    } else {
        // Fall back to PATH lookup for global installs.
        format!(
            r#"{marker}
# Sovereign: keep code intelligence fresh after each commit.
LOG="$HOME/.sovereign/hooks.log"
mkdir -p "$(dirname "$LOG")"
command -v sovereign >/dev/null 2>&1 || exit 0
{{
  printf "[%s] post-commit refresh (pid $$)\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$LOG"
  sovereign project init --no-scip --no-hooks --no-claude-config >> "$LOG" 2>&1
  status_init=$?
  sovereign project refresh --quiet >> "$LOG" 2>&1
  status_refresh=$?
  printf "[%s] done — init=%d refresh=%d\n\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$status_init" "$status_refresh" >> "$LOG"
}} </dev/null &
"#,
            marker = SOVEREIGN_HOOK_MARKER
        )
    };

    if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path)?;

        if existing.contains(SOVEREIGN_HOOK_MARKER) {
            // Already on the current version — idempotent no-op.
            return Ok(());
        }

        if existing.contains("sovereign") && existing.contains("project refresh") {
            // Prior-version hook present; rewrite it by stripping the
            // Sovereign block and appending the new one. The "prior block"
            // is everything from the `# Sovereign: refresh` comment to the
            // first blank line after the closing `fi`.
            let rewritten = strip_prior_sovereign_block(&existing);
            let mut content = rewritten;
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&hook_block);
            std::fs::write(&hook_path, content)?;
        } else {
            // Foreign hook — append ours without touching theirs.
            let mut content = existing;
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push('\n');
            content.push_str(&hook_block);
            std::fs::write(&hook_path, content)?;
        }
    } else {
        let content = format!("#!/bin/sh\n{hook_block}");
        std::fs::write(&hook_path, content)?;
    }

    // Make executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    Ok(())
}

/// Remove the Sovereign-managed block from a prior-version hook so we
/// can rewrite it without clobbering user-added content. The prior block
/// starts at the `# Sovereign: refresh` comment and runs until the `fi`
/// that closes the if/elif statement (or EOF, whichever comes first).
///
/// `dead_code` allowed because `install_post_commit_hook` (the only
/// production caller) is itself deprecated; the function is still
/// exercised by `strip_prior_sovereign_block_*` tests that pin the
/// legacy-hook detection format so users running an old binary that
/// installed a V1/V2 hook can still get it cleanly removed if they ever
/// upgrade.
#[allow(dead_code)]
fn strip_prior_sovereign_block(existing: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    let mut is_v1 = false;
    let mut saw_background = false;

    for line in existing.lines() {
        let trimmed = line.trim_start();

        // Start of any Sovereign hook block: V1 used a comment without a version
        // marker; V2+ use `# SOVEREIGN_HOOK_V<N>`.
        if !inside
            && (trimmed.starts_with("# SOVEREIGN_HOOK_V")
                || trimmed.starts_with("# Sovereign: refresh"))
        {
            inside = true;
            is_v1 = trimmed.starts_with("# Sovereign: refresh");
            saw_background = false;
            continue;
        }

        if inside {
            if is_v1 {
                // V1 blocks end at `fi` on its own line.
                if trimmed == "fi" {
                    inside = false;
                }
                continue;
            } else {
                // V2+ blocks end with a background job line (`... &`) followed
                // by a blank line.  The blank line after `&` is the terminator.
                if trimmed.ends_with('&') {
                    saw_background = true;
                    continue;
                }
                if saw_background && trimmed.is_empty() {
                    inside = false;
                    continue; // consume the blank terminator line
                }
                continue;
            }
        }
        out.push(line);
    }

    // Drop trailing blank lines left behind by stripping.
    while out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out.join("\n")
}

// ─── MCP check ───────────────────────────────────────────────

async fn check_mcp_server(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });

    client
        .post(url)
        .json(&init_body)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ─── Helpers ─────────────────────────────────────────────────

pub(crate) fn default_data_dir() -> Option<PathBuf> {
    // Thin wrapper around util::dirs::sovereign_indexes(), kept as an
    // `Option<PathBuf>` so existing `.or_else(default_data_dir)` call
    // sites don't need to change shape. Returns None only when the home
    // directory can't be resolved — rare, and the callers already handle
    // the fallback to `./sovereign-indexes`.
    let p = crate::util::dirs::sovereign_indexes();
    if p == PathBuf::from(".") { None } else { Some(p) }
}

fn tempfile_dir() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    let suffix = format!("sovereign-project-{}", std::process::id());
    let path = base.join(suffix);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn load_project_config(root: &Path) -> Option<serde_json::Value> {
    let path = root.join(".sovereign").join("project.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Format a Unix timestamp as a human-readable relative time.
fn format_age(unix_ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if unix_ts == 0 {
        return "unknown".to_string();
    }

    let diff = now.saturating_sub(unix_ts);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{} min ago", diff / 60)
    } else if diff < 86400 {
        format!("{} hours ago", diff / 3600)
    } else {
        format!("{} days ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_prior_removes_old_sovereign_block() {
        let existing = r#"#!/bin/sh
# existing user hook content
echo "user step"

# Sovereign: refresh call graph after commit
if [ -x "/path/to/sovereign-cli" ]; then
  "/path/to/sovereign-cli" project refresh --quiet &
elif command -v sovereign >/dev/null 2>&1; then
  sovereign project refresh --quiet &
fi
"#;
        let stripped = strip_prior_sovereign_block(existing);
        assert!(!stripped.contains("project refresh"));
        assert!(stripped.contains("echo \"user step\""));
    }

    #[test]
    fn strip_prior_no_op_when_no_sovereign_block() {
        let existing = "#!/bin/sh\necho hello\n";
        let stripped = strip_prior_sovereign_block(existing);
        assert_eq!(stripped, "#!/bin/sh\necho hello");
    }

    #[test]
    fn strip_prior_removes_v2_sovereign_block() {
        let existing = "#!/bin/sh\n# user hook\necho \"user step\"\n\n# SOVEREIGN_HOOK_V2\n# Sovereign: keep code intelligence fresh after each commit.\nLOG=\"$HOME/.sovereign/hooks.log\"\nmkdir -p \"$(dirname \"$LOG\")\"\nSOVEREIGN=\"/path/to/sovereign-cli\"\nif [ ! -x \"$SOVEREIGN\" ]; then\n  command -v sovereign >/dev/null 2>&1 || exit 0\n  SOVEREIGN=sovereign\nfi\nsetsid sh -c 'printf \"hi\" >> \"$LOG\"' < /dev/null > /dev/null 2>&1 &\n\n";
        let stripped = strip_prior_sovereign_block(existing);
        assert!(!stripped.contains("SOVEREIGN_HOOK_V2"), "V2 marker should be stripped");
        assert!(!stripped.contains("setsid"), "setsid line should be stripped");
        assert!(stripped.contains("echo \"user step\""), "user content preserved");
    }

    // ── opencode config generation ──────────────────────────────

    #[test]
    fn opencode_config_no_commonwealth() {
        let s = generate_opencode_config(9741, None, &[]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["mcp"]["servers"]["sovereign"]["url"], "http://localhost:9741/mcp");
        assert!(v.get("provider").is_none());
    }

    #[test]
    fn opencode_config_commonwealth_no_models_uses_auto_fallback() {
        let s = generate_opencode_config(9741, Some("http://localhost:9741"), &[]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["provider"]["commonwealth"]["options"]["baseURL"], "http://localhost:9741/v1");
        assert!(v["provider"]["commonwealth"]["models"]["auto"].is_object());
    }

    #[test]
    fn opencode_config_commonwealth_real_models() {
        let models = vec!["Qwen3-9B".into(), "Qwen3-27B".into()];
        let s = generate_opencode_config(9741, Some("http://localhost:9741"), &models);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let m = &v["provider"]["commonwealth"]["models"];
        assert!(m["Qwen3-9B"].is_object());
        assert!(m["Qwen3-27B"].is_object());
        assert!(m.get("auto").is_none(), "auto should not appear when real models are known");
    }

    #[test]
    fn opencode_config_normalizes_internal_port() {
        // Port 9742 (internal mesh) should be rewritten to 9741 (public API)
        let s = generate_opencode_config(9741, Some("http://localhost:9742"), &[]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["provider"]["commonwealth"]["options"]["baseURL"], "http://localhost:9741/v1");
    }

    #[test]
    fn merge_opencode_adds_commonwealth_provider() {
        let existing = r#"{"mcp":{"servers":{"sovereign":{"type":"http","url":"http://localhost:9741/mcp"}}}}"#;
        let generated =
            generate_opencode_config(9741, Some("http://localhost:9741"), &[]);
        let merged = merge_opencode_config(existing, &generated);
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert!(v["provider"]["commonwealth"]["options"]["baseURL"].is_string());
    }

    #[test]
    fn merge_opencode_preserves_other_providers() {
        let existing = r#"{"mcp":{"servers":{}},"provider":{"openai":{"name":"OpenAI","options":{"baseURL":"https://api.openai.com/v1"}}}}"#;
        let generated =
            generate_opencode_config(9741, Some("http://localhost:9741"), &[]);
        let merged = merge_opencode_config(existing, &generated);
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert!(v["provider"]["openai"].is_object(), "pre-existing provider must survive");
        assert!(v["provider"]["commonwealth"].is_object(), "commonwealth must be added");
    }

    #[test]
    fn merge_opencode_no_commonwealth_in_generated_leaves_existing_provider_intact() {
        let existing = r#"{"mcp":{"servers":{}},"provider":{"commonwealth":{"name":"old"}}}"#;
        // generate without commonwealth URL → no provider key in generated
        let generated = generate_opencode_config(9741, None, &[]);
        let merged = merge_opencode_config(existing, &generated);
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        // existing commonwealth entry should be preserved unchanged
        assert_eq!(v["provider"]["commonwealth"]["name"], "old");
    }

    #[test]
    fn agents_md_includes_inference_section_when_commonwealth_configured() {
        let md = generate_agents_md("myproject", 9741, true, Some("http://localhost:9741"));
        assert!(md.contains("Commonwealth mesh provider"));
        assert!(md.contains("commonwealth/<model-id>"));
    }

    #[test]
    fn agents_md_no_inference_section_without_commonwealth() {
        let md = generate_agents_md("myproject", 9741, true, None);
        assert!(!md.contains("Commonwealth mesh provider"));
    }

    // ── git hook helpers ─────────────────────────────────────────

    #[tokio::test]
    async fn snapshot_graph_mtimes_tracks_files() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus_dir = tmp.path().join("test-corpus");
        std::fs::create_dir(&corpus_dir).unwrap();
        let graph_path = corpus_dir.join("scip_graph.db");
        std::fs::write(&graph_path, b"stub").unwrap();

        let snap = snapshot_graph_mtimes(tmp.path());
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key(&graph_path));

        // Empty dir → empty snapshot.
        let empty_tmp = tempfile::tempdir().unwrap();
        let empty_snap = snapshot_graph_mtimes(empty_tmp.path());
        assert!(empty_snap.is_empty());
    }
}
