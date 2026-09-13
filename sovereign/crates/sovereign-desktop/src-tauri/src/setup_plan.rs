// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the first-run plan comes from — and the run that acts on it.
//!
//! # Why a spawn (sv-surface svt-7, operator 2026-09-12)
//!
//! The app used to answer "what can this machine run, and which models should
//! it fetch" out of `sovereign_inference::{hardware, setup_planner}` linked
//! into its own binary. That is the daemon's decision — it is the process
//! that loads the weights — and holding the crate to make it was the last
//! thing keeping the inference stack in the desktop bundle.
//!
//! Asking the daemon works only once there IS one, and on a first run there
//! is not: the sidecar exits 1 with no config off a TTY
//! (`daemon_cmd/mod.rs`), refuses a config with no `[models]`
//! (`daemon_cmd/build/inference.rs`), and the app reaches it only after the
//! wizard has written config (`serving_host::ensure_reachable`). So first run
//! SPAWNS the sidecar's own `setup` verb — `--plan --json` to render the
//! screens, `--yes --json` to run them — and a spawn links nothing.
//!
//! Both sides answer the SAME `sovereign_contracts::daemon_wire` shapes
//! (`assets_http` serves them; `setup --plan --json` prints them), so this
//! module is a router between two sources of one answer, never a third
//! implementation of it.

use std::path::PathBuf;
use std::process::Stdio;

use sovereign_contracts::daemon_wire::{
    HardwareProfile, PrimaryOption, ProfileName, SetupPlan, SetupProgressLine, SetupProgressPhase,
    SlotConfig,
};
use sovereign_turn_client::TurnClient;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// The sidecar this build ships — the same binary `serving_host` brings up.
fn sidecar() -> Result<PathBuf, String> {
    crate::daemon_binary::stable_daemon_binary().ok_or_else(|| {
        "this build ships no backend binary, so it cannot compute a setup plan or run \
         first-run setup. Install the CLI and run `svrn setup` in a terminal."
            .to_string()
    })
}

/// Run `svrn setup --plan --json` and parse its stdout.
///
/// Reads nothing, writes nothing, downloads nothing — which is what lets this
/// run on a machine that has never been set up, and run again afterwards.
pub(crate) async fn spawn_plan() -> Result<SetupPlan, String> {
    let bin = sidecar()?;
    let out = Command::new(&bin)
        .args(["setup", "--plan", "--json"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("spawning {} setup --plan: {e}", bin.display()))?;
    if !out.status.success() {
        // The verb writes its refusal to stderr and the plan to stdout. Hand
        // the operator's sentence through rather than a bare exit code.
        return Err(format!(
            "{} setup --plan exited {}: {}",
            bin.display(),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "could not read the setup plan ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
                .chars()
                .take(200)
                .collect::<String>()
        )
    })
}

/// What this machine can run, and the tier that follows.
///
/// The DAEMON's answer when one is reachable — it is the process that holds
/// the weights, and on an attached boot its hardware is the hardware that
/// matters. The spawn otherwise.
pub(crate) async fn hardware(base_url: &str) -> Result<(HardwareProfile, ProfileName), String> {
    #[derive(serde::Deserialize)]
    struct View {
        hardware: HardwareProfile,
        profile: ProfileName,
    }
    match TurnClient::new(base_url.to_string())
        .admin_hardware::<View>()
        .await
    {
        Ok(v) => Ok((v.hardware, v.profile)),
        Err(e) => {
            tracing::info!(reason = %e, "setup_plan: no daemon answered /v1/admin/hardware — spawning the setup verb");
            let plan = spawn_plan().await?;
            Ok((plan.hardware, plan.profile))
        }
    }
}

/// The curated primary catalog for `profile`, or for the detected tier.
pub(crate) async fn catalog(
    base_url: &str,
    profile: Option<&str>,
) -> Result<Vec<PrimaryOption>, String> {
    match TurnClient::new(base_url.to_string())
        .setup_catalog::<Vec<PrimaryOption>>(profile)
        .await
    {
        Ok(c) => Ok(c),
        Err(e) => {
            tracing::info!(reason = %e, "setup_plan: no daemon answered the catalog — spawning the setup verb");
            let plan = spawn_plan().await?;
            refuse_other_tier(profile, plan.profile)?;
            Ok(plan.catalog)
        }
    }
}

/// The single-pick `fast` or `embed` slot. `None` means the bundled manifest
/// defines none for the tier — absent, not substituted.
pub(crate) async fn slot(
    base_url: &str,
    kind: &str,
    profile: Option<&str>,
) -> Result<Option<SlotConfig>, String> {
    match TurnClient::new(base_url.to_string())
        .setup_slot::<Option<SlotConfig>>(kind, profile)
        .await
    {
        Ok(s) => Ok(s),
        Err(e) => {
            tracing::info!(reason = %e, kind, "setup_plan: no daemon answered the slot read — spawning the setup verb");
            let plan = spawn_plan().await?;
            refuse_other_tier(profile, plan.profile)?;
            match kind {
                "fast" => Ok(plan.fast),
                "embed" => Ok(plan.embed),
                other => Err(format!("unknown slot kind: {other}")),
            }
        }
    }
}

/// `setup --plan` describes THIS machine's tier and takes no tier argument.
/// A caller that asked for a different one is told so rather than handed the
/// detected tier's answer under the name it asked for (ARCH principle 6).
///
/// Every shipped caller passes `None` (`SetupPlan.svelte`, `SetupFlow.svelte`,
/// `ModelSelector.svelte` all call `primaryCatalog()` bare), so this is the
/// guard on a door nobody opens today — and the substitution it refuses is
/// exactly the kind that reads as working.
fn refuse_other_tier(asked: Option<&str>, got: ProfileName) -> Result<(), String> {
    match asked {
        Some(p) if p != got.as_str() => Err(format!(
            "no daemon is running, and the setup plan describes this machine's own \
             tier ({}) — it cannot answer for `{p}`. Start the daemon and ask again.",
            got.as_str()
        )),
        _ => Ok(()),
    }
}

/// Run `svrn setup --yes --json` and hand every progress line to `on_line` as
/// it arrives.
///
/// The wizard's whole body — detect, resolve, download three slots, write
/// `config.toml` — is the sidecar's, unchanged, and this reads its narration.
/// Returns the terminal `done` line; every other ending is an `Err` carrying
/// the reason the run gave, never a bare exit code.
///
/// `primary` is the user's pick from the plan: a catalog file name, a pasted
/// `.gguf` URL, or a `.gguf` already on disk. `None` takes the hardware
/// recommendation, which is what the no-decisions flow has always done.
pub(crate) async fn spawn_setup_run(
    data_dir: &std::path::Path,
    primary: Option<&str>,
    on_line: impl Fn(&SetupProgressLine),
) -> Result<SetupProgressLine, String> {
    let bin = sidecar()?;
    let mut cmd = Command::new(&bin);
    cmd.args(["setup", "--yes", "--json", "--data-dir"])
        .arg(data_dir);
    if let Some(p) = primary {
        cmd.arg("--primary").arg(p);
    }
    tracing::info!(
        binary = %bin.display(),
        data_dir = %data_dir.display(),
        primary = primary.unwrap_or("<recommended>"),
        "setup_plan: spawning the sidecar's setup verb"
    );
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawning {} setup --yes: {e}", bin.display()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "setup produced no stdout to read".to_string())?;
    // The human narration moves to stderr under `--json`. Forward it to the
    // app log rather than dropping it: when a download fails, the sidecar's
    // sentence is the only account of why, and a UI frame is not a log.
    if let Some(err) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if !l.trim().is_empty() {
                    tracing::info!(target: "setup_sidecar", "{l}");
                }
            }
        });
    }

    let mut terminal: Option<SetupProgressLine> = None;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(raw) = lines
        .next_line()
        .await
        .map_err(|e| format!("reading setup output: {e}"))?
    {
        if raw.trim().is_empty() {
            continue;
        }
        let line: SetupProgressLine = match serde_json::from_str(&raw) {
            Ok(l) => l,
            Err(e) => {
                // `--json` reserves stdout for these. An unparseable line is a
                // contract break, not noise to skip past quietly.
                tracing::warn!(raw = %raw, error = %e, "setup_plan: unparseable line on the setup stream");
                continue;
            }
        };
        on_line(&line);
        if matches!(
            line.phase,
            SetupProgressPhase::Done | SetupProgressPhase::Failed
        ) {
            terminal = Some(line);
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("waiting on setup: {e}"))?;
    match terminal {
        Some(l) if l.phase == SetupProgressPhase::Done && status.success() => Ok(l),
        Some(l) if l.phase == SetupProgressPhase::Failed => Err(l
            .error
            .unwrap_or_else(|| "setup failed and reported no reason".to_string())),
        // A `done` line with a non-zero exit, or an exit with no terminal line
        // at all. Both are "the run did not answer", which is a different fact
        // from "it answered: no" (ARCH principle 6) — say which.
        Some(_) => Err(format!(
            "setup reported success but exited {}",
            status.code().unwrap_or(-1)
        )),
        None => Err(format!(
            "setup exited {} without reporting a result — see the `setup_sidecar` log lines",
            status.code().unwrap_or(-1)
        )),
    }
}
