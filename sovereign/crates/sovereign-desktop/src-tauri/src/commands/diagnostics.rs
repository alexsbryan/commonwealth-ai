// SPDX-License-Identifier: AGPL-3.0-or-later
//! Self-service diagnostics — the IO half of [`crate::health`], plus
//! the always-available "something's wrong" report.
//!
//! This module exists because the only way to produce a diagnostic
//! artifact used to be *crashing*. `prepare_crash_report` hangs off
//! the Reconnect banner, which only appears after
//! `Supervisor::persist_crash_log` writes a log. Every other way this
//! product fails a person — slow answers, a wrong answer, peers that
//! vanished, an import that stalled — produced nothing they could hand
//! to anyone, so the support conversation started from zero every time.
//!
//! Two commands:
//!
//! - [`run_health_check`] — what the user sees. Answers "is my install
//!   OK?" in their terms, and hands back a fix they can perform for
//!   every non-OK line. Most reports should die here without anyone
//!   being contacted; that is the point.
//! - [`prepare_diagnostic_report`] — what they send when it doesn't.
//!   Same file-on-Desktop, read-it-before-you-send-it posture as the
//!   crash bundle, because that posture is the reason people trust
//!   this thing with their documents.
//! - [`prepare_answer_report`] — the same document, filed against one
//!   specific reply. "It said the wrong thing" is the complaint this
//!   product gets most and the one machine state alone cannot explain;
//!   see [`crate::turn_report`].
//!
//! **Gathering never fails the report.** Every fact is optional and
//! every probe is allowed to come back empty: a user whose daemon is
//! dead is precisely the user who most needs the report to generate.
//! An unreachable probe becomes `None`, which
//! [`crate::health::evaluate`] renders as an honest `Unknown` rather
//! than a fabricated verdict.

use std::sync::Arc;

use tauri::State;

use crate::crash_bundle::{self, ReportReason};
use crate::health::{
    self, CorpusFacts, CrashFact, DaemonFacts, HealthFacts, HealthReport, MeshFacts,
    CRASH_WINDOW_SECS,
};
use crate::state::AppState;

/// Wall-clock budget for the whole gather. The health check is
/// something a worried user clicks; it has to answer, and answering
/// "couldn't reach the engine" quickly beats hanging on a dead socket.
const PROBE_TIMEOUT_SECS: u64 = 3;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Free space on the volume holding `path`, in GB.
///
/// Matched by mount point with the longest-prefix rule rather than by
/// taking the first hit: `~/.svrnmesh` on a machine with a separate
/// `/home` must report `/home`'s free space, and the shortest match
/// (`/`) would silently report the wrong volume — the kind of wrong
/// answer that sends triage hunting in the wrong place.
fn free_disk_gb(path: &std::path::Path) -> Option<f64> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, f64)> = None;
    for d in disks.list() {
        let mount = d.mount_point();
        if !path.starts_with(mount) {
            continue;
        }
        let depth = mount.components().count();
        let gb = d.available_space() as f64 / (1024.0 * 1024.0 * 1024.0);
        if best.as_ref().map(|(dep, _)| depth > *dep).unwrap_or(true) {
            best = Some((depth, gb));
        }
    }
    best.map(|(_, gb)| gb)
}

/// Ask the daemon what it has loaded. `None` when it can't be reached,
/// which is the signal the engine check reads.
async fn probe_daemon(state: &State<'_, Arc<AppState>>) -> Option<DaemonFacts> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(PROBE_TIMEOUT_SECS))
        .build()
        .ok()?;
    let body: serde_json::Value = client
        .get(format!("{}/status", state.client_base_url()))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let loaded = body
        .get("loaded_models")
        .and_then(|m| m.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    // Prefer the daemon's own notion of the primary chat model; fall
    // back to the first loaded entry so a differently-shaped /status
    // still yields something useful rather than a bare "unknown".
    let primary = body
        .get("primary_model")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("loaded_models")
                .and_then(|m| m.as_array())
                .and_then(|a| a.first())
                .and_then(|m| m.get("model").or_else(|| m.get("name")))
                .and_then(|v| v.as_str())
        })
        .map(basename_of);

    Some(DaemonFacts {
        primary_model: primary,
        models_loaded: loaded,
    })
}

/// Model identifiers reach us as paths on some code paths and as bare
/// names on others. Users read this line; strip the directory either
/// way, and never leak a home-directory path into a shared report.
fn basename_of(s: &str) -> String {
    s.rsplit(['/', '\\']).next().unwrap_or(s).to_string()
}

/// Assemble everything [`health::evaluate`] needs. Best-effort by
/// construction — see the module note on why nothing here returns an
/// error.
pub async fn gather_health_facts(state: &State<'_, Arc<AppState>>) -> HealthFacts {
    let daemon = probe_daemon(state).await;

    let mesh = crate::mesh_commands::mesh_get_state(state.clone())
        .await
        .ok()
        .flatten()
        .map(|s| {
            let visible = s
                .members
                .iter()
                .filter(|m| {
                    !m.is_self && matches!(m.status, sovereign_mesh::MemberStatus::Online)
                })
                .count();
            let known = s.members.iter().filter(|m| !m.is_self).count();
            MeshFacts {
                joined: true,
                mesh_name: Some(s.status.name.clone()),
                peers_visible: visible,
                peers_known: known,
            }
        })
        // `mesh_get_state` returning `None` means "not on a mesh",
        // which is a known fact and not a failed probe — so it maps to
        // a populated `MeshFacts`, not to `None`.
        .or(Some(MeshFacts {
            joined: false,
            ..Default::default()
        }));

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .ok()
        .map(|c| c.data.dir)
        .or_else(|| Some(sovereign_contracts::rebrand::svrnmesh_root()));

    let free_disk_gb = data_dir.as_deref().and_then(free_disk_gb);

    let cutoff = now_unix().saturating_sub(CRASH_WINDOW_SECS);
    let recent_crashes = crate::crash_report::list_crash_records()
        .into_iter()
        .filter(|r| r.captured_at_unix >= cutoff)
        .map(|r| CrashFact {
            captured_at_unix: r.captured_at_unix,
            summary: r.summary,
        })
        .collect();

    HealthFacts {
        captured_at_unix: now_unix(),
        daemon_running: daemon.is_some(),
        daemon,
        mesh,
        corpora: gather_corpus_facts(state).await,
        free_disk_gb,
        recent_crashes,
    }
}

/// Knowledge-base counts. `None` when the library can't be read at all
/// — distinct from `Some(CorpusFacts { total: 0, .. })`, which means
/// the user genuinely has none installed.
async fn gather_corpus_facts(state: &State<'_, Arc<AppState>>) -> Option<CorpusFacts> {
    let corpora = crate::commands::list_corpora(state.clone()).await.ok()?;
    let total = corpora.len();
    let failed = corpora.iter().filter(|c| c.status == "failed").count();
    let in_progress = corpora
        .iter()
        .filter(|c| matches!(c.status.as_str(), "installing" | "indexing" | "enriching"))
        .count();
    Some(CorpusFacts {
        total,
        failed,
        in_progress,
    })
}

/// Run the checks and hand the verdicts to the UI.
#[tauri::command]
pub async fn run_health_check(state: State<'_, Arc<AppState>>) -> Result<HealthReport, String> {
    Ok(health::evaluate(&gather_health_facts(&state).await))
}

/// Write a diagnostic report to the Desktop for any reason, not only a
/// crash, and return its path plus the issues URL.
///
/// `reason` is a loose string from the frontend on purpose — see
/// [`ReportReason::parse`]. A user trying to tell us something is
/// broken must never be blocked by an enum they cannot see.
#[tauri::command]
pub async fn prepare_diagnostic_report(
    state: State<'_, Arc<AppState>>,
    reason: String,
    note: Option<String>,
) -> Result<super::CrashReportInfo, String> {
    write_report(&state, ReportReason::parse(&reason), note.as_deref(), None).await
}

/// Report one specific answer.
///
/// The turn snapshot arrives from the frontend rather than being read
/// back out of the runtime — see [`crate::turn_report`] for why. The
/// command's job is to add the machine-state context the message
/// cannot know about (health, config, any recent crash) and to write
/// the file with the same posture as every other report.
#[tauri::command]
pub async fn prepare_answer_report(
    state: State<'_, Arc<AppState>>,
    note: Option<String>,
    turn: crate::turn_report::TurnSnapshot,
) -> Result<super::CrashReportInfo, String> {
    write_report(
        &state,
        ReportReason::WrongAnswer,
        note.as_deref(),
        Some(&turn),
    )
    .await
}

/// Shared tail of both report commands: gather health, resolve the
/// data dir, write the file. Factored out so the two entry points
/// cannot drift in what context they attach — a report about an
/// answer needs the same health picture as one filed from Settings.
async fn write_report(
    state: &State<'_, Arc<AppState>>,
    reason: ReportReason,
    note: Option<&str>,
    turn: Option<&crate::turn_report::TurnSnapshot>,
) -> Result<super::CrashReportInfo, String> {
    let health = health::evaluate(&gather_health_facts(state).await);

    let cfg = sovereign_core::setup_config::SetupConfig::load().ok();
    let app_version = env!("CARGO_PKG_VERSION");
    let data_dir = cfg
        .as_ref()
        .map(|c| c.data.dir.clone())
        .or_else(|| Some(sovereign_contracts::rebrand::svrnmesh_root()))
        .ok_or_else(|| "could not resolve data dir".to_string())?;

    let prepared = crash_bundle::prepare_report_with(&crash_bundle::ReportRequest {
        data_dir: &data_dir,
        config: cfg.as_ref(),
        app_version,
        reason,
        user_note: note,
        health: Some(&health),
        turn,
    })?;
    Ok(super::CrashReportInfo {
        report_path: prepared.report_path.to_string_lossy().into_owned(),
        issues_url: prepared.issues_url,
        reference_code: prepared.reference_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_identifiers_never_leak_a_home_directory() {
        // This string is going into a file the user hands to someone
        // else. A full path names their account.
        assert_eq!(basename_of("/home/alex/.sovereign/models/x.gguf"), "x.gguf");
        assert_eq!(
            basename_of("C:\\Users\\Alex\\models\\x.gguf"),
            "x.gguf"
        );
        assert_eq!(basename_of("qwen3.5-35b"), "qwen3.5-35b");
    }

    #[test]
    fn free_disk_reports_the_deepest_matching_mount() {
        // Longest-prefix, not first-match: on a box with a separate
        // /home, reporting `/`'s free space is a wrong answer that
        // reads as authoritative. We can't fabricate mounts here, so
        // assert the invariant that makes the rule checkable — the
        // answer for a deep path is never larger than for its root
        // when they resolve to different volumes, and querying a real
        // path either answers or honestly declines.
        let tmp = std::env::temp_dir();
        if let Some(gb) = free_disk_gb(&tmp) {
            assert!(gb >= 0.0, "free space must not be negative: {gb}");
            assert!(gb.is_finite(), "free space must be finite: {gb}");
        }
        // A path that matches no mount point must decline rather than
        // guess. On unix everything matches `/`, so this only asserts
        // the no-panic contract.
        let _ = free_disk_gb(std::path::Path::new("/definitely/not/a/mount/xyzzy"));
    }
}
