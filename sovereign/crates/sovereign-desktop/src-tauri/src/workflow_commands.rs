// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands for the **Run a workflow** surface: list the runnable
//! workflows, describe what one can do (the consent bullets), and run one
//! in-process while streaming per-step progress to the UI.
//!
//! Running is in-process via
//! [`sovereign_workflow_host::run_workflow_with_provider`], fed the desktop's own
//! `AppState.inference` provider — so it works in both attach and embedded modes
//! and reuses exactly the provider chat uses, with no `/v1/models` round trip.
//! Progress is the Runner's [`WorkflowProgress`] observer, forwarded onto a
//! job-scoped Tauri channel the UI subscribes to — the same handle pattern as
//! `enrich_build_async`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::Serialize;
use sovereign_workflow::Workflow;
use sovereign_workflow_host::{
    first_comment_line, resolve_workflow_source, run_workflow_with_provider,
    summarize_capabilities, workflows_dir, HttpCorpusInstaller, StepObserver, WorkflowProgress,
    SHIPPED_WORKFLOWS,
};
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

// ── Catalog ────────────────────────────────────────────────────────────────

/// One runnable workflow + the inputs it needs at run time.
#[derive(Debug, Serialize, Clone)]
pub struct WorkflowCatalogEntry {
    pub name: String,
    pub description: String,
    /// `"shipped:<name>"` | `"user:<name>"` — where the TOML came from.
    pub origin: String,
    pub params: Vec<WorkflowParamSpec>,
}

/// One input field. `kind` lets the UI render a dedicated control for the
/// well-known folder/corpus/glob params and a plain text box for the rest.
#[derive(Debug, Serialize, Clone)]
pub struct WorkflowParamSpec {
    pub key: String,
    /// `"folder"` | `"corpus"` | `"glob"` | `"text"`.
    pub kind: String,
    pub label: String,
}

fn classify_param(key: &str) -> WorkflowParamSpec {
    let kind = match key {
        "folder" | "corpus" | "glob" => key,
        _ => "text",
    };
    WorkflowParamSpec {
        key: key.to_string(),
        kind: kind.to_string(),
        label: key.to_string(),
    }
}

fn catalog_entry(name: &str, origin: String, toml: &str) -> Option<WorkflowCatalogEntry> {
    let wf = Workflow::parse(toml).ok()?;
    let params = wf
        .referenced_params()
        .into_iter()
        .map(|k| classify_param(&k))
        .collect();
    Some(WorkflowCatalogEntry {
        name: name.to_string(),
        description: first_comment_line(toml),
        origin,
        params,
    })
}

/// List the workflows a user can run: their own (`~/.sovereign/workflows/`, which
/// shadow shipped starters of the same name) plus the shipped starters.
#[tauri::command]
pub async fn workflow_list_runnable() -> Result<Vec<WorkflowCatalogEntry>, String> {
    let mut entries: Vec<WorkflowCatalogEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // The user's own workflows first — a same-named file shadows the shipped one.
    if let Ok(rd) = std::fs::read_dir(workflows_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(toml) = std::fs::read_to_string(&p) else {
                continue;
            };
            if let Some(entry) = catalog_entry(stem, format!("user:{stem}"), &toml) {
                seen.insert(stem.to_string());
                entries.push(entry);
            }
        }
    }
    for (name, toml) in SHIPPED_WORKFLOWS {
        if seen.contains(*name) {
            continue;
        }
        if let Some(entry) = catalog_entry(name, format!("shipped:{name}"), toml) {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// The plain-language things a workflow can do (write files, use your local
/// model, fetch the network…) — the same consent bullets the living trigger
/// shows, so the user sees what a run will do before starting it.
#[tauri::command]
pub async fn workflow_capabilities(name_or_path: String) -> Result<Vec<String>, String> {
    let (toml, _origin) = resolve_workflow_source(&name_or_path)?;
    let wf = Workflow::parse(&toml).map_err(|e| format!("workflow parse: {e}"))?;
    Ok(summarize_capabilities(&wf).await.describe())
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
    pub corpus: Option<String>,
}

/// A frontend-facing progress event: the Runner's [`WorkflowProgress`] plus the
/// terminal `complete`/`failed` the command appends after the run. A tagged union
/// on `kind` (matching the `WorkflowRunProgress` TS type).
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

impl From<WorkflowProgress> for WorkflowRunEvent {
    fn from(p: WorkflowProgress) -> Self {
        match p {
            WorkflowProgress::RunStarted {
                workflow,
                items,
                steps,
            } => Self::RunStarted {
                workflow,
                items,
                steps,
            },
            WorkflowProgress::StepDone {
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
            WorkflowProgress::ElementSkipped {
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
            WorkflowProgress::ItemDone {
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
            WorkflowProgress::RunFinished { ok, failed } => Self::RunFinished { ok, failed },
        }
    }
}

/// Resolve + run a workflow in-process, streaming progress on a job-scoped
/// channel. Returns the handle immediately; the run proceeds on a background task
/// and the terminal `complete`/`failed` event lands on the channel.
///
/// `params` carries the whole form — `folder`/`corpus`/`glob` and any extra
/// `{param.*}` the workflow declares. `corpus` is auto-derived from the folder
/// basename when the workflow builds a corpus but none was supplied (mirroring the
/// CLI's `cmd_run` ergonomics).
#[tauri::command]
pub async fn workflow_run(
    app: AppHandle,
    state: State<'_, AppState>,
    name_or_path: String,
    params: BTreeMap<String, String>,
) -> Result<WorkflowRunHandle, String> {
    let (toml, _origin) = resolve_workflow_source(&name_or_path)?;
    let wf = Workflow::parse(&toml).map_err(|e| format!("workflow parse: {e}"))?;

    let mut params = params;
    if !params.contains_key("corpus") {
        if let Some(folder) = params.get("folder") {
            if let Some(base) = std::path::Path::new(folder)
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|b| !b.is_empty())
            {
                params.insert("corpus".into(), base.to_string());
            }
        }
    }

    // The corpus this run will build, for the "chat with it" handoff: only when a
    // store step is present and a corpus name resolved.
    let builds_corpus = wf.steps.iter().any(|s| s.uses == "tool:corpus_store");
    let corpus = builds_corpus.then(|| params.get("corpus").cloned()).flatten();

    // Reuse the desktop's own inference provider (attach: a SplitInferenceProvider
    // to the daemon; embedded: in-process) rather than re-discovering models.
    let inference = {
        let guard = state.inference.read().await;
        guard.as_ref().map(Arc::clone)
    };

    let job_id = uuid::Uuid::new_v4().to_string();
    let channel = progress_channel(&job_id);

    // Observer → job-scoped Tauri channel. Failed emits are swallowed (the UI
    // window may have closed) — they must not abort a running workflow.
    let observer: StepObserver = {
        let app = app.clone();
        let channel = channel.clone();
        Arc::new(move |ev: WorkflowProgress| {
            let _ = app.emit(&channel, WorkflowRunEvent::from(ev));
        })
    };

    let installer = Arc::new(HttpCorpusInstaller::new());
    let app_terminal = app.clone();
    let channel_terminal = channel.clone();
    let corpus_terminal = corpus.clone();
    tokio::spawn(async move {
        let terminal = match run_workflow_with_provider(
            &wf,
            inference,
            Some(installer),
            4,
            false,
            params,
            vec![],
            Some(observer),
        )
        .await
        {
            Ok(report) => {
                let ok = report.ok_count();
                WorkflowRunEvent::Complete {
                    ok,
                    failed: report.failed_count(),
                    // Only surface the corpus when at least one item succeeded —
                    // an all-failed run produced nothing to chat with.
                    corpus: if ok > 0 { corpus_terminal } else { None },
                }
            }
            Err(error) => WorkflowRunEvent::Failed { error },
        };
        let _ = app_terminal.emit(&channel_terminal, terminal);
    });

    Ok(WorkflowRunHandle {
        job_id,
        channel,
        corpus,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_param_tags_the_well_known_keys() {
        assert_eq!(classify_param("folder").kind, "folder");
        assert_eq!(classify_param("corpus").kind, "corpus");
        assert_eq!(classify_param("glob").kind, "glob");
        // Anything else is a plain text field.
        assert_eq!(classify_param("outdir").kind, "text");
    }

    #[test]
    fn catalog_entry_classifies_a_workflows_params() {
        let toml = r#"# turn a folder into a cited notebook
[workflow]
name = "notebook"
[source]
type = "folder"
path = "{param.folder}"
glob = "{param.glob}"
[[step]]
id = "store"
uses = "transform:identity"
input = "x"
params = { corpus = "{param.corpus}" }
"#;
        let entry = catalog_entry("notebook", "shipped:notebook".to_string(), toml).unwrap();
        assert_eq!(entry.name, "notebook");
        assert_eq!(entry.origin, "shipped:notebook");
        assert!(entry.description.contains("cited notebook"), "{}", entry.description);
        let kinds: std::collections::BTreeMap<_, _> = entry
            .params
            .iter()
            .map(|p| (p.key.as_str(), p.kind.as_str()))
            .collect();
        assert_eq!(kinds.get("folder"), Some(&"folder"));
        assert_eq!(kinds.get("corpus"), Some(&"corpus"));
        assert_eq!(kinds.get("glob"), Some(&"glob"));
    }
}
