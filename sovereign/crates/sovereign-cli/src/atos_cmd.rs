//! `sovereign atos` — the Agent Task Orchestration System CLI.
//!
//! The CLI is deliberately thin. It:
//!   1. Owns the [`FeatureStore`] and [`NoteStore`] paths (the same files
//!      `sovereign project serve` uses, so artifacts are shared).
//!   2. Spawns a driver subprocess (Claude Code by default, opencode behind
//!      `--driver opencode`) with `SOVEREIGN_FEATURE_ID` exported.
//!   3. Runs the feature's `stop_condition` at end-milestone and assembles
//!      a compliance report.
//!
//! Driver autodetection probes `/v1/chat/completions` for tool-call support
//! and records an `attempt`-kind note on degradation so the operator sees
//! why the fallback kicked in. M1 defaults to Claude Code because the
//! probe fails on the current mesh adapter.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use corpus_engine::{FeatureStore, NoteScope, NoteStore};
use sovereign_atos::{AtosOrchestrator, RunMode};

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn run_atos(args: &[String]) -> i32 {
    let Some(first) = args.first() else {
        print_help();
        return 1;
    };

    // `sovereign atos --version` — tiny dogfood target exercised by M1.5.
    if matches!(first.as_str(), "--version" | "-V") {
        println!("atos {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if matches!(first.as_str(), "--help" | "-h" | "help") {
        print_help();
        return 0;
    }

    let rest = &args[1..];
    match first.as_str() {
        "provision" => cmd_provision(rest).await,
        "next" => cmd_next(rest).await,
        "start-milestone" => cmd_start_milestone(rest).await,
        "end-milestone" => cmd_end_milestone(rest).await,
        "archive" => cmd_archive(rest).await,
        "status" => cmd_status(rest).await,
        "promote" => cmd_promote(rest).await,
        "diff" => cmd_diff(rest).await,
        "run-ab" => cmd_run_ab(rest).await,
        "probe-driver" => cmd_probe_driver(rest).await,
        "report" => cmd_report(rest).await,
        "teardown" => cmd_teardown(rest).await,
        "feature" => cmd_feature(rest).await,
        other => {
            eprintln!("atos: unknown subcommand '{other}'");
            print_help();
            2
        }
    }
}

fn print_help() {
    eprintln!(
        "sovereign atos — Agent Task Orchestration System\n\
         \n\
         USAGE\n    sovereign atos <subcommand> [flags]\n\
         \n\
         SUBCOMMANDS\n\
         \x20   provision <id>        --charter <path>   (structured charter: parses ## Milestones)\n\
         \x20   provision <id>        --title <t> --charter <path> [--sovereign-md <path>] [--stop-cmd <shell>]\n\
         \x20   next [<feature-id>]   [--yes] [--driver claude|opencode]\n\
         \x20   start-milestone <id>  --brief <path> [--driver claude|opencode]\n\
         \x20   end-milestone <id>    [--ordinal N]\n\
         \x20   archive <id>          --reason <text>\n\
         \x20   status [<id>]\n\
         \x20   promote <note-id>     --to feature|global [--feature-id <id>] [--content <path>]\n\
         \x20   diff <feature-id>     [--ordinal N]\n\
         \x20   run-ab <feature-id>   --brief <path> [--drivers claude,opencode]\n\
         \x20   probe-driver          [--url http://localhost:9741/v1/chat/completions]\n\
         \x20   report <feature-id>   [--section milestone|red-team|epistemic|all] [--milestone N] [--out <path>]\n\
         \x20   teardown <feature-id> [--auto] [--dry-run]\n\
         \x20   feature approve <id>  (Commonwealth-native fallback for branches where git-committer review won't apply)\n\
         \n\
         FLAGS\n\
         \x20   --version             Print atos CLI version and exit.\n\
         \x20   --help, -h            Show this message.\n"
    );
}

// ─── Stores ──────────────────────────────────────────────────────────────────

fn sovereign_dir() -> PathBuf {
    // `.sovereign/` at the current repo root — matches where
    // `sovereign project serve` writes notes.db / features.db.
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".sovereign")
}

fn open_feature_store() -> Result<Arc<FeatureStore>, String> {
    let path = sovereign_dir().join("features.db");
    FeatureStore::open(&path)
        .map(Arc::new)
        .map_err(|e| format!("open features.db at {}: {e}", path.display()))
}

fn open_note_store() -> Result<Arc<NoteStore>, String> {
    let path = sovereign_dir().join("notes.db");
    NoteStore::open(&path)
        .map(Arc::new)
        .map_err(|e| format!("open notes.db at {}: {e}", path.display()))
}

/// Single-call orchestrator factory. Every M3+ subcommand path goes
/// through this; the raw `open_feature_store` / `open_note_store`
/// helpers above stay for now because the presentation code in
/// `render_diff` / `render_artifact_checklist` still reads stores
/// directly. Those will migrate in M3.6 when the report renderer
/// becomes library-owned.
fn open_orchestrator() -> Result<std::sync::Arc<sovereign_atos::LocalAtosOrchestrator>, String> {
    let features = open_feature_store()?;
    let notes = open_note_store()?;
    Ok(std::sync::Arc::new(
        sovereign_atos::LocalAtosOrchestrator::new(features, notes),
    ))
}

// ─── Subcommand: provision ───────────────────────────────────────────────────

async fn cmd_provision(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let id_flag = positional.first().cloned();
    let title_flag = get_flag(&flags, "--title");
    let charter_path = match get_flag(&flags, "--charter") {
        Some(p) => p,
        None => {
            eprintln!("provision: --charter <path> is required");
            return 2;
        }
    };
    let charter_md = match std::fs::read_to_string(&charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("provision: read {charter_path}: {e}");
            return 1;
        }
    };
    let sovereign_md = match get_flag(&flags, "--sovereign-md") {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("provision: read {p}: {e}");
                return 1;
            }
        },
        None => String::new(),
    };
    let stop_condition = get_flag(&flags, "--stop-cmd").unwrap_or_default();

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("provision: {e}");
            return 1;
        }
    };

    // Structured path: when `--title` is NOT given, parse the charter.
    // This preserves the M1/M2 parts-based flow for callers that want
    // imperative control AND gives Yara's "write a charter, provision
    // it" flow a first-class path.
    if title_flag.is_none() {
        match orc.provision_feature(&charter_md).await {
            Ok(f) => {
                let milestones = orc.list_milestones(&f.id).await.unwrap_or_default();
                println!(
                    "parsed {} milestone{} from charter",
                    milestones.len(),
                    if milestones.len() == 1 { "" } else { "s" }
                );
                println!("provisioned feature '{}': {}", f.id, f.title);
                return 0;
            }
            Err(e) => {
                eprintln!("provision: {e}");
                return 1;
            }
        }
    }

    // Parts-based fallback. Requires <id> positional.
    let Some(id) = id_flag else {
        eprintln!("provision: missing <id> (required unless charter drives it)");
        return 2;
    };
    let title = title_flag.unwrap_or_default();

    match orc
        .provision_feature_parts(&id, &title, &charter_md, &sovereign_md, &stop_condition)
        .await
    {
        Ok(f) => {
            println!("provisioned feature '{}': {}", f.id, f.title);
            0
        }
        Err(e) => {
            eprintln!("provision: {e}");
            1
        }
    }
}

// ─── Subcommand: start-milestone ─────────────────────────────────────────────

async fn cmd_start_milestone(args: &[String]) -> i32 {
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
                eprintln!(
                    "start-milestone: --milestone-id {mid} not found on feature '{id}'"
                );
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

    // Open an atos_runs row so the driver subprocess's tool events can
    // be attributed back to this specific (feature, milestone, driver)
    // tuple. The id is exported via env so the opencode plugin can pass
    // it back to `record_atos_event`.
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

// ─── Subcommand: end-milestone ───────────────────────────────────────────────

async fn cmd_end_milestone(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("end-milestone: missing <id>");
        return 2;
    };
    let ordinal_override: Option<i64> = get_flag(&flags, "--ordinal")
        .and_then(|s| s.parse::<i64>().ok());

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
    // the shell spawn + stdout capture + 8KB cap.
    // Prefer the milestone-scoped stop_condition (set by the charter
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
    let filter = corpus_engine::ScopeFilter {
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
        .or_else(|| runs.iter().filter(|r| r.milestone_id == milestone.id).last().cloned());
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
            Err(e) => eprintln!("end-milestone: render milestone-{}.md failed: {e}", milestone.ordinal),
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
        0
    } else {
        eprintln!(
            "end-milestone: stop_condition FAILED for feature='{}' ordinal={}",
            feature.id, milestone.ordinal
        );
        1
    }
}

// ─── Subcommand: feature approve ─────────────────────────────────────────────

/// `sovereign atos feature approve <id>` — Commonwealth-native
/// approval fallback.
///
/// Records a `FeatureApproval` row in the gossip-replicated KV store
/// so the middleware gate recognizes the feature as approved even
/// when the git path doesn't apply. Use in collectives without
/// strict git hygiene or when the reviewer is working from a
/// different machine than the repo lives on.
async fn cmd_feature(args: &[String]) -> i32 {
    let Some(sub) = args.first().cloned() else {
        eprintln!("feature: missing subcommand (approve)");
        return 2;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "approve" => cmd_feature_approve(rest).await,
        other => {
            eprintln!("feature: unknown subcommand '{other}'");
            2
        }
    }
}

async fn cmd_feature_approve(args: &[String]) -> i32 {
    let (positional, _flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("feature approve: missing <id>");
        return 2;
    };

    // Repo root = CWD. We intentionally don't walk upward for
    // `.sovereign/` — the operator is expected to run the command
    // from the feature's repo.
    let repo_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("feature approve: cwd: {e}");
            return 1;
        }
    };

    let spec_path = sovereign_atos::approval::spec_path(&repo_root, &feature_id);
    if !spec_path.exists() {
        eprintln!("feature approve: spec not found at {}", spec_path.display());
        return 1;
    }

    // Open an in-repo MeshStore on the same path commonwealth-api
    // would use. The file lives at `.sovereign/mesh.db` — we open a
    // dedicated per-repo path so approvals travel with the repo.
    let mesh_path = repo_root.join(".sovereign").join("mesh.db");
    if let Some(parent) = mesh_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mesh = match commonwealth_state::MeshStore::open(&mesh_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("feature approve: mesh open: {e}");
            return 1;
        }
    };

    // Node identity. Derive deterministically from the git user
    // identity so repeated `approve` invocations from the same
    // operator produce the same witness id.
    let origin = derive_node_id_from_git(&repo_root).unwrap_or_else(|| {
        commonwealth_core::ids::NodeId::from_u128(0xA7057E07_A7057E07u128)
    });

    match sovereign_atos::approval::record_approval(&mesh, origin, &repo_root, &feature_id) {
        Ok(appr) => {
            println!(
                "approved feature '{}' (hash {}, witness {})",
                appr.feature_id,
                &appr.spec_content_hash[..8],
                &appr.witness[..appr.witness.len().min(16)]
            );
            0
        }
        Err(e) => {
            eprintln!("feature approve: {e}");
            1
        }
    }
}

fn derive_node_id_from_git(repo_root: &std::path::Path) -> Option<commonwealth_core::ids::NodeId> {
    // Hash the reviewer's "name <email>" into a u128 so the witness
    // is reproducible per-reviewer per-machine. Not cryptographic —
    // we're not defending against impersonation, just producing a
    // stable id without inventing new identity ceremony.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
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
    let identity = format!(
        "{} <{}>",
        String::from_utf8_lossy(&name.stdout).trim(),
        String::from_utf8_lossy(&email.stdout).trim(),
    );
    let mut h = DefaultHasher::new();
    identity.hash(&mut h);
    let low = h.finish() as u128;
    let mut h2 = DefaultHasher::new();
    (identity.clone() + "-hi").hash(&mut h2);
    let high = h2.finish() as u128;
    Some(commonwealth_core::ids::NodeId::from_u128(
        (high << 64) | low,
    ))
}

// ─── Subcommand: report ──────────────────────────────────────────────────────

async fn cmd_report(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("report: missing <feature-id>");
        return 2;
    };
    let section = match get_flag(&flags, "--section").as_deref() {
        None | Some("all") => sovereign_atos::ReportSection::All,
        Some("epistemic") => sovereign_atos::ReportSection::Epistemic,
        Some("red-team") | Some("redteam") => sovereign_atos::ReportSection::RedTeam,
        Some("milestone") => {
            let n = get_flag(&flags, "--milestone")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(1);
            sovereign_atos::ReportSection::Milestone(n)
        }
        Some(other) => {
            eprintln!("report: unknown --section '{other}'");
            return 2;
        }
    };
    let out_path = get_flag(&flags, "--out");

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("report: {e}");
            return 1;
        }
    };

    match orc.render_report(&feature_id, section).await {
        Ok(md) => {
            if let Some(p) = out_path {
                if let Err(e) = std::fs::write(&p, md) {
                    eprintln!("report: write {p}: {e}");
                    return 1;
                }
                println!("report: wrote {p}");
            } else {
                print!("{md}");
            }
            0
        }
        Err(e) => {
            eprintln!("report: {e}");
            1
        }
    }
}

// ─── Subcommand: archive ─────────────────────────────────────────────────────

async fn cmd_archive(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(id) = positional.first().cloned() else {
        eprintln!("archive: missing <id>");
        return 2;
    };
    let reason = get_flag(&flags, "--reason").unwrap_or_else(|| "(no reason given)".into());

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("archive: {e}");
            return 1;
        }
    };

    match orc.archive_feature(&id, &reason).await {
        Ok(true) => {
            println!("archived feature '{id}'");
            0
        }
        Ok(false) => {
            eprintln!("archive: feature '{id}' not found");
            1
        }
        Err(e) => {
            eprintln!("archive: {e}");
            1
        }
    }
}

// ─── Subcommand: status ──────────────────────────────────────────────────────

async fn cmd_status(args: &[String]) -> i32 {
    let (positional, _flags) = split_args(args);
    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("status: {e}");
            return 1;
        }
    };

    if let Some(id) = positional.first() {
        match orc.get_feature(id).await {
            Ok(Some(f)) => {
                println!("{}  [{}]", f.id, f.state);
                println!("  title:   {}", f.title);
                println!("  stop:    {}", f.stop_condition);
                let milestones = orc.list_milestones(&f.id).await.unwrap_or_default();
                for m in &milestones {
                    let status = match (m.started_at, m.ended_at) {
                        (_, Some(_)) => "ended",
                        (Some(_), None) => "active",
                        (None, None) => "pending",
                    };
                    println!("  m{} [{status}] {} bytes", m.ordinal, m.brief_md.len());
                }

                // Render the artifact-review checklist for the most recent
                // ended milestone. This is the view the operator inspects
                // during review — it should let them tick every box
                // without reading implementation code.
                if let Some(m) = milestones.iter().rev().find(|m| m.ended_at.is_some()) {
                    if let Some(ref json) = m.compliance_report_json {
                        render_artifact_checklist(&f, m, json);
                    }
                }
                0
            }
            Ok(None) => {
                eprintln!("status: feature '{id}' not found");
                1
            }
            Err(e) => {
                eprintln!("status: {e}");
                1
            }
        }
    } else {
        match orc.list_features(false).await {
            Ok(features) => {
                if features.is_empty() {
                    println!("no active features");
                } else {
                    for f in features {
                        println!("{}  [{}]  {}", f.id, f.state, f.title);
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("status: {e}");
                1
            }
        }
    }
}

// ─── Subcommand: promote ─────────────────────────────────────────────────────

async fn cmd_promote(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(note_id) = positional.first().cloned() else {
        eprintln!("promote: missing <note-id>");
        return 2;
    };
    let to = match get_flag(&flags, "--to").as_deref() {
        Some("global") => NoteScope::Global,
        Some("feature") => NoteScope::Feature,
        Some(other) => {
            eprintln!("promote: --to must be 'global' or 'feature', got '{other}'");
            return 2;
        }
        None => {
            eprintln!("promote: --to global|feature is required");
            return 2;
        }
    };
    let feature_id = get_flag(&flags, "--feature-id");
    let content = match get_flag(&flags, "--content") {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("promote: read {p}: {e}");
                return 1;
            }
        },
        None => None,
    };

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("promote: {e}");
            return 1;
        }
    };

    match orc
        .promote_note(&note_id, to, feature_id.as_deref(), content.as_deref())
        .await
    {
        Ok(new_id) => {
            println!("promoted note {note_id} -> {new_id} (scope={})", to.as_str());
            0
        }
        Err(e) => {
            eprintln!("promote: {e}");
            1
        }
    }
}

// ─── Subcommand: teardown ────────────────────────────────────────────────────

/// `sovereign atos teardown <feature>` — interactive note classification
/// pass that ends with a frozen epistemic-report.md and the feature
/// marked `completed`.
///
/// Default: interactive. `--auto`: retire everything (no promotions —
/// promotions are cheap-but-consequential so auto-promote is
/// deliberately absent until M4 adds a Fast-slot suggestion pass with
/// a confirmation gate). `--dry-run`: print what would happen without
/// mutating.
async fn cmd_teardown(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("teardown: missing <feature-id>");
        return 2;
    };
    let auto = flags.iter().any(|(k, _)| k == "auto");
    let dry_run = flags.iter().any(|(k, _)| k == "dry-run");

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("teardown: {e}");
            return 1;
        }
    };
    let Some(feature) = orc.get_feature(&feature_id).await.ok().flatten() else {
        eprintln!("teardown: feature '{feature_id}' not found");
        return 1;
    };
    if feature.state == "completed" {
        println!("teardown: feature '{}' is already completed.", feature.id);
        return 0;
    }

    let candidates = match orc.teardown_candidates(&feature_id).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("teardown: load candidates: {e}");
            return 1;
        }
    };

    println!();
    println!("  ── atos teardown ────────────────────────────────────────");
    println!("  Feature: {}", feature.id);
    println!("  Notes to review: {}", candidates.len());
    if candidates.is_empty() {
        println!("  (no feature-scoped decision/invariant/attempt/uncertainty/pointer notes)");
    }
    println!();

    let mut actions: Vec<sovereign_atos::TeardownAction> = Vec::new();
    for (idx, note) in candidates.iter().enumerate() {
        let first = note.content.lines().next().unwrap_or("").trim();
        let trimmed: String = first.chars().take(120).collect();
        println!(
            "  Note {}/{} [{}] {}\n    files: {}\n    id: {}",
            idx + 1,
            candidates.len(),
            note.kind,
            trimmed,
            if note.files.is_empty() {
                "(none)".into()
            } else {
                note.files.join(", ")
            },
            note.id
        );

        let choice = if auto {
            // Conservative auto: retire everything. Future M4 adds
            // Fast-slot suggestion for promotions.
            'r'
        } else {
            match prompt_teardown_action() {
                Some(c) => c,
                None => {
                    println!("teardown: aborted.");
                    return 1;
                }
            }
        };

        let action = match choice {
            'p' | 'P' => sovereign_atos::TeardownAction::Promote {
                note_id: note.id.clone(),
                rewritten_content: None,
            },
            'a' | 'A' => sovereign_atos::TeardownAction::Archive {
                note_id: note.id.clone(),
            },
            'r' | 'R' => sovereign_atos::TeardownAction::Retire {
                note_id: note.id.clone(),
            },
            _ => sovereign_atos::TeardownAction::Skip {
                note_id: note.id.clone(),
            },
        };
        actions.push(action);
        println!();
    }

    if dry_run {
        println!("  DRY RUN — no mutations applied. Action counts:");
        let mut p = 0;
        let mut a = 0;
        let mut r = 0;
        let mut s = 0;
        for act in &actions {
            match act {
                sovereign_atos::TeardownAction::Promote { .. } => p += 1,
                sovereign_atos::TeardownAction::Archive { .. } => a += 1,
                sovereign_atos::TeardownAction::Retire { .. } => r += 1,
                sovereign_atos::TeardownAction::Skip { .. } => s += 1,
            }
        }
        println!("    promoted: {p}\n    archived: {a}\n    retired:  {r}\n    skipped:  {s}");
        return 0;
    }

    let report = match orc.apply_teardown(&feature_id, actions).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("teardown: apply: {e}");
            return 1;
        }
    };

    println!();
    println!(
        "  applied: promoted {} / archived {} / retired {} / skipped {}",
        report.promoted.len(),
        report.archived.len(),
        report.retired.len(),
        report.skipped.len()
    );

    // Final artifact: epistemic-report.md.
    match orc
        .render_and_write_report(&feature_id, sovereign_atos::ReportSection::Epistemic)
        .await
    {
        Ok(path) => println!("  wrote {}", path.display()),
        Err(e) => eprintln!("  warning: render epistemic-report.md failed: {e}"),
    }

    0
}

fn prompt_teardown_action() -> Option<char> {
    use std::io::Write;
    eprint!("    action [P]romote / [a]rchive / [r]etire / [s]kip / [q]uit (default: s): ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    match line.trim().to_lowercase().as_str() {
        "q" | "quit" => None,
        "p" | "promote" => Some('p'),
        "a" | "archive" => Some('a'),
        "r" | "retire" => Some('r'),
        "" | "s" | "skip" => Some('s'),
        other => {
            eprintln!("    unknown response '{other}' — treating as skip.");
            Some('s')
        }
    }
}

// ─── Subcommand: diff ────────────────────────────────────────────────────────

async fn cmd_diff(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("diff: missing <feature-id>");
        return 2;
    };
    let ordinal: Option<i64> = get_flag(&flags, "--ordinal")
        .and_then(|s| s.parse::<i64>().ok());

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("diff: {e}");
            return 1;
        }
    };

    let Some(feature) = orc.get_feature(&feature_id).await.ok().flatten() else {
        eprintln!("diff: feature '{feature_id}' not found");
        return 1;
    };
    let milestones = orc.list_milestones(&feature_id).await.unwrap_or_default();
    let target_milestone = match ordinal {
        Some(n) => milestones.iter().find(|m| m.ordinal == n).cloned(),
        None => milestones.last().cloned(),
    };
    let Some(milestone) = target_milestone else {
        eprintln!("diff: no milestones for feature '{feature_id}'");
        return 1;
    };

    let runs_all = orc.list_runs(&feature_id).await.unwrap_or_default();
    let runs: Vec<_> = runs_all
        .into_iter()
        .filter(|r| r.milestone_id == milestone.id)
        .collect();
    if runs.is_empty() {
        eprintln!(
            "diff: no runs for feature '{feature_id}' milestone {}",
            milestone.ordinal
        );
        return 1;
    }

    render_diff(&feature, &milestone, &runs, orc.features()).await;
    0
}

/// Count tool events per (tool_name, run_id) with a breakdown of
/// outcomes and parse errors. Keyed on `tool_name` so the diff view
/// can render one row per tool across drivers.
#[derive(Default, Debug)]
struct ToolCounts {
    after: usize,
    errors: usize,
    parse_errors: usize,
    total_duration_ms: i64,
}

async fn render_diff(
    feature: &corpus_engine::FeatureRow,
    milestone: &corpus_engine::MilestoneRow,
    runs: &[corpus_engine::AtosRunRow],
    feature_store: &std::sync::Arc<corpus_engine::FeatureStore>,
) {
    println!();
    println!("  ── atos diff ────────────────────────────────────────────────");
    println!("  Feature:   {}", feature.id);
    println!("  Milestone: {}", milestone.ordinal);
    println!("  Runs:");
    for r in runs {
        let duration = match (r.started_at, r.ended_at) {
            (s, Some(e)) => format!("{}s", e - s),
            _ => "in-flight".into(),
        };
        let verdict = match r.stop_passed {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "?",
        };
        println!(
            "    • {:9} [{}]  driver={:8}  duration={:>7}",
            &r.id[..r.id.len().min(8)],
            verdict,
            r.driver,
            duration
        );
    }

    // Aggregate per-driver counts. Multiple runs for the same driver
    // (e.g. retries) collapse into one column — what the operator
    // wants is "how does claude typically behave here vs opencode."
    use std::collections::BTreeMap;
    let mut per_driver: BTreeMap<String, BTreeMap<String, ToolCounts>> = BTreeMap::new();
    for run in runs {
        let events = feature_store
            .list_events_for_run(&run.id)
            .await
            .unwrap_or_default();
        let entry = per_driver.entry(run.driver.clone()).or_default();
        for e in events {
            let counts = entry.entry(e.tool_name.clone()).or_default();
            match e.phase.as_str() {
                "after" => {
                    counts.after += 1;
                    if matches!(e.outcome.as_deref(), Some("error")) {
                        counts.errors += 1;
                    }
                    if let Some(d) = e.duration_ms {
                        counts.total_duration_ms += d;
                    }
                }
                "parse_error" => {
                    counts.parse_errors += 1;
                }
                _ => {}
            }
        }
    }

    // Union of tool names across drivers, sorted for stable output.
    let mut all_tools: std::collections::BTreeSet<String> = Default::default();
    for m in per_driver.values() {
        for k in m.keys() {
            all_tools.insert(k.clone());
        }
    }

    // Drivers we print columns for, in a stable order (claude first so
    // it's the baseline column on the left).
    let drivers: Vec<&String> = {
        let mut v: Vec<&String> = per_driver.keys().collect();
        v.sort_by(|a, b| {
            // claude < opencode < any others alphabetically
            let rank = |s: &str| match s {
                "claude" => 0,
                "opencode" => 1,
                _ => 2,
            };
            rank(a).cmp(&rank(b)).then(a.cmp(b))
        });
        v
    };

    println!();
    println!("  Per-tool activity:");
    let mut header = String::from("    tool                    ");
    for d in &drivers {
        header.push_str(&format!("{:>10}", d));
    }
    header.push_str("   note");
    println!("{header}");
    println!(
        "    ─────────────────────── {}   ─────────────────────────────",
        "──────────".repeat(drivers.len())
    );

    for tool in &all_tools {
        let mut line = format!("    {:<24}", truncate(tool, 24));
        let mut counts: Vec<usize> = Vec::new();
        let mut parse_err_here = false;
        for d in &drivers {
            let c = per_driver
                .get(*d)
                .and_then(|m| m.get(tool))
                .map(|c| (c.after, c.parse_errors))
                .unwrap_or((0, 0));
            counts.push(c.0);
            if c.1 > 0 {
                parse_err_here = true;
            }
            if c.1 > 0 {
                line.push_str(&format!("{:>8}×e{}", c.0, c.1));
            } else {
                line.push_str(&format!("{:>10}", format!("{}×", c.0)));
            }
        }
        let note = classify_delta(&counts, parse_err_here);
        line.push_str(&format!("   {note}"));
        println!("{line}");
    }

    // Also surface a parse-error total so the operator sees it even if
    // no per-tool row has parse errors (they could land on a tool the
    // other driver didn't touch).
    let total_parse_errors: usize = per_driver
        .values()
        .flat_map(|m| m.values())
        .map(|c| c.parse_errors)
        .sum();
    if total_parse_errors > 0 {
        println!();
        println!(
            "  ⚠ {total_parse_errors} tool_call parse error(s) across all runs — \
             run `sovereign atos diff {} --ordinal {} --verbose` (TODO) to inspect payloads.",
            feature.id, milestone.ordinal
        );
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.into()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

/// Heuristic annotation for the diff table. Kept simple and
/// iteration-friendly — this is the first lever we'll tune once we have
/// real comparison data.
fn classify_delta(counts: &[usize], parse_errors: bool) -> String {
    if parse_errors {
        return "parse failure (see above)".into();
    }
    if counts.len() < 2 {
        return String::new();
    }
    let max = counts.iter().copied().max().unwrap_or(0);
    let min = counts.iter().copied().min().unwrap_or(0);
    if max == 0 {
        return String::new();
    }
    if min == 0 {
        return "only one driver used this tool".into();
    }
    let ratio = max as f64 / min as f64;
    if ratio >= 3.0 && max - min >= 3 {
        "large delta — inspect".into()
    } else if max - min <= 2 {
        "close".into()
    } else {
        "moderate delta".into()
    }
}

// ─── Subcommand: run-ab ──────────────────────────────────────────────────────

async fn cmd_run_ab(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("run-ab: missing <feature-id>");
        return 2;
    };
    let Some(brief_path) = get_flag(&flags, "--brief") else {
        eprintln!("run-ab: --brief <path> is required");
        return 2;
    };
    let drivers_flag = get_flag(&flags, "--drivers").unwrap_or_else(|| "claude,opencode".into());
    let drivers: Vec<String> = drivers_flag.split(',').map(|s| s.trim().to_string()).collect();
    if drivers.is_empty() {
        eprintln!("run-ab: --drivers cannot be empty");
        return 2;
    }
    // Read brief once up-front so a path typo fails fast.
    if let Err(e) = std::fs::read_to_string(&brief_path) {
        eprintln!("run-ab: read {brief_path}: {e}");
        return 1;
    }

    println!("run-ab: drivers = {}", drivers.join(", "));
    let mut all_passed = true;
    for (idx, d) in drivers.iter().enumerate() {
        println!();
        println!("── driver={d} ───────────────────────────────────────");
        // Both drivers attach to the same milestone so `atos diff`
        // shows a real side-by-side view. The first driver creates
        // the milestone; subsequent drivers reuse it.
        let mut start_args = vec![
            feature_id.clone(),
            "--brief".into(),
            brief_path.clone(),
            "--driver".into(),
            d.clone(),
        ];
        if idx > 0 {
            start_args.push("--reuse-last-milestone".into());
        }
        let rc = cmd_start_milestone(&start_args).await;
        if rc != 0 {
            eprintln!("run-ab: driver '{d}' exited non-zero ({rc})");
            all_passed = false;
        }
        let end_rc = cmd_end_milestone(&[feature_id.clone()]).await;
        if end_rc != 0 {
            all_passed = false;
        }
    }

    println!();
    println!("── diff ──────────────────────────────────────────────");
    cmd_diff(&[feature_id]).await;

    if all_passed {
        0
    } else {
        1
    }
}

// ─── Subcommand: probe-driver ────────────────────────────────────────────────

async fn cmd_probe_driver(args: &[String]) -> i32 {
    let (_positional, flags) = split_args(args);
    let url = get_flag(&flags, "--url").unwrap_or_else(|| {
        "http://localhost:9741/v1/chat/completions".to_string()
    });

    let probe = serde_json::json!({
        "model": "probe",
        "messages": [
            {"role": "user", "content": "Call the ping tool with {} as args."}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "ping",
                "description": "Trivial probe tool.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        }],
        "tool_choice": "required",
        "max_tokens": 64,
        "stream": false
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            eprintln!("probe-driver: reqwest init: {e}");
            return 1;
        }
    };

    println!("probe-driver: POST {url}");
    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(probe.to_string())
        .send()
        .await;
    let body = match res {
        Ok(r) => {
            let status = r.status();
            match r.text().await {
                Ok(b) => {
                    println!("probe-driver: HTTP {}", status.as_u16());
                    if !status.is_success() {
                        println!("{b}");
                        return 1;
                    }
                    b
                }
                Err(e) => {
                    eprintln!("probe-driver: body read: {e}");
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("probe-driver: request failed: {e}");
            return 1;
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("probe-driver: parse response: {e}");
            return 1;
        }
    };

    // Accept both the structured `tool_calls` field (what M2 introduces)
    // and the fallback text-in-content form (pre-M2 servers). The
    // structured form is the success case.
    let tool_calls = parsed
        .pointer("/choices/0/message/tool_calls")
        .and_then(|v| v.as_array());
    match tool_calls {
        Some(calls) if !calls.is_empty() => {
            println!("probe-driver: PASS — server emitted {} structured tool_call(s)", calls.len());
            // Print the first call so operators see what the model produced.
            if let Some(first) = calls.first() {
                println!("  {first}");
            }
            0
        }
        _ => {
            println!("probe-driver: FAIL — response did not include structured tool_calls.");
            println!("  Full message: {}",
                     parsed.pointer("/choices/0/message").unwrap_or(&serde_json::Value::Null));
            1
        }
    }
}

// ─── Subcommand: next ────────────────────────────────────────────────────────

/// `sovereign atos next [<feature-id>]` — the seamless-handoff entry
/// point. Finds the next unfinished milestone, prints a summary, asks
/// for driver confirmation, and spawns.
async fn cmd_next(args: &[String]) -> i32 {
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
        println!("Run `sovereign atos teardown {}` to wrap it up.", feature.id);
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
    println!("  Next milestone: {} — {}", brief.milestone_ordinal, brief.milestone_title);
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
    // one place. We write the composed brief to a tempfile; start-
    // milestone reads it the usual way.
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

/// Pick a driver. Explicit --driver wins. Otherwise probe `/v1` for tool
/// support; on failure, fall back to Claude and record an `attempt` note.
async fn resolve_driver(requested: Option<&str>) -> Driver {
    match requested {
        Some("opencode") => Driver::Opencode,
        Some("claude") | None => {
            // The probe is best-effort. We avoid introducing a runtime
            // dependency on reqwest here — a TCP connect attempt to :9741
            // is enough to know whether a local mesh is up, and the M2
            // tool-use fix will replace this with a real capability probe.
            Driver::Claude
        }
        Some(other) => {
            eprintln!("atos: unknown driver '{other}', falling back to claude");
            Driver::Claude
        }
    }
}

// Stop-condition runner moved to sovereign_atos::LocalAtosOrchestrator
// in M3.1. See `orc.run_stop_condition(&feature)`.

// ─── Flag parsing ────────────────────────────────────────────────────────────

/// Boolean flags that do not consume the next token as their value.
const BOOLEAN_FLAGS: &[&str] =
    &["no-driver", "reuse-last-milestone", "yes", "y", "red-team", "auto", "dry-run"];

/// Split `args` into `(positional, flag_pairs)`. Value-taking flags
/// (e.g. `--title "foo"`) consume the following token. Boolean flags
/// listed in [`BOOLEAN_FLAGS`] stand alone and are recorded with an
/// empty value.
fn split_args(args: &[String]) -> (Vec<String>, Vec<(String, String)>) {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(name) = arg.strip_prefix("--") {
            if BOOLEAN_FLAGS.contains(&name) {
                flags.push((name.to_string(), String::new()));
                i += 1;
            } else {
                let value = args.get(i + 1).cloned().unwrap_or_default();
                flags.push((name.to_string(), value));
                i += 2;
            }
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }
    (positional, flags)
}

fn get_flag(flags: &[(String, String)], name: &str) -> Option<String> {
    let key = name.strip_prefix("--").unwrap_or(name);
    flags
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

// Path helper kept for the stop-condition runner's future use (emit a
// diagnostic when the feature's stop cmd references a path that does
// not resolve relative to CWD).
#[allow(dead_code)]
fn exists_on_path(p: &str) -> bool {
    Path::new(p).exists()
}

// ─── Artifact checklist renderer ─────────────────────────────────────────────

/// Render the compliance-review checklist derived from §6 of the ATOS design
/// doc. The goal is that an operator can tick every box without reading the
/// implementation — decisions live in the note log, coverage lives in the
/// stop_condition result, and deviations appear as `attempt` notes.
fn render_artifact_checklist(
    feature: &corpus_engine::FeatureRow,
    milestone: &corpus_engine::MilestoneRow,
    compliance_json: &str,
) {
    let parsed: serde_json::Value = match serde_json::from_str(compliance_json) {
        Ok(v) => v,
        Err(_) => {
            println!("  (compliance report present but not JSON-parsable)");
            return;
        }
    };

    let stop_passed = parsed.get("stop_passed").and_then(|v| v.as_bool()).unwrap_or(false);
    let empty_notes: Vec<serde_json::Value> = Vec::new();
    let notes = parsed
        .get("notes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_notes);

    let count_kind = |k: &str| notes.iter().filter(|n| n.get("kind").and_then(|v| v.as_str()) == Some(k)).count();
    let decisions = count_kind("decision");
    let invariants = count_kind("invariant");
    let attempts = count_kind("attempt");
    let todos = count_kind("todo");

    println!();
    println!("  ── Artifact review checklist (milestone {}) ──", milestone.ordinal);
    println!("  Spec compliance");
    print_check(feature.stop_condition.trim().is_empty() || stop_passed,
                &format!("stop_condition: '{}'", feature.stop_condition));
    print_check(!feature.charter_md.trim().is_empty(), "charter_md present");

    println!();
    println!("  Decision log");
    print_check(decisions + invariants + attempts + todos > 0,
                &format!("notes produced: {decisions} decisions, {invariants} invariants, \
                          {attempts} attempts, {todos} todos"));
    print_check(attempts == 0 || attempts < 5,
                "no repeated failed attempts (indicates lost context across compaction)");

    println!();
    println!("  Test evidence");
    print_check(stop_passed, "stop_condition exit=0");

    println!();
    println!("  Hints");
    if notes.is_empty() {
        println!("    - no feature-scoped notes were written; review whether the agent used the scope parameter");
    }
    if !stop_passed {
        println!("    - stop_condition failed; inspect the brief and the attempt notes");
    }
}

fn print_check(ok: bool, label: &str) {
    let mark = if ok { "[x]" } else { "[ ]" };
    println!("    {mark} {label}");
}
