// SPDX-License-Identifier: AGPL-3.0-or-later
//! Past runs — the shelf listing and the active-run census.
//!
//! Carved out of `deep_research_commands.rs` (ARCH §3.1).

use super::*;

/// One prior run on the shelf — read from its run dir's charter (live facts)
/// and manifest (close facts). A run without a manifest is `interrupted`
/// (the resume affordance's raw material).
#[derive(Debug, Serialize, Clone)]
pub struct DrRunSummary {
    pub run_id: String,
    pub question: Option<String>,
    pub created_at_unix: Option<i64>,
    /// The manifest's close-time state, or `None` when there is no
    /// manifest. ABSENCE IS REPORTED, NEVER DEFAULTED (§18.3): this field
    /// used to read `interrupted` whenever the manifest was missing, which
    /// made a run that was actively turning indistinguishable from one that
    /// had died — and put a Resume button next to it. Read it WITH `live`:
    /// live is "running", absent-and-not-live is genuinely interrupted.
    pub terminal_state: Option<String>,
    /// Is this process driving the run right now? From the live-run
    /// registry — the one decider.
    pub live: bool,
    pub rounds: usize,
    pub report_present: bool,
    pub consent: Option<DrConsent>,
}

/// List prior runs under the base, newest first (dr-<unix> sorts
/// chronologically).
#[tauri::command]
pub async fn dr_list_runs() -> Result<Vec<DrRunSummary>, String> {
    let base = runs_base();
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            let dir = e.path();
            let Some(run_id) = dir.file_name().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            if !dir.is_dir() || !run_id.starts_with("dr-") {
                continue;
            }
            let charter = std::fs::read(dir.join("charter.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Charter>(&raw).ok());
            let manifest = std::fs::read(dir.join("manifest.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok());
            let live = is_live(&run_id);
            out.push(DrRunSummary {
                run_id,
                question: charter.as_ref().map(|c| c.question.clone()),
                created_at_unix: charter.as_ref().map(|c| c.created_at_unix),
                terminal_state: manifest.as_ref().map(|m| m.terminal_state.clone()),
                live,
                rounds: manifest.as_ref().map(|m| m.rounds.len()).unwrap_or(0),
                report_present: dir.join("report.md").is_file(),
                consent: charter.and_then(|c| c.charter.consent).map(|c| DrConsent {
                    release_floor: c.release_floor.as_str().to_string(),
                    granted_at_unix: c.granted_at_unix,
                }),
            });
        }
    }
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    Ok(out)
}

/// One run this process is driving right now, with everything a view that
/// holds no handle needs to re-attach: the channel to listen on and when
/// this leg started.
#[derive(Debug, Serialize, Clone)]
pub struct DrActiveRun {
    pub run_id: String,
    pub channel: String,
    pub question: Option<String>,
    pub started_at_unix: i64,
}

/// Quit anyway, with research still running. Called only after the operator
/// has been told what is in flight and said to go ahead — the close handler
/// refuses on its own until then. The run dir keeps every artifact written
/// so far, so the run comes back as resumable rather than lost.
#[tauri::command]
pub async fn dr_quit_anyway(app: AppHandle) {
    tracing::info!(
        live_run = ?first_live_run_id(),
        "deep-research: operator chose to quit with a run in flight"
    );
    app.exit(0);
}

/// The runs this process is driving. A view that was unmounted when the
/// run began — or a webview that reloaded and lost its listener — recovers
/// the live run from here, instead of showing an empty composer while work
/// is in flight.
#[tauri::command]
pub async fn dr_active_runs() -> Vec<DrActiveRun> {
    let entries: Vec<(String, String, i64, PathBuf)> = {
        let guard = live_runs().lock().expect("live runs mutex");
        guard
            .iter()
            .map(|(id, r)| {
                (
                    id.clone(),
                    r.channel.clone(),
                    r.started_at_unix,
                    r.run_dir.clone(),
                )
            })
            .collect()
    };
    let mut out: Vec<DrActiveRun> = entries
        .into_iter()
        .map(|(run_id, channel, started_at_unix, run_dir)| DrActiveRun {
            run_id,
            channel,
            // The question is the charter's, read at call time: a resumed
            // leg was started with no question text of its own.
            question: std::fs::read(run_dir.join("charter.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Charter>(&raw).ok())
                .map(|c| c.question),
            started_at_unix,
        })
        .collect();
    out.sort_by(|a, b| b.started_at_unix.cmp(&a.started_at_unix));
    out
}
