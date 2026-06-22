// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single-process Runner: topo-order the steps, run each item's graph
//! threading `Artifact`s via `template::resolve_args`, with bounded per-item
//! concurrency and one tracing event per step (the `retrieval_pipeline` shape).
//! Durable/distributed execution is P2 (the pipeline tool as an outer loop).

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use sovereign_core::error::Result;
use sovereign_core::types::StepOutput;

use crate::model::{Scope, Workflow};
use crate::steps::{Step, StepCtx, StepRegistry};
use crate::template;

/// Outcome for one item: the final step's text, or an error message.
#[derive(Debug)]
pub struct ItemReport {
    pub item: String,
    pub result: std::result::Result<String, String>,
}

#[derive(Debug)]
pub struct RunReport {
    pub workflow: String,
    pub items: Vec<ItemReport>,
}

impl RunReport {
    pub fn ok_count(&self) -> usize {
        self.items.iter().filter(|i| i.result.is_ok()).count()
    }
    pub fn failed_count(&self) -> usize {
        self.items.iter().filter(|i| i.result.is_err()).count()
    }
}

pub struct Runner {
    registry: StepRegistry,
}

impl Runner {
    pub fn new(registry: StepRegistry) -> Self {
        Self { registry }
    }

    /// Run `wf` over its source items (or once if it has no `[source]`), with up
    /// to `concurrency` items in flight.
    pub async fn run(&self, wf: &Workflow, concurrency: usize) -> Result<RunReport> {
        // Resolve the step graph once (cheap — Arc clones); fails fast on an
        // unknown `uses` so a bad workflow errors before any item runs.
        let steps: Vec<Arc<dyn Step>> = wf
            .steps
            .iter()
            .map(|s| self.registry.resolve(&s.uses))
            .collect::<Result<_>>()?;
        let order = wf.topo_order()?;
        let items = match &wf.source {
            Some(src) => src.enumerate()?,
            None => vec![BTreeMap::new()],
        };
        tracing::info!(
            target: "workflow",
            workflow = %wf.name, items = items.len(), steps = wf.steps.len(),
            "workflow: run start"
        );

        let reports: Vec<ItemReport> = stream::iter(items.into_iter())
            .map(|item| run_item(wf, &steps, &order, item))
            .buffer_unordered(concurrency.max(1))
            .collect()
            .await;

        Ok(RunReport {
            workflow: wf.name.clone(),
            items: reports,
        })
    }
}

async fn run_item(
    wf: &Workflow,
    steps: &[Arc<dyn Step>],
    order: &[usize],
    item: BTreeMap<String, String>,
) -> ItemReport {
    let item_id = item.get("name").cloned().unwrap_or_else(|| "·".into());
    let mut scope = Scope {
        item,
        completed: BTreeMap::new(),
    };
    let mut last_text = String::new();

    for &i in order {
        let spec = &wf.steps[i];
        let args = template::resolve_args(spec, &scope);
        let ctx = StepCtx {
            item_id: item_id.clone(),
        };
        let start = std::time::Instant::now();
        match steps[i].run(&args, &ctx).await {
            Ok(artifact) => {
                last_text = output_text(&artifact.output);
                tracing::info!(
                    target: "workflow",
                    item = %item_id, step = %spec.id, uses = %spec.uses,
                    ms = start.elapsed().as_millis() as u64,
                    "workflow: step ok"
                );
                scope.completed.insert(spec.id.clone(), artifact);
            }
            Err(e) => {
                tracing::warn!(
                    target: "workflow",
                    item = %item_id, step = %spec.id, uses = %spec.uses, error = %e,
                    "workflow: step failed — item aborted"
                );
                return ItemReport {
                    item: item_id,
                    result: Err(format!("step `{}`: {e}", spec.id)),
                };
            }
        }
    }
    ItemReport {
        item: item_id,
        result: Ok(last_text),
    }
}

fn output_text(o: &StepOutput) -> String {
    match o {
        StepOutput::Text(s) => s.clone(),
        StepOutput::Json(v) => serde_json::to_string(v).unwrap_or_default(),
        StepOutput::ReasonWithToolsResult { text, .. } => text.clone(),
        StepOutput::Jump(_) | StepOutput::Skipped => String::new(),
    }
}
