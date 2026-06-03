//! `sovereign project design` — the agent-collaborative main event.
//!
//! ## Role in the ATOS onboarding flow
//!
//! `cmd_init` scaffolds the project; `cmd_design` is where the
//! user + agent iterate on DESIGN.md together. Running against the
//! already-setup Commonwealth daemon, the session either:
//!
//! - **launches opencode** with the session brief primed, which is
//!   the blessed path — the Commonwealth inference pipeline injects
//!   project_context on every turn, tool-use is native, and the
//!   conversation UX is already polished. This is the default when
//!   opencode is installed and configured.
//! - **runs the embedded stopgap chat loop** (provisional, explicitly
//!   labelled as such) for users who don't yet have opencode. Always
//!   surfaces the one-command path to opencode.
//! - **falls back to solo mode** — no agent, just the structural
//!   [`DesignSignals`] extractor driving CLI prompts, one per
//!   detected gap. Slow but works when the daemon is down.
//!
//! ## What this module owns
//!
//! Preflight (daemon health, opencode readiness, git re-prompt),
//! session bootstrap (the `.sovereign/.atos/design/<id>/` tree with
//! brief.md + state.json + transcript.jsonl), and transport
//! dispatch. Each transport is a sibling module whose MVP shape is
//! tightly scoped — anything that grows should be pulled out under
//! its own doc + tests.
//!
//! ## What this module does NOT own
//!
//! - Writing DESIGN.md content (that's [`crate::design_onboarding`]).
//! - Parsing DESIGN.md (that's [`corpus_engine_atos::design_signals`]).
//! - Composing OPEN_QUESTIONS.md or IMPLEMENTATION_PLAN.md (solo
//!   mode does this inline today; step 5/6 registers the same
//!   operations as MCP tools so the agent can call them too).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::design_onboarding::{self, OnboardOutcome};
use corpus_engine_atos::design_signals::{self, GapMarker, GapReason};

// ─── Public surface ────────────────────────────────────────────────

/// Which transport the user asked for. `Default` runs preflight and
/// falls through to the best available: opencode > stopgap > solo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportChoice {
    /// No flag — let preflight pick.
    Default,
    /// `--via opencode` — insist on opencode.
    Opencode,
    /// `--via claude-code` — insist on claude-code.
    ClaudeCode,
    /// `--stopgap` — embedded CLI chat loop. Provisional.
    Stopgap,
    /// `--solo` — no agent, structural-parser Q&A only.
    Solo,
}

/// Everything `cmd_design` accumulated from the CLI. Captured in a
/// struct so the session driver's internal code paths can't
/// accidentally ignore a user intent.
#[derive(Debug, Clone)]
pub struct SessionRequest {
    pub repo_root: PathBuf,
    pub transport: TransportChoice,
    /// `--import <path>`: copy this file into `<repo>/DESIGN.md` before
    /// starting the session. `None` means "use the DESIGN.md already
    /// at repo root, or drop a fresh template if none is there."
    pub import_path: Option<PathBuf>,
    /// Port the Commonwealth daemon is serving at. Default 9741 —
    /// matches `sovereign setup`'s canonical port.
    pub daemon_port: u16,
    /// Project id, used for session-dir naming and the brief.
    pub project_id: String,
}

/// The high-level entry point called by `cmd_design`. Preflight,
/// bootstrap, transport.
pub async fn run(req: SessionRequest) -> i32 {
    // ── Onboarding: make sure DESIGN.md exists ─────────────────────
    let design_outcome = resolve_design_doc(&req);
    let design_path = match &design_outcome {
        OnboardOutcome::Imported { written } | OnboardOutcome::TemplateDropped { written } => {
            written.clone()
        }
        OnboardOutcome::PreservedExisting { path } => path.clone(),
        OnboardOutcome::Cancelled => {
            eprintln!("  \u{2717} design session aborted — no DESIGN.md to work against.");
            return 1;
        }
    };

    // Personalize the template's H1 on first write (no-op when the
    // user's already edited it). Safe to call on imported docs —
    // idempotent when the `<project>` placeholder isn't present.
    if matches!(design_outcome, OnboardOutcome::TemplateDropped { .. }) {
        design_onboarding::personalize_template_in_place(&design_path, &req.project_id);
    }

    // ── Preflight ──────────────────────────────────────────────────
    let preflight = match req.transport {
        TransportChoice::Solo => {
            // Solo explicitly skips daemon/opencode checks.
            PreflightResult {
                daemon: DaemonState::Skipped,
                opencode: OpencodeState::Skipped,
                chosen_transport: TransportChoice::Solo,
            }
        }
        _ => match preflight(&req).await {
            Ok(r) => r,
            Err(code) => return code,
        },
    };

    // ── Session bootstrap ──────────────────────────────────────────
    let session = match bootstrap_session(&req, &design_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  \u{2717} could not bootstrap session: {e}");
            return 1;
        }
    };
    eprintln!(
        "  \u{2713} session {} bootstrapped at {}",
        session.id,
        session.dir.display()
    );

    // ── Dispatch ───────────────────────────────────────────────────
    match preflight.chosen_transport {
        TransportChoice::Opencode => run_opencode(&req, &session),
        TransportChoice::Stopgap => run_stopgap(&req, &session),
        TransportChoice::Solo => run_solo(&req, &session, &design_path).await,
        TransportChoice::ClaudeCode => {
            eprintln!(
                "  \u{2026} claude-code transport not yet implemented. Use --via opencode or --solo."
            );
            1
        }
        TransportChoice::Default => {
            unreachable!("preflight must resolve Default to a concrete transport")
        }
    }
}

// ─── Onboarding resolution ─────────────────────────────────────────

fn resolve_design_doc(req: &SessionRequest) -> OnboardOutcome {
    if let Some(src) = &req.import_path {
        return design_onboarding::import_design(&req.repo_root, src);
    }
    // No --import: either there's already a DESIGN.md (honor it) or
    // we drop the minimal template (and the session will walk through
    // its gaps).
    let target = design_onboarding::design_path(&req.repo_root);
    if target.exists() {
        OnboardOutcome::PreservedExisting { path: target }
    } else {
        design_onboarding::ensure_template(&req.repo_root)
    }
}

// ─── Preflight ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonState {
    Up,
    Down { reason: String },
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OpencodeState {
    /// Binary on PATH, config present, plugin installed.
    Ready,
    /// Binary missing.
    BinaryMissing,
    /// Binary present, but the ATOS plugin file isn't in the project
    /// tree — user needs to run `sovereign atos install-plugin`.
    PluginMissing,
    /// User never intended to use opencode (--stopgap / --solo).
    Skipped,
}

struct PreflightResult {
    #[allow(dead_code)]
    daemon: DaemonState,
    #[allow(dead_code)]
    opencode: OpencodeState,
    chosen_transport: TransportChoice,
}

async fn preflight(req: &SessionRequest) -> Result<PreflightResult, i32> {
    // 1. Daemon health — single HTTP GET with a tight timeout. The
    //    preflight must be fast (< 500ms) to keep `project design`
    //    feeling snappy. A long hang at session start would kill
    //    the magic.
    let daemon = probe_daemon(req.daemon_port).await;
    if let DaemonState::Down { reason } = &daemon {
        eprintln!();
        eprintln!(
            "  \u{2717} Commonwealth daemon isn't responding at http://localhost:{}.",
            req.daemon_port
        );
        eprintln!("    ({reason})");
        eprintln!();
        eprintln!("    Start it with:    commonwealth daemon start");
        eprintln!("    Or run `sovereign project design --solo` to continue without the agent.");
        return Err(2);
    }
    eprintln!(
        "  \u{2713} Commonwealth daemon up at http://localhost:{}",
        req.daemon_port
    );

    // 2. opencode readiness — only when we're planning to use it.
    let opencode = match req.transport {
        TransportChoice::Opencode | TransportChoice::Default => probe_opencode(&req.repo_root),
        _ => OpencodeState::Skipped,
    };

    // 3. Transport selection:
    //    - explicit --via opencode: opencode must be Ready; otherwise exit with guidance.
    //    - explicit --stopgap / --solo: honor.
    //    - Default: prefer opencode if Ready, else fall back to stopgap with a clear note.
    let chosen = match req.transport {
        TransportChoice::Opencode => {
            if opencode == OpencodeState::Ready {
                TransportChoice::Opencode
            } else {
                explain_opencode_gap(&opencode);
                return Err(2);
            }
        }
        TransportChoice::Stopgap => TransportChoice::Stopgap,
        TransportChoice::Solo => TransportChoice::Solo,
        TransportChoice::ClaudeCode => TransportChoice::ClaudeCode,
        TransportChoice::Default => {
            if opencode == OpencodeState::Ready {
                TransportChoice::Opencode
            } else {
                eprintln!();
                eprintln!("  \u{2026} opencode isn't fully set up — falling back to the provisional stopgap.");
                eprintln!("    For the full experience, fix the gap below and re-run `sovereign project design`.");
                explain_opencode_gap(&opencode);
                TransportChoice::Stopgap
            }
        }
    };

    Ok(PreflightResult {
        daemon,
        opencode,
        chosen_transport: chosen,
    })
}

async fn probe_daemon(port: u16) -> DaemonState {
    // Async reqwest with a tight timeout. The preflight must be fast
    // (< 500ms) to keep `project design` feeling snappy. A long hang
    // at session start would kill the magic. Using the async client
    // here (vs `reqwest::blocking`) keeps us feature-flag-compatible
    // with the workspace-wide reqwest configuration.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return DaemonState::Down {
                reason: format!("reqwest client: {e}"),
            }
        }
    };
    let url = format!("http://localhost:{port}/v1/models");
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => DaemonState::Up,
        Ok(r) => DaemonState::Down {
            reason: format!("HTTP {}", r.status()),
        },
        Err(e) => DaemonState::Down {
            reason: format!("{e}"),
        },
    }
}

fn probe_opencode(repo_root: &Path) -> OpencodeState {
    // Step 1: binary on PATH. `which::which` returns Err if not
    // found; we don't need the path itself — presence is enough.
    if which::which("opencode").is_err() {
        return OpencodeState::BinaryMissing;
    }
    // Step 2: plugin installed in this repo. The ATOS plugin file
    // path convention lives in atos_plugin.rs (same crate); check
    // the canonical location. Plugin version parity (matches CLI
    // version) is a nice-to-have that belongs in a later pass —
    // today just checking presence is a meaningful upgrade over
    // "no check at all" without the rabbit hole of parsing the
    // `// sovereign-atos-version:` header.
    let plugin_path = repo_root
        .join(".opencode")
        .join("plugins")
        .join("sovereign-atos.ts");
    if !plugin_path.exists() {
        return OpencodeState::PluginMissing;
    }
    OpencodeState::Ready
}

fn explain_opencode_gap(state: &OpencodeState) {
    match state {
        OpencodeState::BinaryMissing => {
            eprintln!();
            eprintln!("    opencode isn't on your PATH. Two options:");
            eprintln!("      1. Install opencode: https://opencode.ai (blessed path)");
            eprintln!("      2. `sovereign project design --stopgap` for a provisional CLI chat");
            eprintln!("         (you'll see a banner reminding you to install opencode later)");
        }
        OpencodeState::PluginMissing => {
            eprintln!();
            eprintln!("    opencode is installed but the ATOS plugin isn't in this repo.");
            eprintln!("    Run this once, then re-try design:");
            eprintln!();
            eprintln!("      sovereign atos install-plugin");
        }
        OpencodeState::Ready | OpencodeState::Skipped => {}
    }
}

// ─── Session bootstrap ─────────────────────────────────────────────

pub struct BootstrappedSession {
    pub id: String,
    pub dir: PathBuf,
    pub brief_path: PathBuf,
    #[allow(dead_code)]
    pub state_path: PathBuf,
    #[allow(dead_code)]
    pub transcript_path: PathBuf,
}

fn bootstrap_session(
    req: &SessionRequest,
    design_path: &Path,
) -> std::io::Result<BootstrappedSession> {
    let id = format!("design-{}", unix_now_secs());
    let dir = req
        .repo_root
        .join(".sovereign")
        .join(".atos")
        .join("design")
        .join(&id);
    fs::create_dir_all(&dir)?;

    let brief_path = dir.join("brief.md");
    let state_path = dir.join("state.json");
    let transcript_path = dir.join("transcript.jsonl");

    let brief = render_session_brief(req, design_path);
    fs::write(&brief_path, brief)?;

    let state = render_state_json(&id, req, design_path);
    fs::write(&state_path, state)?;

    // Create an empty transcript so downstream writes can `append`
    // without a first-time open/create race.
    fs::write(&transcript_path, "")?;

    Ok(BootstrappedSession {
        id,
        dir,
        brief_path,
        state_path,
        transcript_path,
    })
}

fn render_session_brief(req: &SessionRequest, design_path: &Path) -> String {
    // The exact brief shape from the plan, rendered with the current
    // DESIGN.md hash + signals snapshot so the agent has concrete
    // evidence the user can point at. The pipeline injects this brief
    // on every turn via project_context.
    let design_text = fs::read_to_string(design_path).unwrap_or_default();
    let design_hash = short_hash(&design_text);
    let signals = design_signals::extract(&design_text);

    let anchors_block = if signals.anchors.is_empty() {
        "_(none yet)_".to_string()
    } else {
        signals
            .anchors
            .iter()
            .map(|a| format!("  - {}", a.text))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let gaps_block = if signals.gaps.is_empty() {
        "_(none — the doc is either fully specified or nearly empty)_".to_string()
    } else {
        signals
            .gaps
            .iter()
            .take(12)
            .map(|g| {
                format!(
                    "  - [{:?}] §{} · {}",
                    g.reason,
                    g.section,
                    truncate(&g.snippet, 80)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"# Design collaboration session

You are collaborating with the user on DESIGN.md for the project at
`{repo}`. Your goal is to help them produce a DESIGN.md they're
comfortable with, an OPEN_QUESTIONS.md of load-bearing gaps, and
eventually an IMPLEMENTATION_PLAN.md — via tools, not by generating
all content yourself.

## Principles (non-negotiable)

- **Humble.** Ask at most one substantive question at a time.
- **Situated.** Cite the user's own words back to them. Don't
  introduce generic fault-lines unless the design text supports
  them.
- **Antifragile.** A question is worth asking only if answering it
  makes the design stronger. If you'd write a placeholder, don't
  ask.
- **Free-form body.** The user's `## Anchors` block is structured;
  everything else can be as messy as they want.

## Current state

- DESIGN.md: `{design_rel}` (sha: `{design_hash}`)
- Anchors currently listed:
{anchors_block}
- Structural gaps the parser already saw ({gap_count} total, first 12 shown):
{gaps_block}

## Tools available to you

- `design_signals_extract(path)` — run after each substantive edit
  to see what gaps remain.
- `design_open_questions_write(questions)` — append to
  OPEN_QUESTIONS.md. IDs are `oq.<section-slug>.<nth>`; never
  overwrite an answered entry.
- `design_plan_generate()` — produce IMPLEMENTATION_PLAN.md when
  the user says they're ready. Refuses if unanswered blocking OQs
  remain.
- `project_context(query)` — search indexed project docs.
- `read_notes(...)` / `write_note(...)` — durable session notes.

## Stop conditions

- User types `/done` or `/plan` → generate plan and exit.
- User types `/solo` → exit; they'll continue in their editor.
- No user input for 15 min → emit a final summary note and exit idle.

## Session id

`{session_hint}` — use this as the `session_id` field on any note
you persist via `write_note`.
"#,
        repo = req.repo_root.display(),
        design_rel = design_path
            .strip_prefix(&req.repo_root)
            .unwrap_or(design_path)
            .display(),
        design_hash = design_hash,
        anchors_block = anchors_block,
        gap_count = signals.gaps.len(),
        gaps_block = gaps_block,
        session_hint = req.project_id,
    )
}

fn render_state_json(id: &str, req: &SessionRequest, design_path: &Path) -> String {
    // Plain hand-written JSON to keep the surface dependency-free.
    // Keys match the state shape in the plan (session_id, started_at,
    // design_path, prior_hash, status, transport). Kept small; the
    // richer state lives in transcript.jsonl as an append stream.
    let design_text = fs::read_to_string(design_path).unwrap_or_default();
    let hash = short_hash(&design_text);
    format!(
        "{{\n  \"session_id\": {:?},\n  \"started_at\": {},\n  \"design_path\": {:?},\n  \"prior_hash\": {:?},\n  \"status\": \"active\",\n  \"transport\": {:?},\n  \"project_id\": {:?}\n}}\n",
        id,
        unix_now_secs(),
        design_path.display().to_string(),
        hash,
        match req.transport {
            TransportChoice::Opencode | TransportChoice::Default => "opencode",
            TransportChoice::Stopgap => "stopgap",
            TransportChoice::ClaudeCode => "claude-code",
            TransportChoice::Solo => "solo",
        },
        req.project_id,
    )
}

// ─── Transport: opencode ───────────────────────────────────────────

fn run_opencode(req: &SessionRequest, session: &BootstrappedSession) -> i32 {
    // The blessed path: spawn opencode with the brief primed via env
    // so the plugin's `X-Feature-Id` tagging lines up with the session
    // id. We exec the child and wait; opencode takes over the TTY.
    // On exit, we print a summary line so the user knows how to
    // continue (`sovereign project plan`).
    eprintln!();
    eprintln!("  Launching opencode — session {}.", session.id);
    eprintln!(
        "    brief: {}",
        session
            .brief_path
            .strip_prefix(&req.repo_root)
            .unwrap_or(&session.brief_path)
            .display()
    );
    eprintln!();

    let status = std::process::Command::new("opencode")
        .current_dir(&req.repo_root)
        .env("SOVEREIGN_SESSION_ID", &session.id)
        .env("SOVEREIGN_FEATURE_ID", format!("design-{}", req.project_id))
        .env("SOVEREIGN_BRIEF_PATH", &session.brief_path)
        .status();

    match status {
        Ok(s) if s.success() => {
            eprintln!();
            eprintln!(
                "  \u{2713} Session {} closed. Run `sovereign project plan` to turn answered",
                session.id
            );
            eprintln!("    OPEN_QUESTIONS.md entries into IMPLEMENTATION_PLAN.md.");
            0
        }
        Ok(s) => {
            eprintln!("  \u{2717} opencode exited with status {s}");
            1
        }
        Err(e) => {
            eprintln!("  \u{2717} could not spawn opencode: {e}");
            eprintln!(
                "    This usually means opencode isn't on PATH — try `--stopgap` or `--solo`."
            );
            1
        }
    }
}

// ─── Transport: stopgap ────────────────────────────────────────────

fn run_stopgap(_req: &SessionRequest, session: &BootstrappedSession) -> i32 {
    // MVP placeholder. The full embedded streaming chat loop is a
    // material pass of its own — streaming HTTP, tool-call plumbing
    // through the 24-tool registry, slash commands, per-patch diff
    // prompts. Until it lands, the honest move is to tell the user
    // exactly that, so they don't think the session already started
    // but silently did nothing.
    //
    // Every line here respects the "push toward opencode ASAP"
    // invariant from the plan.
    eprintln!();
    eprintln!(
        "  \u{26a0} `--stopgap` is provisional and its embedded chat loop hasn't landed yet."
    );
    eprintln!();
    eprintln!("    Your session is real — brief + state written at:");
    eprintln!("      {}", session.dir.display());
    eprintln!();
    eprintln!("    Until the stopgap ships, your options are:");
    eprintln!(
        "      \u{00b7} Install opencode, then re-run `sovereign project design` (blessed path)."
    );
    eprintln!("      \u{00b7} Run `sovereign project design --solo` for structural-parser-driven");
    eprintln!("        CLI prompts against your DESIGN.md (no agent, but real gaps get captured).");
    eprintln!();
    eprintln!("    — stopgap mode · `sovereign project design --via opencode` when you're ready —");
    2
}

// ─── Transport: solo (no agent) ────────────────────────────────────

async fn run_solo(req: &SessionRequest, session: &BootstrappedSession, design_path: &Path) -> i32 {
    eprintln!();
    eprintln!("  Solo mode — no agent. I'll walk each gap the structural parser saw.");
    eprintln!(
        "    Answers land in `{}/OPEN_QUESTIONS.md` at repo root (append-only).",
        req.repo_root.display()
    );

    let design_text = match fs::read_to_string(design_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("  \u{2717} could not read {}: {e}", design_path.display());
            return 1;
        }
    };
    let signals = design_signals::extract(&design_text);

    if signals.gaps.is_empty() {
        eprintln!();
        eprintln!("  \u{2713} No structural gaps found. DESIGN.md looks complete to the parser.");
        eprintln!("    Next: `sovereign project plan` when you're ready to draft the phase plan.");
        return 0;
    }

    eprintln!();
    eprintln!("  {} gap(s) detected.", signals.gaps.len());

    let open_questions_path = req.repo_root.join("OPEN_QUESTIONS.md");
    let answers = prompt_for_gaps(&signals.gaps);
    if answers.is_empty() {
        eprintln!();
        eprintln!("  \u{2026} No answers captured. Nothing written.");
        return 0;
    }

    if let Err(e) = append_open_questions(&open_questions_path, &answers, &session.id) {
        eprintln!("  \u{2717} could not write OPEN_QUESTIONS.md: {e}");
        return 1;
    }

    eprintln!();
    eprintln!(
        "  \u{2713} {} open question(s) recorded at {}.",
        answers.len(),
        open_questions_path
            .strip_prefix(&req.repo_root)
            .unwrap_or(&open_questions_path)
            .display()
    );
    eprintln!("    Next: `sovereign project plan` to fold answers into IMPLEMENTATION_PLAN.md.");
    0
}

/// A captured solo-mode answer. Same shape `design_open_questions_write`
/// will take when step 5 registers it as an MCP tool — keeps the
/// surfaces aligned.
#[derive(Debug, Clone)]
struct SoloAnswer {
    id: String,
    question: String,
    anchor: String,
    answer: String,
}

fn prompt_for_gaps(gaps: &[GapMarker]) -> Vec<SoloAnswer> {
    let mut answers = Vec::new();
    // Stable numbering per-section, to mirror the agent-path
    // `oq.<slug>.<n>` scheme.
    let mut per_section: std::collections::BTreeMap<String, usize> = Default::default();
    for gap in gaps {
        let slug = slugify(&gap.section);
        let counter = per_section.entry(slug.clone()).or_insert(0);
        *counter += 1;
        let id = format!("oq.{slug}.{counter}");

        eprintln!();
        eprintln!("  ── {} ────────────────────────────────────────", id);
        eprintln!("    Anchor: DESIGN.md §{}", gap.section);
        let synth_question = synthesize_question(gap);
        eprintln!("    Q: {synth_question}");
        eprintln!(
            "       (context: {:?}{})",
            gap.reason,
            format_snippet(&gap.snippet)
        );
        eprint!("    A (blank = skip): ");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let answer = crate::util::prompts::prompt_string("").unwrap_or_default();
        let trimmed = answer.trim();
        if trimmed.is_empty() {
            continue;
        }
        answers.push(SoloAnswer {
            id,
            question: synth_question,
            anchor: format!("DESIGN.md §{}", gap.section),
            answer: trimmed.to_string(),
        });
    }
    answers
}

fn synthesize_question(gap: &GapMarker) -> String {
    match gap.reason {
        GapReason::TbdMarker => format!("Resolve this TBD: {}", gap.snippet.trim()),
        GapReason::UnclearMarker => {
            format!(
                "You marked this unclear — what do you want it to say?: {}",
                gap.snippet.trim()
            )
        }
        GapReason::EmptySection => format!(
            "Section `{}` is empty — what belongs here? (Or write `skip` if it's intentional.)",
            gap.section
        ),
        GapReason::OpenChoice => format!(
            "You wrote an X-vs-Y choice without resolving it: {} — which side, and why?",
            gap.snippet.trim()
        ),
        GapReason::LiteralQuestion => format!(
            "You asked a question inline: {} — what's your current best answer?",
            gap.snippet.trim()
        ),
    }
}

fn format_snippet(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        format!(" — {}", truncate(s, 120))
    }
}

// ─── OPEN_QUESTIONS.md writer ──────────────────────────────────────

fn append_open_questions(
    path: &Path,
    answers: &[SoloAnswer],
    session_id: &str,
) -> std::io::Result<()> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut out = if existing.trim().is_empty() {
        open_questions_header().to_string()
    } else {
        // Keep existing content unchanged. Append-only — matches the
        // antifragile property from the plan.
        let mut s = existing;
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s
    };
    for a in answers {
        out.push_str("\n---\n\n");
        out.push_str(&format!("### {}\n", a.id));
        out.push_str(&format!("**Q:** {}\n", a.question));
        out.push_str(&format!("**Anchor:** {}\n", a.anchor));
        out.push_str(&format!("**Answer:**\n{}\n\n", a.answer));
        out.push_str(&format!(
            "_Captured by `sovereign project design --solo` · session `{}` · {}_\n",
            session_id,
            iso_date_utc(unix_now_secs())
        ));
    }
    fs::write(path, out)
}

fn open_questions_header() -> &'static str {
    "# Open questions\n\nEach question below is a load-bearing gap in DESIGN.md. Answers are\nappend-only — never edit DESIGN.md directly to hide a gap, because the\nlog of resolutions *is* the provenance. Run `sovereign project plan` when\nyou're ready to fold answered questions into the implementation plan.\n"
}

// ─── Small helpers ─────────────────────────────────────────────────

fn unix_now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn iso_date_utc(secs: i64) -> String {
    // Same civil-from-days algorithm used elsewhere in project_cmd.rs.
    // Duplicated locally to avoid coupling cmd_design to a helper we
    // may want to move. Correct for the test date 2026-04-22.
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn short_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    let out = h.finalize();
    hex_short(&out)
}

fn hex_short(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(12);
    for b in bytes.iter().take(6) {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("section");
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_handles_typical_headings() {
        assert_eq!(slugify("Data & interfaces"), "data-interfaces");
        assert_eq!(slugify("Anchors"), "anchors");
        assert_eq!(slugify("What we're building"), "what-we-re-building");
        assert_eq!(slugify("  "), "section");
    }

    #[test]
    fn synthesize_question_uses_specific_wording_per_reason() {
        let make = |reason, snippet: &str, section: &str| GapMarker {
            section: section.into(),
            snippet: snippet.into(),
            reason,
            line: 1,
        };
        let tbd = synthesize_question(&make(GapReason::TbdMarker, "wire format", "Data"));
        assert!(tbd.to_lowercase().contains("tbd"));
        let empty = synthesize_question(&make(GapReason::EmptySection, "", "Data"));
        assert!(empty.contains("empty"));
        let choice = synthesize_question(&make(GapReason::OpenChoice, "A vs B", "Schema"));
        assert!(choice.contains("A vs B"));
    }

    #[test]
    fn open_questions_file_is_written_with_header_when_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("OPEN_QUESTIONS.md");
        let answers = vec![SoloAnswer {
            id: "oq.data-interfaces.1".into(),
            question: "What goes here?".into(),
            anchor: "DESIGN.md §Data & interfaces".into(),
            answer: "We use gRPC.".into(),
        }];
        append_open_questions(&path, &answers, "design-123").unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("# Open questions"));
        assert!(contents.contains("oq.data-interfaces.1"));
        assert!(contents.contains("We use gRPC."));
        assert!(contents.contains("design-123"));
    }

    #[test]
    fn open_questions_append_preserves_existing_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("OPEN_QUESTIONS.md");
        fs::write(&path, "# Open questions\n\n(existing content)\n").unwrap();
        let answers = vec![SoloAnswer {
            id: "oq.anchors.1".into(),
            question: "What's the primary persistence?".into(),
            anchor: "DESIGN.md §Anchors".into(),
            answer: "sqlite".into(),
        }];
        append_open_questions(&path, &answers, "s").unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("(existing content)"),
            "existing content must be preserved on append"
        );
        assert!(contents.contains("sqlite"));
    }

    #[test]
    fn render_session_brief_includes_hash_and_gaps() {
        let tmp = tempfile::tempdir().unwrap();
        let design = tmp.path().join("DESIGN.md");
        fs::write(
            &design,
            "# P\n\n## Anchors\n\n- Primary persistence: sqlite\n\n## Plan\n\nTBD: wire format\n",
        )
        .unwrap();
        let req = SessionRequest {
            repo_root: tmp.path().to_path_buf(),
            transport: TransportChoice::Default,
            import_path: None,
            daemon_port: 9741,
            project_id: "probe".into(),
        };
        let brief = render_session_brief(&req, &design);
        assert!(brief.contains("Primary persistence: sqlite"));
        assert!(brief.contains("sha:"));
        // First 12 chars of a sha256 is not accidentally zero-length.
        assert!(brief.contains("[TbdMarker]"));
        assert!(
            brief.contains("`probe`"),
            "session brief mentions project_id"
        );
    }

    #[test]
    fn render_session_brief_handles_fully_specified_doc() {
        let tmp = tempfile::tempdir().unwrap();
        let design = tmp.path().join("DESIGN.md");
        fs::write(
            &design,
            "# P — Design\n\n## Anchors\n\n- Primary persistence: sqlite\n- Primary interface: CLI\n- Language: Rust\n\n## Plan\n\nbody body body body body.\n",
        )
        .unwrap();
        let req = SessionRequest {
            repo_root: tmp.path().to_path_buf(),
            transport: TransportChoice::Default,
            import_path: None,
            daemon_port: 9741,
            project_id: "p".into(),
        };
        let brief = render_session_brief(&req, &design);
        assert!(brief.contains("Primary persistence: sqlite"));
    }

    #[test]
    fn bootstrap_session_creates_expected_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let design = tmp.path().join("DESIGN.md");
        fs::write(&design, "# X\n").unwrap();
        let req = SessionRequest {
            repo_root: tmp.path().to_path_buf(),
            transport: TransportChoice::Solo,
            import_path: None,
            daemon_port: 9741,
            project_id: "x".into(),
        };
        let session = bootstrap_session(&req, &design).unwrap();
        assert!(session.dir.exists());
        assert!(session.brief_path.exists());
        assert!(session.state_path.exists());
        assert!(session.transcript_path.exists());
        // Session id namespace is recognisable.
        assert!(session.id.starts_with("design-"));
        // state.json mentions the transport and design path.
        let state = fs::read_to_string(&session.state_path).unwrap();
        assert!(state.contains("\"transport\": \"solo\""));
        assert!(state.contains("DESIGN.md"));
    }
}
