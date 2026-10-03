// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workflow job wire shapes — `/internal/workflows/*`
//! (`sovereign_workflow_host::workflow_http`). Moved here at sv-surface
//! svt-3 (2026-09-11): the desktop's `workflow_commands` and the CLI's
//! `workflow_cmd` are CLIENTS of that surface, and naming its answers cost
//! the desktop a `sovereign-desktop -> sovereign-workflow-host` layer edge —
//! the workflow ENGINE, linked to parse a job id. `workflow_http` re-exports
//! every item at its historical path; the routes and the CLI are unchanged.
//!
//! Every shape here is serde over primitives. `WorkflowJobEvent` mirrors
//! the runner's `WorkflowProgress` variant for variant plus the two
//! terminal arms the host appends; the projection between them is
//! `workflow_http::job_event_from_progress` (a free function — the `From`
//! impl could not follow the type down, orphan rule).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One runnable workflow + the inputs it needs at run time — the shape the
/// desktop's Run-a-workflow view renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowListEntry {
    /// The workflow's name (its catalog key).
    pub name: String,
    /// The definition's first comment line.
    pub description: String,
    /// `"shipped:<name>"` | `"user:<name>"` | the resolved file path.
    pub origin: String,
    /// The `${param}` inputs the definition references.
    pub params: Vec<WorkflowParamSpec>,
}

/// One input field. `kind` lets the UI render a dedicated control for the
/// well-known folder/corpus/glob params and a plain text box for the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowParamSpec {
    /// The param name as referenced in the definition.
    pub key: String,
    /// `"folder"` | `"corpus"` | `"glob"` | `"text"`.
    pub kind: String,
    /// Display label (the key, today).
    pub label: String,
}

/// Query of `GET /internal/workflows/capabilities`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CapabilitiesQuery {
    /// The workflow to describe.
    pub name: String,
}

/// Answer of `GET /internal/workflows/capabilities`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    /// The workflow described.
    pub name: String,
    /// Plain-language consent bullets ("run shell commands", "use your local
    /// model"…) — the trust gate the caller shows before starting a run.
    pub bullets: Vec<String>,
}

/// Body for `POST /run`. `toml` is set when the CALLER already resolved a
/// file (the CLI with a local path against a remote `--daemon`); otherwise
/// the daemon resolves `name_or_path` from its own catalog — same
/// `~/.svrnmesh/workflows` dir, same shadowing rules.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunRequest {
    /// A catalog name or a path the daemon resolves.
    pub name_or_path: String,
    /// The definition itself, when the caller already read it.
    #[serde(default)]
    pub toml: Option<String>,
    /// `${param}` values.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Item concurrency override.
    #[serde(default)]
    pub concurrency: Option<usize>,
    /// Bypass the step cache.
    #[serde(default)]
    pub no_cache: Option<bool>,
}

/// Answer of `POST /run`.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunResponse {
    /// The host's job id; poll `GET /jobs/{id}` with it.
    pub job_id: String,
    /// Where the definition came from (echoed so a client can show it).
    pub origin: String,
    /// The corpus this run will build (it has a `tool:corpus_store` step and
    /// a resolved `corpus` param) — so the UI can offer "chat with it".
    pub corpus: Option<String>,
}

/// Query of `GET /jobs/{id}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct JobQuery {
    /// Only events with `seq > after` are returned. Default 0 = everything.
    #[serde(default)]
    pub after: u64,
}

/// A job's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Still running; poll again.
    Running,
    /// Terminal: the run produced a report.
    Complete,
    /// Terminal: the run errored before producing a report.
    Failed,
}

/// Answer of `GET /jobs/{id}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct JobResponse {
    /// The job asked about.
    pub job_id: String,
    /// Its state at the time of the read.
    pub status: JobStatus,
    /// Where the definition came from.
    pub origin: String,
    /// Retained events with `seq > after`.
    pub events: Vec<JobEvent>,
}

/// One retained progress event with its monotonic cursor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEvent {
    /// Monotonic per job; a client polls with `?after=` the last it saw.
    pub seq: u64,
    /// The event, flattened beside `seq`.
    #[serde(flatten)]
    pub event: WorkflowJobEvent,
}

/// The wire progress enum: the Runner's `WorkflowProgress` variants plus
/// the terminal `complete`/`failed` the host appends. Tagged on `kind`
/// (snake_case) so a client can switch on it directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum WorkflowJobEvent {
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
    /// Terminal: the run produced a report. `corpus` is the built corpus
    /// when at least one item succeeded and the workflow stores one.
    Complete {
        workflow: String,
        ok: usize,
        failed: usize,
        corpus: Option<String>,
        items: Vec<JobItemOutcome>,
    },
    /// Terminal: the whole run errored before producing a report.
    Failed {
        error: String,
    },
}

/// One item's outcome in the terminal `complete` event — the per-item report
/// the CLI prints (`## item` + output / error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobItemOutcome {
    /// The item's name.
    pub item: String,
    /// Every step ran.
    pub ok: bool,
    /// The last step's output, when it succeeded.
    pub output: Option<String>,
    /// The failing step's error, when it did not.
    pub error: Option<String>,
    /// Steps executed.
    pub ran: usize,
    /// Steps served from the cache.
    pub cached: usize,
}

/// Answer of `GET /internal/workflows/list`.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkflowListResponse {
    /// The runnable catalog, sorted by name.
    pub workflows: Vec<WorkflowListEntry>,
}
