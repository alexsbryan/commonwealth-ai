// SPDX-License-Identifier: AGPL-3.0-or-later
//! `DaemonWorkflowRuntime` — the living trigger's daemon-side implementation.
//!
//! The watcher's `Worker` (in `sovereign-tools`) calls
//! [`WorkflowTriggerRuntime::dispatch`] when a sweep changes files on a folder
//! that has a `run_on_changes` workflow attached. This is the concrete runtime the
//! daemon installs: it resolves the workflow from the shared catalog and runs it
//! in-process against the daemon's own loopback inference, passing the changed
//! files as parameters.
//!
//! It lives here (daemon-side glue, not in the extractable `sovereign-workflow-host`
//! package) because it composes the monolith's watched-folder machinery
//! (`sovereign-tools`) with the package runner
//! ([`sovereign_workflow_host::run_workflow_in_process`]) — a binding only the
//! daemon needs. The `WorkflowTriggerRuntime` trait stays in `sovereign-tools` so
//! the worker can call across this boundary without depending on the workflow engine.
//!
//! Concurrency policy: **skip-if-in-flight**, per corpus. A folder sweeps at most
//! every ~60s, so there's no burst to debounce; the real hazard is a *long*
//! workflow (model steps) still running when the next sweep fires. Aborting it
//! mid-run would orphan side effects (a half-written brief, a partial corpus), so
//! instead we skip the new dispatch and let the next sweep after it finishes pick
//! up the accumulated changes. The content cache makes that re-run cheap.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use sovereign_tools::local_corpus::config::{LocalCorpusConfig, WatchedFolderConfig};
use sovereign_tools::local_corpus::watched::diff::WatchedDiff;
use sovereign_tools::local_corpus::watched::workflow_trigger::WorkflowTriggerRuntime;
use sovereign_workflow::Workflow;

use sovereign_workflow_host::{resolve_workflow_source, run_workflow_in_process};

/// Runs a watched folder's `run_on_changes` workflow on the daemon when a sweep
/// changes files. Install one via `WatchedSubsystem::install(.., Some(Arc::new(rt)))`.
pub struct DaemonWorkflowRuntime {
    /// The daemon's own base URL (e.g. `http://127.0.0.1:9741`) — triggered
    /// workflows route their `model:`/`embed:` steps back through it.
    daemon_url: String,
    /// Per-item concurrency for a triggered run.
    concurrency: usize,
    /// In-flight run per corpus (skip-if-in-flight). A finished handle is replaced
    /// on the next dispatch.
    in_flight: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
}

impl DaemonWorkflowRuntime {
    /// `daemon_url` is the loopback base (no `/v1` suffix — the host runner adds it).
    pub fn new(daemon_url: impl Into<String>) -> Self {
        Self {
            daemon_url: daemon_url.into(),
            concurrency: 4,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl WorkflowTriggerRuntime for DaemonWorkflowRuntime {
    async fn dispatch(
        &self,
        corpus_id: &str,
        config: &LocalCorpusConfig,
        watched_cfg: &WatchedFolderConfig,
        diff: &WatchedDiff,
    ) {
        let Some(workflow) = watched_cfg.run_on_changes.clone() else {
            return;
        };
        let corpus = corpus_id.to_string();

        // Skip if a run for this corpus is still going (see module docs). Also GC a
        // finished handle so the map doesn't accumulate.
        let mut guard = self.in_flight.lock().await;
        if let Some(h) = guard.get(&corpus) {
            if !h.is_finished() {
                tracing::info!(
                    corpus = %corpus,
                    workflow = %workflow,
                    "living-trigger: previous run still in flight — skipping this sweep"
                );
                return;
            }
        }

        // Capture owned context for the detached run.
        let folder = config.root_path.display().to_string();
        let changed: Vec<String> = diff
            .added
            .iter()
            .chain(diff.modified.iter())
            .cloned()
            .collect();
        let daemon_url = self.daemon_url.clone();
        let concurrency = self.concurrency;
        let corpus_for_task = corpus.clone();

        let handle = tokio::spawn(async move {
            run_trigger(
                &workflow,
                &daemon_url,
                concurrency,
                &corpus_for_task,
                &folder,
                &changed,
            )
            .await;
        });
        guard.insert(corpus, handle);
    }
}

/// Resolve + run a triggered workflow, injecting the trigger context as params:
/// `{param.folder}` (the watched root), `{param.corpus}` (the corpus id), and
/// `{param.changed}` (newline-joined relative paths of added/modified files).
/// Headless — outcomes go to `tracing` (the daemon's glassbox), never stdout.
async fn run_trigger(
    workflow: &str,
    daemon_url: &str,
    concurrency: usize,
    corpus: &str,
    folder: &str,
    changed: &[String],
) {
    let (toml, origin) = match resolve_workflow_source(workflow) {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!(corpus, workflow, error = %e, "living-trigger: unresolved workflow");
            return;
        }
    };
    let wf = match Workflow::parse(&toml) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(corpus, workflow = %origin, error = %e, "living-trigger: parse failed");
            return;
        }
    };

    let mut params = BTreeMap::new();
    params.insert("folder".to_string(), folder.to_string());
    params.insert("corpus".to_string(), corpus.to_string());
    params.insert("changed".to_string(), changed.join("\n"));

    tracing::info!(
        corpus,
        workflow = %origin,
        changed = changed.len(),
        "living-trigger: running workflow"
    );
    // Preserve the pre-extraction tool surface: `standard_registry` no longer
    // carries the corpus/atlas tools, so inject them here (the daemon links
    // sovereign-tools).
    // The daemon trigger historically passed no `extra_tools`, relying on the old
    // 16-tool `standard_registry`; the corpus/atlas tools are restored here (the
    // CLI's enrichment-authoring tools were never in the trigger path).
    // B:P9a: the embed-slot query-instruction prefix + chat context window are
    // now sourced by the runner from the daemon's own OICP manifest (loopback,
    // same box), so no `DEFAULT_MANIFEST` closure is threaded through here.
    let extra = sovereign_tools::workflow_corpus_tools();
    match run_workflow_in_process(&wf, daemon_url, concurrency, false, params, extra).await {
        Ok(report) => tracing::info!(
            corpus,
            workflow = %origin,
            ok = report.ok_count(),
            failed = report.failed_count(),
            "living-trigger: workflow finished"
        ),
        Err(e) => {
            tracing::warn!(corpus, workflow = %origin, error = %e, "living-trigger: workflow failed")
        }
    }
}
