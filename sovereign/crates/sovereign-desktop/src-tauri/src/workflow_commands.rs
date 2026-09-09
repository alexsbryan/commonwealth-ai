// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands for the **Run a workflow** surface: list the runnable
//! workflows, describe what one can do (the consent bullets), and run one as
//! a DAEMON job while streaming per-step progress to the UI.
//!
//! sv-surface rung 5 (2026-09-09): execution moved daemon-side. This module
//! is a client of the daemon's `/internal/workflows/*` routes (defined in
//! `sovereign-workflow-host::workflow_http` — the wire types below are
//! imported from there, the same one-definition rule `watched_folder_commands`
//! follows), in BOTH boot modes: the embedded desktop daemon serves the same
//! routes in-process, and attach hits the external daemon's port. The run
//! POSTs a job, then a poll task bridges the job's events onto the SAME
//! job-scoped Tauri channel with the SAME event shape the frontend already
//! renders — the UI is unchanged.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use sovereign_workflow_host::workflow_http::{
    CapabilitiesResponse, JobResponse, RunRequest, RunResponse, WorkflowJobEvent,
    WorkflowListEntry, WorkflowListResponse,
};

use crate::state::AppState;

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

// ── Catalog ────────────────────────────────────────────────────────────────

/// List the workflows a user can run. The catalog (user workflows shadowing
/// shipped starters) is the DAEMON's — one home, served by the route.
#[tauri::command]
pub async fn workflow_list_runnable(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<WorkflowListEntry>, String> {
    let base = state.client_base_url();
    let resp: WorkflowListResponse = http_client()?
        .get(format!("{base}/internal/workflows/list"))
        .send()
        .await
        .map_err(|e| format!("workflow list: {e}"))?
        .error_for_status()
        .map_err(|e| format!("workflow list: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse workflow list: {e}"))?;
    Ok(resp.workflows)
}

/// The plain-language things a workflow can do (write files, use your local
/// model, fetch the network…) — the same consent bullets the living trigger
/// shows, so the user sees what a run will do before starting it.
#[tauri::command]
pub async fn workflow_capabilities(
    state: State<'_, Arc<AppState>>,
    name_or_path: String,
) -> Result<Vec<String>, String> {
    let base = state.client_base_url();
    let resp: CapabilitiesResponse = http_client()?
        .get(format!("{base}/internal/workflows/capabilities"))
        .query(&[("name", name_or_path)])
        .send()
        .await
        .map_err(|e| format!("workflow capabilities: {e}"))?
        .error_for_status()
        .map_err(|e| format!("workflow capabilities: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse workflow capabilities: {e}"))?;
    Ok(resp.bullets)
}

// ── Run ──────────────────────────────────────────────────────────────────────

fn progress_channel(job_id: &str) -> String {
    format!("workflow://progress/{job_id}")
}

#[derive(Debug, Serialize, Clone)]
pub struct WorkflowRunHandle {
    pub job_id: String,
    pub channel: String,
    /// The corpus this run will build (it has a `tool:corpus_store` step and a
    /// resolved `corpus` param) — so the UI can offer "chat with it" on success.
    /// Derived daemon-side by the run route; one home.
    pub corpus: Option<String>,
}

/// A frontend-facing progress event: the job's wire events plus the terminal
/// `complete`/`failed`. A tagged union on `kind` (matching the
/// `WorkflowRunProgress` TS type) — UNCHANGED from the in-process era, so the
/// frontend needs no edit.
#[derive(Debug, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowRunEvent {
    RunStarted {
        workflow: String,
        items: usize,
        steps: usize,
    },
    StepDone {
        item: String,
        step: String,
        uses: String,
        for_each: bool,
        cached: bool,
        step_index: usize,
        total_steps: usize,
    },
    ElementSkipped {
        item: String,
        step: String,
        index: usize,
        error: String,
    },
    ItemDone {
        item: String,
        ok: bool,
        ran: usize,
        cached: usize,
    },
    RunFinished {
        ok: usize,
        failed: usize,
    },
    /// Terminal: the run finished and (if it built a corpus) it's searchable.
    Complete {
        ok: usize,
        failed: usize,
        corpus: Option<String>,
    },
    /// Terminal: the whole run errored before producing a report.
    Failed {
        error: String,
    },
}

impl From<WorkflowJobEvent> for WorkflowRunEvent {
    fn from(ev: WorkflowJobEvent) -> Self {
        match ev {
            WorkflowJobEvent::RunStarted {
                workflow,
                items,
                steps,
            } => Self::RunStarted {
                workflow,
                items,
                steps,
            },
            WorkflowJobEvent::StepDone {
                item,
                step,
                uses,
                for_each,
                cached,
                step_index,
                total_steps,
            } => Self::StepDone {
                item,
                step,
                uses,
                for_each,
                cached,
                step_index,
                total_steps,
            },
            WorkflowJobEvent::ElementSkipped {
                item,
                step,
                index,
                error,
            } => Self::ElementSkipped {
                item,
                step,
                index,
                error,
            },
            WorkflowJobEvent::ItemDone {
                item,
                ok,
                ran,
                cached,
            } => Self::ItemDone {
                item,
                ok,
                ran,
                cached,
            },
            WorkflowJobEvent::RunFinished { ok, failed } => Self::RunFinished { ok, failed },
            // The per-item outcomes are the CLI's print material; the UI
            // shape carries only the tallies + corpus handoff.
            WorkflowJobEvent::Complete {
                ok, failed, corpus, ..
            } => Self::Complete { ok, failed, corpus },
            WorkflowJobEvent::Failed { error } => Self::Failed { error },
        }
    }
}

/// How often the poll task bridges new job events onto the Tauri channel.
const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// Submit a workflow run to the daemon and bridge its events onto a
/// job-scoped channel. Returns the handle immediately; a background poll task
/// forwards events and stops at the terminal `complete`/`failed`.
///
/// `params` carries the whole form — `folder`/`corpus`/`glob` and any extra
/// `{param.*}` the workflow declares. The `corpus` default (folder basename)
/// and the built-corpus derivation are the daemon's.
#[tauri::command]
pub async fn workflow_run(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    name_or_path: String,
    params: BTreeMap<String, String>,
) -> Result<WorkflowRunHandle, String> {
    let base = state.client_base_url();
    let client = http_client()?;
    let run: RunResponse = client
        .post(format!("{base}/internal/workflows/run"))
        .json(&RunRequest {
            name_or_path,
            params,
            toml: None,
            concurrency: None,
            no_cache: None,
        })
        .send()
        .await
        .map_err(|e| format!("workflow run: {e}"))?
        .error_for_status()
        .map_err(|e| format!("workflow run: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse workflow run ack: {e}"))?;

    let channel = progress_channel(&run.job_id);
    let channel_for_handle = channel.clone();

    // Poll bridge → job-scoped Tauri channel. Failed emits are swallowed (the
    // UI window may have closed) — they must not abort the bridge.
    let job_id = run.job_id.clone();
    tokio::spawn(async move {
        let client = match http_client() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut after = 0u64;
        loop {
            let resp = client
                .get(format!(
                    "{base}/internal/workflows/jobs/{job_id}?after={after}"
                ))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => match r.json::<JobResponse>().await {
                    Ok(job) => {
                        let terminal = job.status
                            != sovereign_workflow_host::workflow_http::JobStatus::Running;
                        for event in job.events {
                            after = after.max(event.seq);
                            let _ = app.emit(&channel, WorkflowRunEvent::from(event.event));
                        }
                        if terminal {
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(job = %job_id, error = %e, "workflow poll: bad job body");
                    }
                },
                Ok(r) => {
                    // A 404 past the retention window, or a restarting daemon:
                    // report the break once and stop bridging rather than
                    // spinning on a gone job.
                    tracing::warn!(
                        job = %job_id,
                        status = %r.status(),
                        "workflow poll: job fetch failed — stopping the bridge"
                    );
                    let _ = app.emit(
                        &channel,
                        WorkflowRunEvent::Failed {
                            error: format!("job status fetch returned {}", r.status()),
                        },
                    );
                    return;
                }
                Err(e) => {
                    tracing::warn!(job = %job_id, error = %e, "workflow poll: retrying");
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });

    Ok(WorkflowRunHandle {
        job_id: run.job_id,
        channel: channel_for_handle,
        corpus: run.corpus,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every wire event maps onto the frontend shape the TS type already
    /// declares — the UI contract this conversion must not disturb.
    #[test]
    fn wire_events_map_onto_the_frontend_shape() {
        let ev = WorkflowRunEvent::from(WorkflowJobEvent::RunStarted {
            workflow: "notebook".into(),
            items: 3,
            steps: 4,
        });
        match ev {
            WorkflowRunEvent::RunStarted {
                workflow,
                items,
                steps,
            } => {
                assert_eq!(workflow, "notebook");
                assert_eq!((items, steps), (3, 4));
            }
            other => panic!("wrong shape: {other:?}"),
        }

        // The terminal keeps the corpus handoff and drops the CLI's per-item
        // print material.
        let ev = WorkflowRunEvent::from(WorkflowJobEvent::Complete {
            workflow: "notebook".into(),
            ok: 2,
            failed: 0,
            corpus: Some("notes".into()),
            items: vec![sovereign_workflow_host::workflow_http::JobItemOutcome {
                item: "a.md".into(),
                ok: true,
                output: Some("stored 3 chunks".into()),
                error: None,
                ran: 4,
                cached: 0,
            }],
        });
        match ev {
            WorkflowRunEvent::Complete { ok, failed, corpus } => {
                assert_eq!((ok, failed), (2, 0));
                assert_eq!(corpus.as_deref(), Some("notes"));
            }
            other => panic!("wrong shape: {other:?}"),
        }
    }
}
