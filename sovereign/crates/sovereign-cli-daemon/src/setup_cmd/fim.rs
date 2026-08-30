// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn setup --fim` — one-command inline-completion onboarding.
//!
//! The manual flow this replaces is seven steps in
//! `packages/vscode-sovereign/README.md`: download a coder GGUF,
//! download an embed GGUF, hand-write a `[models.edit]` block, restart
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
//! **A dedicated slot, and the chat model is left alone.** This writes
//! `[models.edit]` only: `[models].primary` and `[models].fast` are
//! not touched, so the machine ends up with four slots — primary,
//! fast, embed, edit — and ONE editing model serving both lanes
//! (ghost text via the vocab's FIM markers, the next-edit Tab queue
//! via its own dialect).
//!
//! **This used to be lean mode**, which aliased `primary` to the
//! editing model so a single copy served chat and completions. That
//! existed because the only ladder rung was Mellum2, whose smallest
//! artifact is 7 GB — too much to pin beside a 16–20 GB primary on
//! the `high`/`very_high` tiers, which have ~3.5 GB spare AT THE
//! FLOOR of their band. The trade it made was the user's chat model.
//!
//! The default rung is 1.54 GB now (Sweep-Next-Edit-1.5B, a
//! Qwen2.5-Coder derivative whose vocab carries atomic FIM markers —
//! verified, see `models.toml`), so the headroom argument no longer
//! applies at any tier and the trade is not worth making. The Mellum2
//! rungs remain addressable via `--quant`.

use std::io::{self, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sovereign_core::models_manifest::SlotConfig;
use sovereign_inference::hardware::{self, HardwareProfile, ProfileName};
use sovereign_inference::setup_planner::{
    fim_rung_for_profile, fim_slot_for_rung, hf_download_url, next_fim_rung, resolve_slot, SlotKind,
};

use crate::setup_config::{DaemonSection, EditSection, SetupConfig};
use sovereign_core::types::NextEditFormat;

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
    ///
    /// Only the EDIT path is compared now. It used to also require
    /// `primary == model_path`, because lean mode aliased the two;
    /// the edit slot is dedicated now and the chat primary is none of
    /// this command's business.
    fn already_on_this_edit_model(&self) -> bool {
        self.existing
            .as_ref()
            .is_some_and(|e| e.fim_path.as_deref() == Some(self.model_path.as_path()))
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
        eprintln!(
            "    Config is written and correct — this is a lifecycle problem, not a FIM one."
        );
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

    print_decision(&plan, &verified, &editor, any_scip_graph_populated().await);
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
        .unwrap_or_else(sovereign_contracts::rebrand::svrnmesh_root);
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
        .and_then(|c| c.models.as_ref().map(|m| m.embed.clone()))
        .filter(|p| file_is_present(p));
    let (embed_download, embed_path) = match existing_embed {
        Some(p) => (None, p),
        None => {
            let embed_slot = resolve_slot(&profile, SlotKind::Embed).ok_or_else(|| {
                "bundled manifest has no embed slot for this hardware".to_string()
            })?;
            let path = models_dir.join(&embed_slot.file);
            (Some((embed_slot, path.clone())), path)
        }
    };

    // `--fim` edits a slot table, so it needs one: a terminal has no
    // `[models]` to add `[models.edit]` to.
    let existing = existing_cfg
        .as_ref()
        .and_then(|c| c.models.as_ref())
        .map(|m| ExistingConfig {
            primary: m.primary.clone(),
            fim_path: m.edit.as_ref().map(|f| f.path.clone()),
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
    println!(
        "    Serving  a dedicated edit slot ({:.2} GB), pinned beside your",
        plan.slot.size_gb
    );
    println!("             chat model. Both editing lanes — ghost text and the");
    println!("             next-edit Tab queue — come off this one model.");
    println!("             Your [models].primary is NOT touched.");
    match &plan.existing {
        Some(_) if plan.already_on_this_edit_model() => {
            println!();
            println!(
                "    Config   {} is already set up this way;",
                plan.config_path.display()
            );
            println!("             this run re-verifies it end to end.");
        }
        Some(e) => {
            println!();
            println!("    Config   {}", plan.config_path.display());
            println!(
                "               primary      {} \u{2192} {}",
                short(&e.primary),
                short(&plan.model_path)
            );
            println!(
                "               primary      {} (unchanged)",
                short(&e.primary)
            );
            println!(
                "               models.edit   {} \u{2192} {}",
                e.fim_path
                    .as_deref()
                    .map(short)
                    .unwrap_or_else(|| "(unset)".into()),
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
            println!(
                "             Backed up first to {}",
                plan.backup_path.display()
            );
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

/// The next-edit prompt/parse contract for a ladder rung.
///
/// The dialect is a property of the FINE-TUNE and cannot be probed
/// from the vocab, so the manifest rung is the only place that knows
/// it. Writing it here is what stops a specialist from being served
/// through the `region_instruct` default — which does not fail, it
/// returns confident, well-formed, WRONG edits (the next-edit bakeoff
/// scored Instinct 0/30 exactly that way).
fn next_edit_format_for_rung(rung: &str) -> Option<NextEditFormat> {
    match rung {
        "sweep_1_5b" => Some(NextEditFormat::Sweep),
        // The Mellum2 rungs are Instruct models and speak the default.
        _ => None,
    }
}

fn write_config(plan: &Plan) -> Result<(), String> {
    let fim = EditSection {
        path: plan.model_path.clone(),
        // Defaults are deliberate, not laziness: 4096 ctx / 48 tokens
        // / temp 0.2 are the FIM defaults documented on `EditSection`,
        // and writing them explicitly would freeze today's values
        // into every user's config where a later tuning pass could
        // never reach them.
        context_size: None,
        max_tokens: None,
        temperature: None,
        max_prefix_chars: None,
        max_suffix_chars: None,
        // ...but the dialect IS written, because it is not a tuning
        // knob with a sane default — it is a fact about the weights.
        next_edit_format: next_edit_format_for_rung(&plan.rung),
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
        Err(_) => {
            // No config at all. The edit model is a small specialist —
            // making it the chat primary is precisely the lean-mode
            // trade this command just stopped making, and a next-edit
            // fine-tune is a poor chat model. Send them through the
            // ordinary wizard first so `primary`/`fast`/`embed` exist,
            // then this command adds the fourth slot beside them.
            return Err(format!(
                "no config at {} yet — run `svrn setup` first to choose a chat model, \
                 then re-run `svrn setup --fim` to pin the editing model beside it. \
                 (This command used to write the editing model as your chat primary; \
                 it no longer does, so it needs a primary to sit next to.)",
                plan.config_path.display()
            ));
        }
    };

    // `primary` and `fast` are deliberately NOT touched. Lean mode
    // used to overwrite `primary` with the editing model and clear
    // `fast` so `fast_path()` aliased back to it — one copy in RAM,
    // at the cost of the user's chat model. The editing model is
    // 1.54 GB now, so it is pinned as its own slot and the chat
    // model survives: primary + fast + embed + edit.
    // `--fim` requires an existing slot table; `plan` was built from one.
    let models = cfg
        .models
        .as_mut()
        .ok_or("`svrn setup --fim` needs a node that holds models — a terminal has no [models] to add an editing slot to")?;
    models.embed = plan.embed_path.clone();
    models.edit = Some(fim);

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
    /// `None` when this model's vocab carries no FIM markers — it
    /// serves next-edit but `/v1/completions` 503s. A supported
    /// arrangement, so it is reported rather than treated as a probe
    /// failure.
    fim_style: Option<String>,
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
    // Prefer the canonical key; fall back to the deprecated `fim`
    // mirror so this verification still works against a daemon binary
    // older than the lane split. The mirror is scheduled for removal —
    // when it goes, this fallback goes with it.
    let edit = status
        .pointer("/inference/edit")
        .or_else(|| status.pointer("/inference/fim"))
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            "the daemon is up but reports no editing slot (`inference.edit` is\n    \
             null). Check the daemon log's [edit_slot] lines; they name the model\n    \
             and the reason."
                .to_string()
        })?;
    let str_at = |k: &str| {
        edit.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    };
    let aliased_to_fast = edit
        .get("aliased_to_fast")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (slot, model_id) = (str_at("slot"), str_at("model_id"));
    // `fim_style` is now absent when the model carries no FIM markers.
    // That is a real, supported arrangement — next-edit serves and
    // `/v1/completions` 503s — so report it as such rather than
    // printing "?" and letting the operator assume a broken probe.
    match edit.get("fim_style").and_then(|v| v.as_str()) {
        Some(style) => {
            println!("    \u{2713} FIM slot live — {model_id} ({style}) on slot '{slot}'")
        }
        None => {
            println!("    \u{26a0} {model_id} on slot '{slot}' serves next-edit but NOT FIM —");
            println!("      its tokenizer carries no FIM markers, so /v1/completions will");
            println!("      503. Point [models.edit].path at a coder GGUF (Mellum2,");
            println!("      Qwen2.5-Coder) if you need inline completion.");
        }
    }
    if aliased_to_fast {
        // Inverted from what this used to check. A dedicated slot is
        // now the intended arrangement, so the surprising case is the
        // ALIAS: it means `[models].primary` still points at the
        // editing model, i.e. a config left over from lean mode.
        // Completions work, but chat is being served by an editing
        // model — which is the trade this command stopped making.
        println!("    \u{26a0} the editing model is also your [models].primary —");
        println!("      chat is being served by an editing model. That is the old");
        println!("      lean-mode arrangement; point [models].primary at a chat");
        println!("      model to get the four-slot layout.");
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
        ttft_ms.map(|t| format!(", ttft {t}ms")).unwrap_or_default()
    );

    Ok(Verified {
        slot,
        model_id,
        fim_style: edit
            .get("fim_style")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        aliased_to_fast,
        sample: text.lines().next().unwrap_or("").trim().to_string(),
        ttft_ms,
    })
}

// ─── Editor extension ──────────────────────────────────────────────

/// Where a user without a source checkout gets the `.vsix`. The extension
/// ships on the public shelf under its own `vscode-v*` tag stream (see
/// RELEASING.md). Same shelf as `update_cmd::REPO` — keep them in step.
///
/// This path matters more than it looks: anyone who installed from the CLI
/// tarball has no `packages/vscode-sovereign` to build from, so this banner
/// is the *only* instruction they get. It named a tag prefix that never
/// existed (`svrn-fim-*`) until 2026-07-29.
///
/// Deliberately the unfiltered list. GitHub's `?q=tag:vscode-v` releases
/// filter returns HTTP 200 with the release absent from the body (verified
/// 2026-07-29), so a "helpful" filtered link is an empty page — worse than
/// a list the reader can scan. There is no per-prefix `latest` pointer
/// either; `/releases/latest` is repo-global and belongs to the desktop app.
const VSIX_RELEASES_URL: &str = "https://github.com/alexsbryan/svrnmesh-releases/releases";

enum EditorOutcome {
    Installed {
        editor: String,
    },
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
/// Does ANY corpus hold a populated SCIP graph?
///
/// Asks the graph for its symbol count rather than testing the file's
/// existence or size: an empty schema is ~4 KB and a failed export leaves
/// exactly that, which is the bug `doctor`'s `scip_indexed` check was
/// rewritten to catch. Same question that check asks, and it must not drift
/// into a second answer (ARCH §10.6) — if this ever needs more than a
/// boolean, call into that check rather than growing a rival.
async fn any_scip_graph_populated() -> bool {
    let dir = crate::daemon_cmd::sovereign_root().join("indexes");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let db = entry.path().join("scip_graph.db");
        if !db.exists() {
            continue;
        }
        let Some(name) = entry
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if let Ok(graph) = corpus_engine_scip::ScipGraph::open(&db, &name) {
            if graph.symbol_count().await > 0 {
                return true;
            }
        }
    }
    false
}

fn print_decision(plan: &Plan, v: &Verified, editor: &EditorOutcome, scip_populated: bool) {
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
        println!(
            "    Dedicated slot '{}' \u{2014} a second model is resident.",
            v.slot
        );
    }
    match &v.fim_style {
        Some(style) => {
            println!("    Style {style} \u{2014} probed from the vocab, not assumed.")
        }
        None => println!(
            "    No FIM markers in the vocab \u{2014} next-edit serves, \
             /v1/completions 503s."
        ),
    }
    if !v.sample.is_empty() {
        println!(
            "    Proof: it completed `_ =>` with `{}`{}.",
            truncate(&v.sample, 44),
            v.ttft_ms.map(|t| format!(" in {t}ms")).unwrap_or_default()
        );
    }

    // The symbol lane needs a code index and setup does not build one.
    // OFFERED, never run: indexing a repo takes minutes, and `setup --fim`
    // is a command people expect to finish. Silence here is what makes the
    // lane invisible on a fresh install — the status-bar item simply never
    // appears, there is no error, and nobody thinks to look (that failure
    // mode is the first entry under "Traps" in NEXT_EDIT_HANDOVER.md).
    // Suppressed once a populated graph exists, so this stops nagging the
    // moment it is true.
    if !scip_populated {
        println!();
        println!("  Optional \u{2014} unlock call-site navigation");
        println!("    svrn init                # in your repo; minutes, one time");
        println!();
        println!("    Edit a function's parameter list and the editor offers its call");
        println!("    sites as a jump list \u{2014} the shape the pattern engine cannot see,");
        println!("    because there the edit you make and the edits it implies are");
        println!("    different text. It reads a symbol graph, so it needs that index.");
        println!("    Rust only today. It proposes no edits, only places to go.");
        println!("    Check it later with:  svrn doctor        # see `scip_indexed`");
    }

    println!();
    println!("  Swap the model");
    println!(
        "    svrn setup --fim --quant <rung>     # sweep_1_5b | mxfp4_moe | q4_k_m | q6_k | q8_0"
    );
    println!("    (only [models.edit] moves \u{2014} your chat model is left alone)");
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
            println!(
                "          at http://127.0.0.1:{}/v1/completions.",
                plan.client_port
            );
        }
        EditorOutcome::Unavailable(why) => {
            println!("  Editor: not installed \u{2014} {why}.");
            println!("          Completions are still being served at");
            println!(
                "            http://127.0.0.1:{}/v1/completions",
                plan.client_port
            );
            println!("          To get the extension: open the release shelf, pick the newest");
            println!("          `vscode-v*` release, download its `sovereign-fim-*.vsix`, then:");
            println!("            {VSIX_RELEASES_URL}");
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
    fn already_configured_looks_only_at_the_edit_path() {
        let model = PathBuf::from("/models/m.gguf");
        let other = PathBuf::from("/models/other.gguf");

        let matching = plan_with(
            model.clone(),
            Some(ExistingConfig {
                primary: model.clone(),
                fim_path: Some(model.clone()),
            }),
        );
        assert!(matching.already_on_this_edit_model());

        // primary matches but FIM was never configured
        let half = plan_with(
            model.clone(),
            Some(ExistingConfig {
                primary: model.clone(),
                fim_path: None,
            }),
        );
        assert!(!half.already_on_this_edit_model());

        // A DIFFERENT chat primary with the edit slot already on this
        // model is the arrangement we now write, so it counts as
        // already-configured. Under lean mode this asserted the
        // opposite — `primary` had to match too, and a separate chat
        // model made the config look unconfigured.
        let dedicated = plan_with(
            model.clone(),
            Some(ExistingConfig {
                primary: other.clone(),
                fim_path: Some(model),
            }),
        );
        assert!(dedicated.already_on_this_edit_model());

        // The edit slot pointing at some OTHER model is what "not
        // configured with this one" actually means now.
        let elsewhere = plan_with(
            PathBuf::from("/models/m.gguf"),
            Some(ExistingConfig {
                primary: other,
                fim_path: Some(PathBuf::from("/models/different.gguf")),
            }),
        );
        assert!(!elsewhere.already_on_this_edit_model());

        assert!(!plan_with(PathBuf::from("/models/m.gguf"), None).already_on_this_edit_model());
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

    /// The no-source-checkout banner is the only install instruction a
    /// tarball user ever sees, and for months it pointed at a tag prefix
    /// (`svrn-fim-*`) that no release has ever used. Nothing failed loudly
    /// — the text just sent people hunting. Pin the shelf so a future edit
    /// can't quietly reintroduce a dead pointer.
    #[test]
    fn vsix_releases_url_points_at_the_public_shelf() {
        assert!(
            VSIX_RELEASES_URL.starts_with("https://github.com/alexsbryan/svrnmesh-releases"),
            "the .vsix must be sourced from the public shelf, not the private source repo: {VSIX_RELEASES_URL}"
        );
        assert!(
            VSIX_RELEASES_URL.ends_with("/releases"),
            "must be the unfiltered release list — GitHub's ?q=tag: filter renders an empty page: {VSIX_RELEASES_URL}"
        );
        assert!(
            !VSIX_RELEASES_URL.contains("svrn-fim"),
            "`svrn-fim-*` is not a real tag stream; the extension ships under `vscode-v*`"
        );
    }

    #[test]
    fn existing_vsix_is_none_when_there_are_none() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), b"{}").unwrap();
        assert_eq!(existing_vsix(tmp.path()), None);
    }
}
