// SPDX-License-Identifier: AGPL-3.0-or-later
//! Milestone lifecycle: `start-milestone`, `end-milestone`, `next`,
//! plus the auto-redteam spawn and the subprocess driver.
//!
//! Flow:
//!
//! 1. **`start-milestone`** or **`next`** (the seamless variant)
//!    opens an [`AtosRunRow`], spawns a driver subprocess with
//!    `SOVEREIGN_FEATURE_ID` / `ATOS_RUN_ID` / `ATOS_DRIVER` /
//!    `ATOS_MODE` exported, then closes the run with a placeholder
//!    verdict when the driver exits.
//! 2. **`end-milestone`** runs the stop condition, overwrites the
//!    placeholder with the real verdict, writes the per-milestone
//!    artifact, and — if the charter opted in and this is the final
//!    milestone — spawns an auto-redteam pass.
//!
//! The [`Driver`] enum names the two supported drivers; new ones get
//! added as variants rather than strings so the help text stays
//! honest.

use std::process::Stdio;

use corpus_engine_notes::NoteScope;
use sovereign_atos::{AtosOrchestrator, RunMode};

use super::args::{get_flag, split_args};
use super::stores::open_orchestrator;

// ─── start-milestone ─────────────────────────────────────────────────────────

pub(crate) async fn cmd_start_milestone(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("start-milestone: missing <id>");
        return 2;
    };
    let brief_path = match get_flag(&flags, "--brief") {
        Some(p) => p,
        None => {
            eprintln!("start-milestone: --brief <path> is required");
            return 2;
        }
    };
    let brief_md = match std::fs::read_to_string(&brief_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("start-milestone: read {brief_path}: {e}");
            return 1;
        }
    };
    let driver_flag = get_flag(&flags, "--driver");
    let no_driver = flags.iter().any(|(k, _)| k == "no-driver");
    // `--reuse-last-milestone` is set by `run-ab` so both A/B drivers
    // attach to the same milestone ordinal. Without this, each driver
    // spawn would increment the ordinal and `atos diff` would only
    // show one driver column.
    let reuse_last = flags.iter().any(|(k, _)| k == "reuse-last-milestone");
    // `--milestone-id <uuid>` lets `cmd_next` target a specific
    // charter-provisioned milestone without scanning or appending.
    // Takes precedence over --reuse-last-milestone.
    let milestone_id_override = get_flag(&flags, "--milestone-id");
    // `--red-team` opens the run in `mode=redteam` so reports and
    // teardown can exclude these runs from normal-progress gating.
    // Driver env gains `ATOS_MODE=redteam` so the opencode plugin
    // (M2.6) can restrict tool surface.
    let red_team = flags.iter().any(|(k, _)| k == "red-team");
    let run_mode = if red_team {
        RunMode::Redteam
    } else {
        RunMode::Normal
    };
    let driver = resolve_driver(driver_flag.as_deref()).await;

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("start-milestone: {e}");
            return 1;
        }
    };

    if orc.get_feature(&id).await.ok().flatten().is_none() {
        eprintln!("start-milestone: feature '{id}' is not provisioned");
        return 1;
    }

    let milestone = if let Some(mid) = milestone_id_override.as_ref() {
        let list = orc.list_milestones(&id).await.unwrap_or_default();
        match list.into_iter().find(|m| &m.id == mid) {
            Some(m) => m,
            None => {
                eprintln!("start-milestone: --milestone-id {mid} not found on feature '{id}'");
                return 1;
            }
        }
    } else if reuse_last {
        let list = orc.list_milestones(&id).await.unwrap_or_default();
        match list.into_iter().last() {
            Some(m) => m,
            None => {
                eprintln!(
                    "start-milestone: --reuse-last-milestone but feature '{id}' has no milestones"
                );
                return 1;
            }
        }
    } else {
        let ordinal = match orc.next_ordinal(&id).await {
            Ok(n) => n,
            Err(e) => {
                eprintln!("start-milestone: next_ordinal: {e}");
                return 1;
            }
        };
        match orc.add_milestone(&id, ordinal, &brief_md).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("start-milestone: {e}");
                return 1;
            }
        }
    };
    let ordinal = milestone.ordinal;
    let _ = orc.mark_milestone_started(&milestone.id).await;

    // Open an atos_runs row so the driver subprocess's tool events
    // can be attributed back to this specific (feature, milestone,
    // driver) tuple. The id is exported via env so the opencode
    // plugin can pass it back to `record_atos_event`.
    let run = match orc
        .begin_run(&id, &milestone.id, driver.as_label(), run_mode)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("start-milestone: open_run: {e}");
            return 1;
        }
    };

    println!(
        "start-milestone: feature='{id}' ordinal={ordinal} driver={} run_id={}",
        driver.as_label(),
        run.run_id
    );

    if no_driver {
        println!(
            "start-milestone: --no-driver set; milestone {} recorded without spawning a driver",
            milestone.id
        );
        return 0;
    }

    let spawn_result = driver.spawn(&id, &brief_md, &run.run_id, run_mode);
    let exit_code = match &spawn_result {
        Ok(status) => status.code().unwrap_or(-1),
        Err(_) => -1,
    };
    // stop_passed at this point is unknown — end-milestone will
    // actually run the stop condition. We provisionally close the
    // run here with stop_passed=false and stop_stdout=None so an
    // interrupted session isn't left dangling; end-milestone
    // overwrites when it lands.
    let _ = orc.close_run(&run.run_id, exit_code, false, None).await;
    match spawn_result {
        Ok(status) if status.success() => 0,
        Ok(status) => {
            eprintln!(
                "start-milestone: driver exited with {}",
                status.code().unwrap_or(-1)
            );
            1
        }
        Err(e) => {
            eprintln!("start-milestone: driver spawn failed: {e}");
            1
        }
    }
}

// ─── end-milestone ───────────────────────────────────────────────────────────

pub(crate) async fn cmd_end_milestone(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("end-milestone: missing <id>");
        return 2;
    };
    let ordinal_override: Option<i64> =
        get_flag(&flags, "--ordinal").and_then(|s| s.parse::<i64>().ok());

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("end-milestone: {e}");
            return 1;
        }
    };
    let Some(feature) = orc.get_feature(&id).await.ok().flatten() else {
        eprintln!("end-milestone: feature '{id}' not provisioned");
        return 1;
    };

    let milestones = match orc.list_milestones(&id).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("end-milestone: list_milestones: {e}");
            return 1;
        }
    };
    // Pick the milestone tied to the most recent run. This matches
    // what `atos next` (and its M3.4 descendants) expect: after
    // start-milestone opens a run against milestone 1, end-milestone
    // must close THAT milestone even if milestone 2 exists in the
    // charter. The old `.last()` behavior silently closed the wrong
    // milestone on a two-milestone charter.
    //
    // A/B parity: multiple runs can attach to the same milestone
    // (`--milestone-id` / `--reuse-last-milestone`); picking the
    // most-recent run still resolves to the right ordinal.
    let all_runs = orc.list_runs(&id).await.unwrap_or_default();
    let target = match ordinal_override {
        Some(n) => milestones.iter().find(|m| m.ordinal == n).cloned(),
        None => {
            let latest_run = all_runs.iter().max_by_key(|r| r.started_at);
            match latest_run {
                Some(r) => milestones.iter().find(|m| m.id == r.milestone_id).cloned(),
                None => milestones.last().cloned(),
            }
        }
    };
    let Some(milestone) = target else {
        eprintln!("end-milestone: no milestone to close");
        return 1;
    };

    // Run the stop condition through the orchestrator. Library owns
    // the shell spawn + stdout capture + 8KB cap. Prefer the
    // milestone-scoped stop_condition (set by the charter
    // provisioner) and fall back to the feature-level one for M1/M2
    // features.
    let stop_outcome = match orc.run_milestone_stop_condition(&feature, &milestone).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("end-milestone: stop_condition spawn: {e}");
            return 1;
        }
    };
    let stop_passed = stop_outcome.passed;

    // Gather feature-scoped notes for the compliance report. Until
    // M3.6 migrates rendering into the library, we keep a direct
    // NoteStore read here — orc.notes() exposes the same handle.
    let filter = corpus_engine_notes::ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(id.clone()),
    };
    let feature_notes: Vec<serde_json::Value> = orc
        .notes()
        .read_notes_scoped(None, &[], &[], &[], 100, false, &filter)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|n| {
                    serde_json::json!({
                        "id": n.id,
                        "kind": n.kind,
                        "content": n.content,
                        "created_at": n.created_at,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Find the run for this milestone (A/B case allows multiple).
    // Pick the most recent that's still open OR the most recent
    // overall, and close it with the real verdict.
    let runs = orc.list_runs(&feature.id).await.unwrap_or_default();
    let target_run = runs
        .iter()
        .filter(|r| r.milestone_id == milestone.id)
        .rev()
        .find(|r| r.ended_at.is_none())
        .cloned()
        .or_else(|| {
            runs.iter()
                .rfind(|r| r.milestone_id == milestone.id)
                .cloned()
        });
    if let Some(run) = target_run.as_ref() {
        let _ = orc
            .close_run(
                &run.id,
                run.exit_code.map(|c| c as i32).unwrap_or(0),
                stop_passed,
                Some(stop_outcome.stdout.as_str()),
            )
            .await;
    }

    let report = serde_json::json!({
        "feature_id": feature.id,
        "ordinal": milestone.ordinal,
        "stop_condition": feature.stop_condition,
        "stop_passed": stop_passed,
        "note_count": feature_notes.len(),
        "notes": feature_notes,
        "run_id": target_run.as_ref().map(|r| r.id.clone()),
        "driver": target_run.as_ref().map(|r| r.driver.clone()),
    });
    let report_json = report.to_string();
    let _ = orc.mark_milestone_ended(&milestone.id, &report_json).await;

    if stop_passed {
        println!(
            "end-milestone: stop_condition PASSED for feature='{}' ordinal={}",
            feature.id, milestone.ordinal
        );
        // Artifact hook: render milestone-<n>.md. Best-effort —
        // failure to write the artifact should not fail the
        // milestone closure.
        match orc
            .render_and_write_report(
                &feature.id,
                sovereign_atos::ReportSection::Milestone(milestone.ordinal),
            )
            .await
        {
            Ok(path) => println!("end-milestone: wrote {}", path.display()),
            Err(e) => eprintln!(
                "end-milestone: render milestone-{}.md failed: {e}",
                milestone.ordinal
            ),
        }
        // Red-team runs always refresh the red-team.md artifact too,
        // in case this end-milestone closes a --red-team run.
        if let Some(run) = target_run.as_ref() {
            if run.mode == "redteam" {
                match orc
                    .render_and_write_report(&feature.id, sovereign_atos::ReportSection::RedTeam)
                    .await
                {
                    Ok(path) => println!("end-milestone: wrote {}", path.display()),
                    Err(e) => eprintln!("end-milestone: render red-team.md failed: {e}"),
                }
            }
        }

        // Auto red-team trigger — fires iff:
        // - feature.auto_redteam was set (charter opt-in);
        // - this is the final milestone (highest ordinal);
        // - the just-closed run was `mode=normal` (we don't recurse
        //   on red-team's own runs);
        // - no redteam run already fired for this milestone
        //   (idempotent on re-invocation).
        let final_ordinal = milestones.iter().map(|m| m.ordinal).max().unwrap_or(0);
        let just_run_was_normal = target_run
            .as_ref()
            .map(|r| r.mode == "normal")
            .unwrap_or(false);
        let redteam_already_fired = runs
            .iter()
            .any(|r| r.milestone_id == milestone.id && r.mode == "redteam");
        if feature.auto_redteam
            && milestone.ordinal == final_ordinal
            && just_run_was_normal
            && !redteam_already_fired
        {
            println!(
                "⚙ auto-redteam: charter opted in — spawning red-team pass for milestone {}…",
                milestone.ordinal
            );
            spawn_auto_redteam(&feature.id, &milestone.id, milestone.ordinal);
        }
        0
    } else {
        eprintln!(
            "end-milestone: stop_condition FAILED for feature='{}' ordinal={}",
            feature.id, milestone.ordinal
        );
        1
    }
}

/// Spawn a blocking red-team pass over `milestone_id` by re-invoking
/// this binary. We use `std::process::Command` (not `tokio::spawn`)
/// because Yara's stdout is the right surface: she sees the red-team
/// run live in her terminal, same as a normal `start-milestone`.
///
/// Failures are noisy (printed to stderr) but never block the caller's
/// exit code — the normal milestone already passed; a red-team
/// hiccup shouldn't turn that into a "the whole thing failed."
fn spawn_auto_redteam(feature_id: &str, milestone_id: &str, ordinal: i64) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("auto-redteam: current_exe: {e} — skipping");
            return;
        }
    };

    let status = std::process::Command::new(&exe)
        .args([
            "atos",
            "start-milestone",
            feature_id,
            "--red-team",
            "--milestone-id",
            milestone_id,
        ])
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!(
                "auto-redteam: start-milestone --red-team exited {}",
                s.code().unwrap_or(-1)
            );
            return;
        }
        Err(e) => {
            eprintln!("auto-redteam: start-milestone --red-team spawn failed: {e}");
            return;
        }
    }

    let ord_str = ordinal.to_string();
    let status = std::process::Command::new(&exe)
        .args(["atos", "end-milestone", feature_id, "--ordinal", &ord_str])
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("⚙ auto-redteam: red-team run closed and red-team.md written.");
        }
        Ok(s) => {
            eprintln!(
                "auto-redteam: end-milestone exited {}",
                s.code().unwrap_or(-1)
            );
        }
        Err(e) => {
            eprintln!("auto-redteam: end-milestone spawn failed: {e}");
        }
    }
}

// ─── next (seamless handoff) ─────────────────────────────────────────────────

/// `svrn atos next [<feature-id>]` — the seamless-handoff entry
/// point. Finds the next unfinished milestone, prints a summary, asks
/// for driver confirmation, and spawns.
pub(crate) async fn cmd_next(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let auto_yes = flags.iter().any(|(k, _)| k == "yes" || k == "y");
    let driver_flag = get_flag(&flags, "--driver");

    let feature_id = match positional.first().cloned() {
        Some(id) => id,
        None => match std::env::var("SOVEREIGN_FEATURE_ID") {
            Ok(id) if !id.is_empty() => id,
            _ => {
                eprintln!(
                    "next: missing <feature-id> and $SOVEREIGN_FEATURE_ID is unset.\n\
                     Pass a feature id or run inside an ATOS-launched driver session."
                );
                return 2;
            }
        },
    };

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("next: {e}");
            return 1;
        }
    };
    let Some(feature) = orc.get_feature(&feature_id).await.ok().flatten() else {
        eprintln!("next: feature '{feature_id}' not found");
        return 1;
    };

    let Some(brief) = orc
        .next_milestone(&feature_id, RunMode::Normal)
        .await
        .unwrap_or(None)
    else {
        println!(
            "feature '{}' has no unfinished milestone — every ordinal has a passing run.",
            feature.id
        );
        println!(
            "Run `svrn atos teardown {}` to wrap it up.",
            feature.id
        );
        return 0;
    };

    // Pre-spawn summary. Shows last milestone's status + what's next.
    let runs = orc.list_runs(&feature_id).await.unwrap_or_default();
    let milestones_all = orc.list_milestones(&feature_id).await.unwrap_or_default();
    let ordinal_of = |milestone_id: &str| -> Option<i64> {
        milestones_all
            .iter()
            .find(|m| m.id == milestone_id)
            .map(|m| m.ordinal)
    };
    let last_passed_ordinal = runs
        .iter()
        .filter(|r| r.mode == "normal" && r.stop_passed == Some(true))
        .filter_map(|r| ordinal_of(&r.milestone_id))
        .max();

    println!();
    println!("  ── atos next ────────────────────────────────────────────");
    println!("  Feature:        {} [{}]", feature.id, feature.state);
    if let Some(ord) = last_passed_ordinal {
        println!("  Last milestone: {} [PASS]", ord);
    } else {
        println!("  Last milestone: (none yet)");
    }
    println!(
        "  Next milestone: {} — {}",
        brief.milestone_ordinal, brief.milestone_title
    );
    if !brief.stop_condition.is_empty() {
        println!("  Stop condition: {}", brief.stop_condition);
    } else {
        println!("  Stop condition: (manual review — no shell command)");
    }
    println!();

    // Driver selection.
    let chosen_driver = match driver_flag.as_deref() {
        Some("claude") => "claude".to_string(),
        Some("opencode") => "opencode".to_string(),
        Some(other) => {
            eprintln!("next: unknown driver '{other}'");
            return 2;
        }
        None => {
            if auto_yes {
                "claude".to_string()
            } else {
                match prompt_driver_choice() {
                    Some(d) => d,
                    None => {
                        println!("aborted.");
                        return 0;
                    }
                }
            }
        }
    };

    // Delegate to start-milestone so the spawn/ledger path stays in
    // one place. We write the composed brief to a tempfile;
    // start-milestone reads it the usual way.
    let rendered = brief.render();
    let tmp = match write_brief_tempfile(&rendered) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("next: could not write tempfile: {e}");
            return 1;
        }
    };
    let start_args = vec![
        feature_id.clone(),
        "--brief".into(),
        tmp.clone(),
        "--driver".into(),
        chosen_driver,
        // `next` never creates a new milestone — milestones are
        // charter-driven; `--milestone-id` pins the run to the
        // exact one `next_milestone` picked.
        "--milestone-id".into(),
        brief.milestone_id.clone(),
    ];
    let rc = cmd_start_milestone(&start_args).await;
    // Best-effort tempfile cleanup; leaving one behind is harmless.
    let _ = std::fs::remove_file(&tmp);
    if rc != 0 {
        return rc;
    }

    // Close out the run by running the real stop_condition. We
    // dispatch through cmd_end_milestone for the same reason: one
    // implementation of end-milestone semantics.
    cmd_end_milestone(&[feature_id]).await
}

fn prompt_driver_choice() -> Option<String> {
    use std::io::Write;
    eprint!("  spawn driver? [y=claude / o=opencode / N=abort]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    match line.trim().to_lowercase().as_str() {
        "" | "n" | "no" => None,
        "y" | "yes" | "c" | "claude" => Some("claude".into()),
        "o" | "opencode" => Some("opencode".into()),
        other => {
            eprintln!("  unknown response '{other}' — aborting.");
            None
        }
    }
}

fn write_brief_tempfile(contents: &str) -> std::io::Result<String> {
    use std::io::Write;
    let mut path = std::env::temp_dir();
    path.push(format!(
        "atos-brief-{}.md",
        uuid::Uuid::new_v4().as_simple()
    ));
    let mut f = std::fs::File::create(&path)?;
    f.write_all(contents.as_bytes())?;
    Ok(path.to_string_lossy().into_owned())
}

// ─── Driver ──────────────────────────────────────────────────────────────────

enum Driver {
    Claude,
    Opencode,
}

impl Driver {
    fn as_label(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Opencode => "opencode",
        }
    }

    fn spawn(
        &self,
        feature_id: &str,
        brief_md: &str,
        run_id: &str,
        mode: RunMode,
    ) -> std::io::Result<std::process::ExitStatus> {
        let mode_str = mode.as_str();
        match self {
            // Claude Code speaks MCP over localhost today. The MCP
            // tool calls it issues already land in `tool_call_log`;
            // the ATOS_RUN_ID export is carried for a future Claude
            // wrapper that mirrors those into `atos_tool_events`.
            Self::Claude => std::process::Command::new("claude")
                .arg("--print")
                .arg(brief_md)
                .env("SOVEREIGN_FEATURE_ID", feature_id)
                .env("ATOS_RUN_ID", run_id)
                .env("ATOS_DRIVER", "claude")
                .env("ATOS_MODE", mode_str)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status(),
            // Opencode + the sovereign-atos plugin feed tool events
            // into `atos_tool_events` keyed by $ATOS_RUN_ID. The
            // plugin is what M2.6 ships.
            Self::Opencode => std::process::Command::new("opencode")
                .arg("run")
                .arg("--input")
                .arg("-")
                .env("SOVEREIGN_FEATURE_ID", feature_id)
                .env("ATOS_RUN_ID", run_id)
                .env("ATOS_DRIVER", "opencode")
                .env("ATOS_MODE", mode_str)
                .stdin(std::process::Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .and_then(|mut child| {
                    if let Some(mut stdin) = child.stdin.take() {
                        use std::io::Write;
                        let _ = stdin.write_all(brief_md.as_bytes());
                    }
                    child.wait()
                }),
        }
    }
}

/// Pick a driver. Explicit --driver wins. Otherwise fall back to
/// Claude — the probe-based capability discovery is a future-work
/// item deferred in the original file and preserved here so the
/// refactor is behaviour-equivalent.
async fn resolve_driver(requested: Option<&str>) -> Driver {
    match requested {
        Some("opencode") => Driver::Opencode,
        Some("claude") | None => Driver::Claude,
        Some(other) => {
            eprintln!("atos: unknown driver '{other}', falling back to claude");
            Driver::Claude
        }
    }
}
