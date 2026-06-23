// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single-process Runner: topo-order the steps, run each item's graph
//! threading `Artifact`s via `template::resolve_args`, with bounded per-item
//! concurrency, a content-addressed cache (skip a `Read` step on a key hit),
//! and one tracing event per step. Durable/distributed execution is P2 (the
//! pipeline tool as an outer loop).

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use sovereign_core::error::Result;
use sovereign_core::types::StepOutput;

use crate::cache::{ArtifactCache, NoCache};
use crate::model::{Scope, SourceItem, Workflow};
use crate::steps::{Step, StepCtx, StepRegistry};
use crate::{cache, template};

/// Outcome for one item: the final step's text (or an error), plus how many of
/// its steps ran vs. were served from the cache.
#[derive(Debug)]
pub struct ItemReport {
    pub item: String,
    pub result: std::result::Result<String, String>,
    pub ran: usize,
    pub cached: usize,
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
    pub fn ran_total(&self) -> usize {
        self.items.iter().map(|i| i.ran).sum()
    }
    pub fn cached_total(&self) -> usize {
        self.items.iter().map(|i| i.cached).sum()
    }
}

pub struct Runner {
    registry: StepRegistry,
    cache: Arc<dyn ArtifactCache>,
}

impl Runner {
    /// A runner with no cache (every step runs). The default.
    pub fn new(registry: StepRegistry) -> Self {
        Self {
            registry,
            cache: Arc::new(NoCache),
        }
    }

    /// A runner backed by a content-addressed cache — `Read` steps with an
    /// unchanged key are skipped and reused; a re-run is free resume.
    pub fn with_cache(registry: StepRegistry, cache: Arc<dyn ArtifactCache>) -> Self {
        Self { registry, cache }
    }

    /// Run `wf` over its source items (or once if it has no `[source]`), with up
    /// to `concurrency` items in flight.
    pub async fn run(&self, wf: &Workflow, concurrency: usize) -> Result<RunReport> {
        let steps: Vec<Arc<dyn Step>> = wf
            .steps
            .iter()
            .map(|s| self.registry.resolve(&s.uses))
            .collect::<Result<_>>()?;
        // A step caches iff it's Read-effect (safe to skip) and not opted out.
        let do_cache: Vec<bool> = wf
            .steps
            .iter()
            .zip(steps.iter())
            .map(|(spec, step)| step.descriptor().cacheable() && spec.cache != Some(false))
            .collect();
        let order = wf.topo_order()?;
        let items = match &wf.source {
            Some(src) => src.enumerate()?,
            None => vec![SourceItem {
                fields: BTreeMap::new(),
                fingerprint: String::new(),
            }],
        };
        tracing::info!(
            target: "workflow",
            workflow = %wf.name, items = items.len(), steps = wf.steps.len(),
            "workflow: run start"
        );

        let reports: Vec<ItemReport> = stream::iter(items.into_iter())
            .map(|item| run_item(wf, &steps, &order, &do_cache, &self.cache, item))
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
    do_cache: &[bool],
    cache: &Arc<dyn ArtifactCache>,
    item: SourceItem,
) -> ItemReport {
    let item_id = item
        .fields
        .get("name")
        .cloned()
        .unwrap_or_else(|| "·".into());
    let fingerprint = item.fingerprint;
    let mut scope = Scope {
        item: item.fields,
        completed: BTreeMap::new(),
    };
    let mut last_text = String::new();
    let mut ran = 0usize;
    let mut cached = 0usize;

    for &i in order {
        let spec = &wf.steps[i];
        let args = template::resolve_args(spec, &scope);
        let key = do_cache[i].then(|| cache::cache_key(&spec.uses, &spec.id, &args, &fingerprint));

        // Cache hit → skip the step, reuse its artifact.
        if let Some(k) = &key {
            if let Some(artifact) = cache.get(k) {
                last_text = output_text(&artifact.output);
                tracing::info!(
                    target: "workflow",
                    item = %item_id, step = %spec.id, uses = %spec.uses, cached = true,
                    "workflow: step cached (skipped)"
                );
                cached += 1;
                scope.completed.insert(spec.id.clone(), artifact);
                continue;
            }
        }

        let ctx = StepCtx {
            item_id: item_id.clone(),
        };
        let start = std::time::Instant::now();
        match steps[i].run(&args, &ctx).await {
            Ok(artifact) => {
                if let Some(k) = &key {
                    cache.put(k, &artifact);
                }
                last_text = output_text(&artifact.output);
                ran += 1;
                tracing::info!(
                    target: "workflow",
                    item = %item_id, step = %spec.id, uses = %spec.uses,
                    ms = start.elapsed().as_millis() as u64, cached = false,
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
                    ran,
                    cached,
                };
            }
        }
    }
    ItemReport {
        item: item_id,
        result: Ok(last_text),
        ran,
        cached,
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
