// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn setup --fim` — one-command inline-completion onboarding.
//!
//! The manual flow this replaces is seven steps in
//! `packages/vscode-sovereign/README.md`: download a coder GGUF,
//! download an embed GGUF, hand-write a `[models.fim]` block, restart
//! the daemon, curl a completion, build a `.vsix`, install it. Every
//! step has a failure mode that surfaces minutes later as "ghost text
//! doesn't appear", with nothing to tell the operator which rung
//! broke. This module runs the same seven steps and *verifies* them,
//! stopping at the first failure with the fix.
//!
//! Two design commitments, both load-bearing:
//!
//! **Consent before mutation.** [`build_plan`] decides everything —
//! model, quant, paths, which config keys change, what gets backed up
//! — and [`print_plan`] shows it, before a single byte is downloaded
//! or written. `--yes` skips the prompt, not the plan.
//!
//! **Lean mode.** `[models].primary` and `[models.fim].path` are set
//! to the SAME file, so `ModelsSection::fast_path()` (which falls
//! back to `primary` when `fast` is unset) equals the FIM path and
//! `EmbeddedLlamaCpp::install_fim_slot` takes its alias branch — one
//! copy of Mellum2 in RAM serving both chat and completions. The
//! alternative, a dedicated pinned `fim` slot beside a separate chat
//! primary, needs 7–13 GB of headroom on top of the primary; the
//! `high` (16.3 GB primary, 20–23 GB band) and `very_high` (20.5 GB
//! primary, ≥24 GB band) tiers have ~3.5 GB. That is measured against
//! `models.toml`, not assumed — see the FIM LADDER block there.

use std::io::{self, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sovereign_core::models_manifest::SlotConfig;
use sovereign_inference::hardware::{self, HardwareProfile, ProfileName};
use sovereign_inference::setup_planner::{
    fim_rung_for_profile, fim_slot_for_rung, hf_download_url, next_fim_rung, resolve_slot, SlotKind,
};

use crate::setup_config::{
    DaemonSection, DataSection, FimSection, ModelsSection, SetupConfig,
};

use super::download::{download_silent, download_with_progress};
use super::Opts;

/// How long to wait for the daemon to answer after a restart. A cold
/// 7–13 GB mmap warmup is 30–60s on an SSD and materially longer on
/// a spinning disk or a cold page cache; the old 30s ceiling from
/// `finish::wait_for_daemon` would report a false failure on exactly
/// the machines this command targets.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(300);

/// Round-trip budget for the synthetic completion. Generous because
/// the first completion after a restart also pays prompt prefill on
/// a cold KV cache.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(90);

// ─── Plan ──────────────────────────────────────────────────────────

/// Everything decided before anything is mutated. Built by
/// [`build_plan`], printed by [`print_plan`], then executed. Keeping
/// it a value (rather than a sequence of side effects) is what makes
/// the consent screen honest: the plan the operator approves is the
/// plan that runs.
struct Plan {
    profile: ProfileName,
    /// Ladder rung name, e.g. `"q6_k"`.
    rung: String,
    /// Whether `rung` came from `--quant` rather than the hardware.
    rung_overridden: bool,
    slot: SlotConfig,
    models_dir: PathBuf,
    /// Destination for the Mellum2 GGUF — the file that becomes both
    /// `primary` and `fim.path`.
    model_path: PathBuf,
    /// Embed slot to download, `None` when a usable one already
    /// exists on disk (the daemon requires an embed model, but there
    /// is no reason to re-fetch one the operator already has).
    embed_download: Option<(SlotConfig, PathBuf)>,
    embed_path: PathBuf,
    /// The config we're about to change, when one exists.
    existing: Option<ExistingConfig>,
    config_path: PathBuf,
    backup_path: PathBuf,
    data_dir: PathBuf,
    client_port: u16,
}

/// The parts of a pre-existing config the plan needs to talk about
/// honestly: what we're replacing, and whether FIM was already set up.
struct ExistingConfig {
    primary: PathBuf,
    fim_path: Option<PathBuf>,
}

impl Plan {
    /// Total bytes we intend to fetch — `0` when everything needed is
    /// already on disk, which is the re-run case and worth saying out
    /// loud rather than implying a fresh multi-GB download.
    fn download_gb(&self) -> f64 {
        let mut gb = 0.0;
        if !file_is_present(&self.model_path) {
            gb += self.slot.size_gb;
        }
        if let Some((slot, path)) = &self.embed_download {
            if !file_is_present(path) {
                gb += slot.size_gb;
            }
        }
        gb
    }

    /// True when the config already points at exactly what we'd
    /// write. Drives "already configured" phrasing so a re-run
    /// doesn't claim credit for work it didn't do.
    fn already_lean_on_this_model(&self) -> bool {
        self.existing.as_ref().is_some_and(|e| {
            e.primary == self.model_path && e.fim_path.as_deref() == Some(self.model_path.as_path())
        })
    }
}

fn file_is_present(p: &Path) -> bool {
    p.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

// ─── Entry point ───────────────────────────────────────────────────

pub(super) async fn run_fim_setup(opts: &Opts) -> i32 {
    println!();
    println!("  Sovereign Setup — inline completion");
    println!("  {}", "─".repeat(54));
    println!();

    let plan = match build_plan(opts).await {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };

    print_plan(&plan);

    if !confirm(opts) {
        println!();
        println!("  Nothing changed.");
        return 1;
    }

    // ── Download ──────────────────────────────────────────────────
    println!();
    if let Err(e) = std::fs::create_dir_all(&plan.models_dir) {
        eprintln!("error: cannot create {}: {e}", plan.models_dir.display());
        return 1;
    }
    if let Err(code) = download_models(&plan).await {
        return code;
    }

    // ── Config ────────────────────────────────────────────────────
    println!();
    if let Err(msg) = write_config(&plan) {
        eprintln!("error: {msg}");
        return 1;
    }

    // `daemon --setup-only --fim` runs INSIDE the daemon's own
    // first-boot path — the process that would serve these probes is
    // the one waiting for us to return. Restarting or probing here
    // would deadlock against a listener that hasn't bound yet.
    if opts.wizard_only {
        println!();
        println!("  \u{2713} Config written. The daemon will load it as it boots.");
        println!();
        println!("  Verify once it's up:  svrn setup --fim --yes");
        return 0;
    }

    // ── Daemon ────────────────────────────────────────────────────
    println!();
    if let Err(msg) = bring_daemon_up(&plan).await {
        eprintln!();
        eprintln!("  \u{2717} {msg}");
        eprintln!("    Config is written and correct — this is a lifecycle problem, not a FIM one.");
        eprintln!("    Try:  svrn daemon run        # foreground, prints the load errors live");
        return 1;
    }

    // ── Verify ────────────────────────────────────────────────────
    println!();
    println!("  Verifying...");
    let verified = match verify(plan.client_port).await {
        Ok(v) => v,
        Err(msg) => {
            eprintln!();
            eprintln!("  \u{2717} {msg}");
            return 1;
        }
    };

    // ── Editor ────────────────────────────────────────────────────
    println!();
    let editor = if opts.skip_editor {
        EditorOutcome::Skipped
    } else {
        install_extension()
    };

    print_decision(&plan, &verified, &editor);
    0
}

// ─── Plan construction ─────────────────────────────────────────────

async fn build_plan(opts: &Opts) -> Result<Plan, String> {
    eprint!("  Detecting hardware... ");
    io::stderr().flush().ok();
    let hw = tokio::task::spawn_blocking(HardwareProfile::detect)
        .await
        .map_err(|e| format!("hardware detection panicked: {e}"))?;
    let profile = hardware::select_profile(&hw);
    println!(
        "{}, {:.0}GB {}memory",
        match &hw.gpu_name {
            Some(name) => name.clone(),
            None if hw.is_unified_memory => "Apple Silicon".to_string(),
            None => "CPU-only system".to_string(),
        },
        hw.system_ram_gb(),
        if hw.is_unified_memory { "unified " } else { "" }
    );

    let rung_overridden = opts.quant.is_some();
    let rung = opts
        .quant
        .clone()
        .unwrap_or_else(|| fim_rung_for_profile(&profile).to_string());
    // `parse_args` already rejected an unknown `--quant`, so a miss
    // here means the ladder and the manifest disagree — a build-time
    // bug, not operator error. Say which, so the report is useful.
    let slot = fim_slot_for_rung(&rung).ok_or_else(|| {
        format!(
            "FIM rung '{rung}' is not in the bundled manifest — \
             models.toml and setup_planner::FIM_RUNGS are out of sync"
        )
    })?;

    // Existing config decides the data dir and port; `--data-dir`
    // overrides; otherwise the standard root.
    let existing_cfg = SetupConfig::load().ok();
    let config_path = SetupConfig::default_path();
    let data_dir = opts
        .data_dir
        .clone()
        .or_else(|| existing_cfg.as_ref().map(|c| c.data.dir.clone()))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    let client_port = existing_cfg
        .as_ref()
        .map(|c| c.daemon.client_port)
        .unwrap_or_else(|| DaemonSection::default().client_port);

    let models_dir = data_dir.join("models");
    let model_path = models_dir.join(&slot.file);

    // Embed: reuse whatever the operator already has rather than
    // re-downloading. The FIM path does not use the embed slot at
    // all — the daemon simply refuses to start without one.
    let existing_embed = existing_cfg
        .as_ref()
        .map(|c| c.models.embed.clone())
        .filter(|p| file_is_present(p));
    let (embed_download, embed_path) = match existing_embed {
        Some(p) => (None, p),
        None => {
            let embed_slot = resolve_slot(&profile, SlotKind::Embed)
                .ok_or_else(|| "bundled manifest has no embed slot for this hardware".to_string())?;
            let path = models_dir.join(&embed_slot.file);
            (Some((embed_slot, path.clone())), path)
        }
    };

    let existing = existing_cfg.as_ref().map(|c| ExistingConfig {
        primary: c.models.primary.clone(),
        fim_path: c.models.fim.as_ref().map(|f| f.path.clone()),
    });

    Ok(Plan {
        profile,
        rung,
        rung_overridden,
        slot,
        models_dir,
        model_path,
        embed_download,
        embed_path,
        existing,
        backup_path: config_path.with_extension("toml.pre-fim"),
        config_path,
        data_dir,
        client_port,
    })
}

// ─── Consent ───────────────────────────────────────────────────────

fn print_plan(plan: &Plan) {
    let gb = plan.download_gb();
    println!();
    println!("  Plan — nothing has changed yet");
    println!();
    println!(
        "    Model    {} {} ({:.1} GB){}",
        plan.slot.base_name,
        plan.slot.quant,
        plan.slot.size_gb,
        if plan.rung_overridden {
            "  [--quant]"
        } else {
            ""
        }
    );
    println!("    From     {}", plan.slot.hf_url);
    println!("    To       {}", plan.model_path.display());
    println!();
    println!("    Serving  lean mode — this model becomes BOTH the chat");
    println!("             primary and the completion model, one copy in RAM.");
    match &plan.existing {
        Some(_) if plan.already_lean_on_this_model() => {
            println!();
            println!("    Config   {} is already set up this way;", plan.config_path.display());
            println!("             this run re-verifies it end to end.");
        }
        Some(e) => {
            println!();
            println!("    Config   {}", plan.config_path.display());
            println!("               primary      {} \u{2192} {}", short(&e.primary), short(&plan.model_path));
            println!("               fast         cleared (primary serves it)");
            println!(
                "               models.fim   {} \u{2192} {}",
                e.fim_path.as_deref().map(short).unwrap_or_else(|| "(unset)".into()),
                short(&plan.model_path)
            );
            println!(
                "               embed        {} ({})",
                short(&plan.embed_path),
                if plan.embed_download.is_some() {
                    "downloaded — yours is missing"
                } else {
                    "unchanged"
                }
            );
            println!("             Backed up first to {}", plan.backup_path.display());
        }
        None => {
            println!();
            println!("    Config   {} (new)", plan.config_path.display());
        }
    }
    if gb > 0.0 {
        println!();
        println!("    Download {gb:.1} GB total");
    } else {
        println!();
        println!("    Download nothing — every file is already on disk");
    }
    println!();
    println!("    Then     start (or restart) the daemon, verify a real");
    println!("             completion, install the editor extension.");
}

fn short(p: &Path) -> String {
    p.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn confirm(opts: &Opts) -> bool {
    if opts.yes {
        return true;
    }
    if !io::stdin().is_terminal() {
        eprintln!();
        eprintln!("error: not a terminal, so there's nobody to ask.");
        eprintln!("       Re-run with --yes to accept the plan above.");
        return false;
    }
    println!();
    print!("  Proceed? [Y/n] ");
    io::stdout().flush().ok();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
}

// ─── Execution ─────────────────────────────────────────────────────

async fn download_models(plan: &Plan) -> Result<(), i32> {
    println!("  Downloading...");
    println!();
    let label = format!("{} {}", plan.slot.base_name, plan.slot.quant);
    if let Err(e) = download_with_progress(
        &hf_download_url(&plan.slot),
        &plan.model_path,
        &label,
        plan.slot.size_gb,
    )
    .await
    {
        eprintln!("  \u{2717} {label}: {e}");
        eprintln!();
        eprintln!("    Nothing was written to your config — re-run when the download can succeed.");
        return Err(1);
    }

    if let Some((slot, path)) = &plan.embed_download {
        match download_silent(&hf_download_url(slot), path, slot.size_gb).await {
            Ok(()) => println!("    \u{2713} {} (embedder)", slot.file),
            Err(e) => {
                eprintln!("  \u{2717} embedder {}: {e}", slot.file);
                eprintln!();
                eprintln!("    The daemon requires an embed model to start, so this is fatal.");
                eprintln!("    Nothing was written to your config.");
                return Err(1);
            }
        }
    }
    Ok(())
}

fn write_config(plan: &Plan) -> Result<(), String> {
    let fim = FimSection {
        path: plan.model_path.clone(),
        // Defaults are deliberate, not laziness: 4096 ctx / 48 tokens
        // / temp 0.2 are the FIM defaults documented on `FimSection`,
        // and writing them explicitly would freeze today's values
        // into every user's config where a later tuning pass could
        // never reach them.
        context_size: None,
        max_tokens: None,
        temperature: None,
        max_prefix_chars: None,
        max_suffix_chars: None,
    };

    let mut cfg = match SetupConfig::load() {
        Ok(existing) => {
            // Back up BEFORE mutating. Lean mode replaces the chat
            // primary; an operator who decides tomorrow that they
            // want their 35B back must not have to reconstruct the
            // rest of their config from memory.
            std::fs::copy(&plan.config_path, &plan.backup_path).map_err(|e| {
                format!(
                    "could not back up {} to {}: {e}",
                    plan.config_path.display(),
                    plan.backup_path.display()
                )
            })?;
            println!("    \u{2713} Backed up {}", plan.backup_path.display());
            existing
        }
        Err(_) => SetupConfig {
            compute: Default::default(),
            models: ModelsSection {
                primary: plan.model_path.clone(),
                fast: None,
                embed: plan.embed_path.clone(),
                code: None,
                context_size: None,
                extra: std::collections::BTreeMap::new(),
                max_extras_memory_gb: None,
                primary_pool: None,
                fim: None,
            },
            daemon: DaemonSection::default(),
            data: DataSection {
                dir: plan.data_dir.clone(),
            },
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        },
    };

    cfg.models.primary = plan.model_path.clone();
    // Clearing `fast` is what puts the daemon in alias mode:
    // `fast_path()` falls back to `primary`, which now equals
    // `fim.path`, so `install_fim_slot` serves completions from the
    // resident fast slot instead of loading a second copy. Leaving a
    // stale `fast` here would quietly cost a whole extra model.
    cfg.models.fast = None;
    cfg.models.embed = plan.embed_path.clone();
    cfg.models.fim = Some(fim);

    let path = cfg.save()?;
    println!("    \u{2713} Wrote {}", path.display());
    Ok(())
}

/// Get the daemon running the config we just wrote.
///
/// `restart` is NOT the universal answer here. With no daemon running
/// and no service registered, `restart_daemon` = stop + start, and its
/// stop leg falls through to the service manager, fails, and returns
/// non-zero *without ever starting anything* — which is exactly the
/// fresh-install case this command is built for. So: probe first,
/// restart only what is actually up.
async fn bring_daemon_up(plan: &Plan) -> Result<(), String> {
    let running =
        daemon_answers(plan.client_port).await || crate::daemon_cmd::read_daemon_pid().is_some();
    let verb = if running { "Restarting" } else { "Starting" };
    println!(
        "  {verb} the daemon (cold load of a {:.1} GB model takes a minute)...",
        plan.slot.size_gb
    );
    // No `--rpc-worker` here: the FIM setup path is configuring THIS node's own
    // completion slot, not offering its GPU to the mesh. Whatever the operator's
    // config says about serving still applies via the role translation.
    let rc = if running {
        crate::daemon_cmd::lifecycle::restart_daemon(&[]).await
    } else {
        crate::daemon_cmd::lifecycle::start_daemon(&[]).await
    };
    if rc == 0 {
        return Ok(());
    }
    Err(format!(
        "the daemon did not come up ({} exited {rc}).",
        verb.to_lowercase()
    ))
}

/// Cheap liveness probe — is anything serving on the client port?
async fn daemon_answers(port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ─── Verification ──────────────────────────────────────────────────

/// What the three rungs proved, for the final banner.
struct Verified {
    slot: String,
    model_id: String,
    fim_style: String,
    aliased_to_fast: bool,
    sample: String,
    ttft_ms: Option<u64>,
}

/// The same ladder the extension's `Diagnose Completion Setup`
/// command walks — daemon reachable, FIM slot live, real completion
/// round-trips — run here so `--fim` cannot report success on a
/// daemon that would 503 on the first keystroke.
async fn verify(port: u16) -> Result<Verified, String> {
    let client = reqwest::Client::builder()
        .timeout(COMPLETION_TIMEOUT)
        .build()
        .map_err(|e| format!("could not build an http client: {e}"))?;
    let base = format!("http://127.0.0.1:{port}");

    // 1. Daemon reachable.
    let deadline = std::time::Instant::now() + DAEMON_READY_TIMEOUT;
    let models_url = format!("{base}/v1/models");
    let mut up = false;
    while std::time::Instant::now() < deadline {
        if client
            .get(&models_url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
    if !up {
        return Err(format!(
            "daemon never answered on :{port} within {}s.\n    \
             The config is written; this is a startup failure.\n    \
             Run `svrn daemon run` to watch the load errors live.",
            DAEMON_READY_TIMEOUT.as_secs()
        ));
    }
    println!("    \u{2713} daemon reachable on :{port}");

    // 2. FIM slot installed. A null here means the marker probe
    //    refused the model — the one failure this whole command
    //    exists to make legible.
    let status: serde_json::Value = client
        .get(format!("{base}/status"))
        .send()
        .await
        .map_err(|e| format!("GET /status failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("GET /status returned unparseable JSON: {e}"))?;
    let fim = status.pointer("/inference/fim").filter(|v| !v.is_null()).ok_or_else(|| {
        "the daemon is up but reports no FIM slot (`inference.fim` is null).\n    \
         The vocab probe refused the model — its FIM markers did not tokenize\n    \
         atomically. Check the daemon log's [fim] lines; they name the model\n    \
         and the reason."
            .to_string()
    })?;
    let str_at = |k: &str| {
        fim.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    };
    let aliased_to_fast = fim
        .get("aliased_to_fast")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (slot, model_id, fim_style) = (str_at("slot"), str_at("model_id"), str_at("fim_style"));
    println!("    \u{2713} FIM slot live — {model_id} ({fim_style}) on slot '{slot}'");
    if !aliased_to_fast {
        // Not fatal — completions work either way — but it means the
        // lean-mode invariant broke and the operator is paying for a
        // second resident copy without having asked to.
        println!(
            "    \u{26a0} serving from a DEDICATED slot, not the shared fast slot —"
        );
        println!("      lean mode didn't take. Two copies of the model are resident.");
    }

    // 3. A real completion. The synthetic case is deliberately one
    //    the model cannot get "right" by echoing: the answer has to
    //    come from the middle.
    let body = serde_json::json!({
        "prefix": "fn fibonacci(n: u32) -> u32 {\n    match n {\n        0 => 0,\n        1 => 1,\n        _ => ",
        "suffix": "\n    }\n}\n",
        "path": "probe.rs",
        "language": "rust",
        "debug": true,
    });
    let resp = client
        .post(format!("{base}/v1/completions"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST /v1/completions failed: {e}"))?;
    let code = resp.status();
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("POST /v1/completions returned unparseable JSON: {e}"))?;
    if !code.is_success() {
        return Err(format!(
            "POST /v1/completions returned {code}: {}",
            json.get("error")
                .map(|e| e.to_string())
                .unwrap_or_else(|| json.to_string())
        ));
    }
    let text = json
        .pointer("/choices/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let ttft_ms = json
        .pointer("/sovereign_debug/timings_ms/ttft")
        .and_then(|v| v.as_u64());
    if text.trim().is_empty() {
        return Err(format!(
            "the completion round-tripped but came back empty \
             (stop_rule={}).\n    The slot is live, so this is a sampling / stop-rule \
             problem rather than a setup one.",
            json.pointer("/sovereign_debug/stop_rule")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
        ));
    }
    println!(
        "    \u{2713} completion round-tripped ({} chars{})",
        text.len(),
        ttft_ms
            .map(|t| format!(", ttft {t}ms"))
            .unwrap_or_default()
    );

    Ok(Verified {
        slot,
        model_id,
        fim_style,
        aliased_to_fast,
        sample: text.lines().next().unwrap_or("").trim().to_string(),
        ttft_ms,
    })
}

// ─── Editor extension ──────────────────────────────────────────────

enum EditorOutcome {
    Installed { editor: String },
    /// No editor CLI on PATH, or no extension sources to build from.
    /// Carries the reason so the banner can say what to do instead.
    Unavailable(String),
    Skipped,
}

/// Install the `svrn fim` extension into whichever VS Code-family
/// editor is on PATH. Best-effort by design: a failure here leaves a
/// fully working daemon, so it degrades to an instruction rather than
/// failing the command.
fn install_extension() -> EditorOutcome {
    let Some(editor) = find_editor() else {
        return EditorOutcome::Unavailable(
            "no VS Code-family CLI on PATH (looked for code, cursor, windsurf, codium)".into(),
        );
    };
    let Some(ext_dir) = find_extension_dir() else {
        return EditorOutcome::Unavailable(
            "extension sources not found (this build isn't running from a repo checkout)".into(),
        );
    };

    println!("  Installing the editor extension ({editor})...");

    let vsix = match existing_vsix(&ext_dir).or_else(|| build_vsix(&ext_dir)) {
        Some(v) => v,
        None => {
            return EditorOutcome::Unavailable(format!(
                "could not produce a .vsix in {}",
                ext_dir.display()
            ))
        }
    };

    match std::process::Command::new(&editor)
        .arg("--install-extension")
        .arg(&vsix)
        .arg("--force")
        .output()
    {
        Ok(out) if out.status.success() => {
            println!("    \u{2713} installed into {editor}");
            EditorOutcome::Installed { editor }
        }
        Ok(out) => EditorOutcome::Unavailable(format!(
            "{editor} --install-extension failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => EditorOutcome::Unavailable(format!("could not run {editor}: {e}")),
    }
}

fn find_editor() -> Option<String> {
    ["code", "cursor", "windsurf", "codium"]
        .into_iter()
        .find(|c| on_path(c))
        .map(str::to_string)
}

/// PATH scan rather than shelling out to `which` — one less external
/// dependency on a path that runs during onboarding, when the least
/// is known about the machine.
fn on_path(cmd: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let candidates: &[&str] = if cfg!(windows) {
        &[".cmd", ".exe", ""]
    } else {
        &[""]
    };
    std::env::split_paths(&path).any(|dir| {
        candidates
            .iter()
            .any(|ext| dir.join(format!("{cmd}{ext}")).is_file())
    })
}

/// Locate `packages/vscode-sovereign`. Checked against the working
/// directory first (the common case: an operator in the repo) and
/// then relative to the running binary, which covers
/// `target/debug/sovereign-cli-daemon` invoked from elsewhere.
fn find_extension_dir() -> Option<PathBuf> {
    const REL: &str = "packages/vscode-sovereign";
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().map(Path::to_path_buf));
    }
    roots
        .into_iter()
        .map(|r| r.join(REL))
        .find(|p| p.join("package.json").is_file())
}

fn existing_vsix(dir: &Path) -> Option<PathBuf> {
    let mut found: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("vsix") {
            continue;
        }
        let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
        if found.as_ref().is_none_or(|(t, _)| modified > *t) {
            found = Some((modified, path));
        }
    }
    found.map(|(_, p)| p)
}

/// `npm install && npm run package`. Slow (tens of seconds) and
/// noisy, so it is announced; silent multi-minute stalls during
/// onboarding are how people conclude a tool has hung.
fn build_vsix(dir: &Path) -> Option<PathBuf> {
    if !on_path("npm") {
        return None;
    }
    println!("    building the extension (npm — this takes a moment)...");
    for args in [["install"].as_slice(), ["run", "package"].as_slice()] {
        let out = std::process::Command::new("npm")
            .args(args)
            .current_dir(dir)
            .output()
            .ok()?;
        if !out.status.success() {
            eprintln!(
                "    npm {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return None;
        }
    }
    existing_vsix(dir)
}

// ─── Final banner ──────────────────────────────────────────────────

/// The decision, the escape hatch, and the one upgrade worth trying —
/// in that order, and short. An operator who reads nothing else
/// should still know what changed on their machine and how to undo it.
fn print_decision(plan: &Plan, v: &Verified, editor: &EditorOutcome) {
    println!();
    println!("  {}", "─".repeat(54));
    println!("  \u{2713} Inline completion is live");
    println!();
    println!("  What we decided");
    println!(
        "    {} on {} hardware \u{2014} {}",
        v.model_id,
        profile_label(&plan.profile),
        if plan.rung_overridden {
            format!("{} because you asked for it", plan.slot.quant)
        } else {
            format!("{} picked for your hardware", plan.slot.quant)
        }
    );
    if v.aliased_to_fast {
        println!(
            "    Lean mode: it serves chat AND completions from slot '{}' \u{2014}",
            v.slot
        );
        println!("    one copy in RAM. Your previous chat model is no longer loaded.");
    } else {
        println!("    Dedicated slot '{}' \u{2014} a second model is resident.", v.slot);
    }
    println!("    Style {} \u{2014} probed from the vocab, not assumed.", v.fim_style);
    if !v.sample.is_empty() {
        println!(
            "    Proof: it completed `_ =>` with `{}`{}.",
            truncate(&v.sample, 44),
            v.ttft_ms
                .map(|t| format!(" in {t}ms"))
                .unwrap_or_default()
        );
    }

    println!();
    println!("  Swap the model");
    println!("    svrn setup --fim --quant <rung>     # mxfp4_moe | q4_k_m | q6_k | q8_0");
    // Deliberately NOT `svrn model set primary <file>`: in lean mode
    // primary and models.fim.path must move together, and `model set`
    // touches only one of them. A half-applied swap loads two models.
    println!("    (both primary and models.fim move together \u{2014} that's what keeps it lean)");
    if let Some((next_rung, next_slot)) = next_fim_rung(&plan.rung) {
        println!();
        println!("  Worth trying next");
        println!(
            "    svrn setup --fim --quant {next_rung}      # {} {} \u{2014} {:.1} GB, +{:.1} GB",
            next_slot.base_name,
            next_slot.quant,
            next_slot.size_gb,
            next_slot.size_gb - plan.slot.size_gb
        );
        println!("    Better completions if you have the memory; same model, finer quant.");
    }

    if plan.existing.is_some() {
        println!();
        println!("  Undo");
        println!(
            "    cp {} {}  &&  svrn daemon restart",
            plan.backup_path.display(),
            plan.config_path.display()
        );
    }

    println!();
    match editor {
        EditorOutcome::Installed { editor } => {
            println!("  Next: reload {editor}, open a code file, pause after a line.");
            println!("        Ghost text appears; Tab accepts. The status bar shows the model.");
        }
        EditorOutcome::Skipped => {
            println!("  Editor: skipped (--skip-editor). The daemon is serving completions");
            println!("          at http://127.0.0.1:{}/v1/completions.", plan.client_port);
        }
        EditorOutcome::Unavailable(why) => {
            println!("  Editor: not installed \u{2014} {why}.");
            println!("          Completions are still being served at");
            println!(
                "            http://127.0.0.1:{}/v1/completions",
                plan.client_port
            );
            println!("          To get the extension: download the .vsix from the project's");
            println!("          GitHub releases (tag `svrn-fim-*`) and run");
            println!("            code --install-extension <downloaded>.vsix");
            println!("          From a source checkout you can build it instead:");
            println!("            cd packages/vscode-sovereign && npm install && npm run package");
        }
    }
}

fn profile_label(p: &ProfileName) -> &'static str {
    match p {
        ProfileName::CpuOnly => "cpu-only",
        ProfileName::LowMem => "low-memory",
        ProfileName::Default => "default-tier",
        ProfileName::High => "high-tier",
        ProfileName::VeryHigh => "very-high-tier",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(file: &str, size_gb: f64) -> SlotConfig {
        SlotConfig {
            file: file.into(),
            base_name: "Mellum2-12B-A2.5B".into(),
            quant: "Q6_K".into(),
            size_gb,
            hf_url: "https://huggingface.co/JetBrains/x".into(),
            ..Default::default()
        }
    }

    fn plan_with(model: PathBuf, existing: Option<ExistingConfig>) -> Plan {
        Plan {
            profile: ProfileName::High,
            rung: "q6_k".into(),
            rung_overridden: false,
            slot: slot("m.gguf", 10.88),
            models_dir: model.parent().unwrap().to_path_buf(),
            model_path: model,
            embed_download: None,
            embed_path: PathBuf::from("/models/embed.gguf"),
            existing,
            config_path: PathBuf::from("/cfg/config.toml"),
            backup_path: PathBuf::from("/cfg/config.toml.pre-fim"),
            data_dir: PathBuf::from("/cfg"),
            client_port: 9741,
        }
    }

    /// A re-run against files already on disk must not claim it is
    /// about to download 10.88 GB — the consent screen is only worth
    /// anything if its numbers are true.
    #[test]
    fn download_gb_is_zero_when_the_model_is_already_present() {
        let tmp = tempfile::tempdir().unwrap();
        let model = tmp.path().join("m.gguf");
        std::fs::write(&model, b"GGUF").unwrap();
        let plan = plan_with(model, None);
        assert_eq!(plan.download_gb(), 0.0);
    }

    #[test]
    fn download_gb_counts_a_missing_model() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = plan_with(tmp.path().join("absent.gguf"), None);
        assert!((plan.download_gb() - 10.88).abs() < f64::EPSILON);
    }

    #[test]
    fn download_gb_counts_a_missing_embed_too() {
        let tmp = tempfile::tempdir().unwrap();
        let model = tmp.path().join("m.gguf");
        std::fs::write(&model, b"GGUF").unwrap();
        let mut plan = plan_with(model, None);
        let embed = tmp.path().join("e.gguf");
        plan.embed_download = Some((slot("e.gguf", 0.6), embed));
        assert!((plan.download_gb() - 0.6).abs() < 1e-9);
    }

    /// An empty file is not a model. `download_gguf` resumes from a
    /// `.part`, but a zero-byte final path is the signature of an
    /// interrupted move and must count as absent.
    #[test]
    fn zero_byte_files_count_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let model = tmp.path().join("m.gguf");
        std::fs::write(&model, b"").unwrap();
        assert!(!file_is_present(&model));
    }

    #[test]
    fn already_lean_requires_both_primary_and_fim_to_match() {
        let model = PathBuf::from("/models/m.gguf");
        let other = PathBuf::from("/models/other.gguf");

        let matching = plan_with(
            model.clone(),
            Some(ExistingConfig {
                primary: model.clone(),
                fim_path: Some(model.clone()),
            }),
        );
        assert!(matching.already_lean_on_this_model());

        // primary matches but FIM was never configured
        let half = plan_with(
            model.clone(),
            Some(ExistingConfig {
                primary: model.clone(),
                fim_path: None,
            }),
        );
        assert!(!half.already_lean_on_this_model());

        // FIM points somewhere else — the dedicated-slot arrangement
        let dedicated = plan_with(
            model.clone(),
            Some(ExistingConfig {
                primary: other,
                fim_path: Some(model),
            }),
        );
        assert!(!dedicated.already_lean_on_this_model());

        assert!(!plan_with(PathBuf::from("/models/m.gguf"), None).already_lean_on_this_model());
    }

    #[test]
    fn backup_path_sits_beside_the_config() {
        let cfg = PathBuf::from("/home/u/.sovereign/config.toml");
        assert_eq!(
            cfg.with_extension("toml.pre-fim"),
            PathBuf::from("/home/u/.sovereign/config.toml.pre-fim")
        );
    }

    #[test]
    fn truncate_keeps_short_strings_whole() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdefghij", 5), "abcd\u{2026}");
    }

    #[test]
    fn on_path_finds_a_real_executable_dir_entry() {
        // `sh` exists on every unix host the daemon supports; on
        // Windows the candidate-extension branch is what matters and
        // this assertion would be about a different binary, so scope
        // the test to unix.
        #[cfg(unix)]
        assert!(on_path("sh"), "expected sh on PATH");
        assert!(!on_path("definitely-not-a-real-binary-9f3a"));
    }

    #[test]
    fn existing_vsix_picks_the_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("sovereign-fim-0.0.1.vsix");
        let new = tmp.path().join("sovereign-fim-0.1.0.vsix");
        std::fs::write(&old, b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new, b"new").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"ignore me").unwrap();
        assert_eq!(existing_vsix(tmp.path()), Some(new));
    }

    #[test]
    fn existing_vsix_is_none_when_there_are_none() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), b"{}").unwrap();
        assert_eq!(existing_vsix(tmp.path()), None);
    }
}
