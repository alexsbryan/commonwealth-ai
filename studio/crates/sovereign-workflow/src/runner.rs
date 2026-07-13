// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single-process Runner: topo-order the steps, run each item's graph
//! threading `Artifact`s via `template::resolve_args`, with bounded per-item
//! concurrency, a content-addressed cache (skip a `Read` step on a key hit),
//! and one tracing event per step. Durable/distributed execution is P2 (the
//! pipeline tool as an outer loop).

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use sovereign_contracts::error::Result;
use sovereign_contracts::types::StepOutput;

use crate::cache::{ArtifactCache, NoCache};
use crate::kind::StepKind;
use crate::model::{Artifact, ResolvedArgs, Scope, SourceItem, Workflow};
use crate::progress::{emit, StepObserver, WorkflowProgress};
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
    params: Arc<BTreeMap<String, String>>,
    observer: Option<StepObserver>,
}

impl Runner {
    /// A runner with no cache (every step runs). The default.
    pub fn new(registry: StepRegistry) -> Self {
        Self {
            registry,
            cache: Arc::new(NoCache),
            params: Arc::new(BTreeMap::new()),
            observer: None,
        }
    }

    /// A runner backed by a content-addressed cache — `Read` steps with an
    /// unchanged key are skipped and reused; a re-run is free resume.
    pub fn with_cache(registry: StepRegistry, cache: Arc<dyn ArtifactCache>) -> Self {
        Self {
            registry,
            cache,
            params: Arc::new(BTreeMap::new()),
            observer: None,
        }
    }

    /// Run-global parameters, readable in any templated field as `{param.key}` and
    /// in the source's path/glob/items (`--param k=v`, `--folder`). Builder so the
    /// existing `run(&wf, n)` signature and its call sites stay unchanged.
    pub fn with_params(mut self, params: BTreeMap<String, String>) -> Self {
        self.params = Arc::new(params);
        self
    }

    /// Attach a [`StepObserver`] that receives a [`WorkflowProgress`] event at
    /// each lifecycle point (run start, every step's completion, item + run
    /// finish). The default is none — the headless path, where progress goes
    /// only to `tracing`. An interactive caller (the desktop run surface) uses
    /// this to stream "watch it go" updates to its UI.
    pub fn with_observer(mut self, observer: Option<StepObserver>) -> Self {
        self.observer = observer;
        self
    }

    /// Run `wf` over its source items (or once if it has no `[source]`), with up
    /// to `concurrency` items in flight.
    pub async fn run(&self, wf: &Workflow, concurrency: usize) -> Result<RunReport> {
        // Parse each `uses` to a typed StepKind once, then resolve it (ARCH §2.1).
        let steps: Vec<Arc<dyn Step>> = wf
            .steps
            .iter()
            .map(|s| StepKind::parse(&s.uses).and_then(|k| self.registry.resolve(&k)))
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
            Some(src) => src.enumerate(&self.params)?,
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
        emit(
            self.observer.as_ref(),
            WorkflowProgress::RunStarted {
                workflow: wf.name.clone(),
                items: items.len(),
                steps: wf.steps.len(),
            },
        );

        let observer = self.observer.as_ref();
        let reports: Vec<ItemReport> = stream::iter(items)
            .map(|item| {
                run_item(
                    wf,
                    &steps,
                    &order,
                    &do_cache,
                    &self.cache,
                    &self.params,
                    observer,
                    item,
                    concurrency,
                )
            })
            .buffer_unordered(concurrency.max(1))
            .collect()
            .await;

        let ok = reports.iter().filter(|i| i.result.is_ok()).count();
        emit(
            self.observer.as_ref(),
            WorkflowProgress::RunFinished {
                ok,
                failed: reports.len() - ok,
            },
        );

        Ok(RunReport {
            workflow: wf.name.clone(),
            items: reports,
        })
    }
}

/// Run one item's graph and fire `ItemDone` once it finishes (success or
/// abort). A thin wrapper over [`run_item_inner`] so the inner body keeps its
/// several early `return fail(…)` paths untouched — the terminal event fires in
/// exactly one place regardless of which path produced the report.
#[allow(clippy::too_many_arguments)]
async fn run_item(
    wf: &Workflow,
    steps: &[Arc<dyn Step>],
    order: &[usize],
    do_cache: &[bool],
    cache: &Arc<dyn ArtifactCache>,
    params: &Arc<BTreeMap<String, String>>,
    observer: Option<&StepObserver>,
    item: SourceItem,
    concurrency: usize,
) -> ItemReport {
    let report = run_item_inner(
        wf,
        steps,
        order,
        do_cache,
        cache,
        params,
        observer,
        item,
        concurrency,
    )
    .await;
    emit(
        observer,
        WorkflowProgress::ItemDone {
            item: report.item.clone(),
            ok: report.result.is_ok(),
            ran: report.ran,
            cached: report.cached,
        },
    );
    report
}

#[allow(clippy::too_many_arguments)]
async fn run_item_inner(
    wf: &Workflow,
    steps: &[Arc<dyn Step>],
    order: &[usize],
    do_cache: &[bool],
    cache: &Arc<dyn ArtifactCache>,
    params: &Arc<BTreeMap<String, String>>,
    observer: Option<&StepObserver>,
    item: SourceItem,
    concurrency: usize,
) -> ItemReport {
    let item_id = item
        .fields
        .get("name")
        .cloned()
        .unwrap_or_else(|| "·".into());
    let fingerprint = item.fingerprint;
    let mut scope = Scope {
        item: item.fields,
        completed: Arc::new(BTreeMap::new()),
        element: None,
        params: Arc::clone(params),
    };
    let mut last_text = String::new();
    let mut ran = 0usize;
    let mut cached = 0usize;
    let fail = |id: String, spec: &crate::model::StepSpec, msg: String, ran, cached| ItemReport {
        item: id,
        result: Err(format!("step `{}`: {msg}", spec.id)),
        ran,
        cached,
    };

    for (step_index, &i) in order.iter().enumerate() {
        let spec = &wf.steps[i];
        let step = &steps[i];
        let cacheable = do_cache[i];
        let ran_before = ran;
        let ctx = StepCtx {
            item_id: item_id.clone(),
        };

        let artifact = if let Some(fe) = &spec.for_each {
            // Fan-out: run this step once per element of `fe`'s collection.
            // Each element resolves + caches independently (its resolved args
            // include the element), so editing one element re-runs only it.
            let collection = match scope.completed.get(fe).map(|a| &a.output) {
                Some(StepOutput::Json(serde_json::Value::Array(arr))) => arr.clone(),
                _ => {
                    return fail(
                        item_id,
                        spec,
                        format!("`for_each = \"{fe}\"` is not a collection (a JSON array)"),
                        ran,
                        cached,
                    )
                }
            };
            // Run elements concurrently (bounded by `concurrency`), preserving
            // order. The daemon's embed slot continuous-batches the concurrent
            // requests GPU-side, so a `for_each embed` over many chunks is no
            // longer one HTTP round-trip at a time — and per-element caching
            // survives (each element keeps its own content-addressed key, so
            // editing one chunk still re-runs only it, batched with other misses).
            let elem_results: Vec<std::result::Result<(serde_json::Value, bool), String>> =
                stream::iter(collection)
                    .map(|elem| {
                        let mut elem_scope = scope.clone();
                        elem_scope.element = Some(elem);
                        let args = template::resolve_args(spec, &elem_scope);
                        // Per-element key folds in NO file fingerprint: the
                        // element's content is already in `args` (via
                        // `{element.…}`), so identical elements share a key and
                        // editing one element invalidates only it.
                        let key =
                            cacheable.then(|| cache::cache_key(&spec.uses, &spec.id, &args, ""));
                        let step = Arc::clone(step);
                        let cache = Arc::clone(cache);
                        let ctx = ctx.clone();
                        async move {
                            run_one(&step, &args, key.as_deref(), &cache, &ctx)
                                .await
                                .map(|(art, was_cached)| (output_value(&art.output), was_cached))
                                .map_err(|e| e.to_string())
                        }
                    })
                    .buffered(concurrency.max(1))
                    .collect()
                    .await;
            let on_error_skip = spec.on_error.as_deref() == Some("skip");
            let mut results = Vec::with_capacity(elem_results.len());
            let mut failures: Vec<serde_json::Value> = Vec::new();
            for (idx, r) in elem_results.into_iter().enumerate() {
                match r {
                    Ok((v, was_cached)) => {
                        if was_cached {
                            cached += 1;
                        } else {
                            ran += 1;
                        }
                        results.push(v);
                    }
                    // A tolerant `for_each` (`on_error = "skip"`) records the
                    // failing element and continues — the real Phase-1
                    // skip-and-continue, so one bad chapter doesn't sink a whole
                    // book. The default aborts the item on the first error.
                    Err(e) if on_error_skip => {
                        tracing::warn!(
                            target: "workflow",
                            item = %item_id, step = %spec.id, element = idx, error = %e,
                            "workflow: for_each element failed — skipped (on_error=skip)"
                        );
                        emit(
                            observer,
                            WorkflowProgress::ElementSkipped {
                                item: item_id.clone(),
                                step: spec.id.clone(),
                                index: idx,
                                error: e.clone(),
                            },
                        );
                        failures.push(serde_json::json!({ "index": idx, "error": e }));
                    }
                    Err(e) => return fail(item_id, spec, e, ran, cached),
                }
            }
            Artifact::new(
                "collection",
                StepOutput::Json(serde_json::Value::Array(results)),
            )
            .with_failures(failures)
        } else {
            let args = template::resolve_args(spec, &scope);
            let key =
                cacheable.then(|| cache::cache_key(&spec.uses, &spec.id, &args, &fingerprint));
            match run_one(step, &args, key.as_deref(), cache, &ctx).await {
                Ok((art, was_cached)) => {
                    if was_cached {
                        cached += 1;
                    } else {
                        ran += 1;
                    }
                    art
                }
                Err(e) => return fail(item_id, spec, format!("{e}"), ran, cached),
            }
        };

        tracing::info!(
            target: "workflow",
            item = %item_id, step = %spec.id, uses = %spec.uses,
            for_each = spec.for_each.is_some(),
            "workflow: step done"
        );
        emit(
            observer,
            WorkflowProgress::StepDone {
                item: item_id.clone(),
                step: spec.id.clone(),
                uses: spec.uses.clone(),
                for_each: spec.for_each.is_some(),
                // No new `ran` work this step → every unit was a cache hit.
                cached: ran == ran_before,
                step_index,
                total_steps: order.len(),
            },
        );
        last_text = output_text(&artifact.output);
        // make_mut is a no-op clone here: the element scopes that shared this Arc
        // during a for_each were dropped before we reach the next insert.
        Arc::make_mut(&mut scope.completed).insert(spec.id.clone(), artifact);
    }

    ItemReport {
        item: item_id,
        result: Ok(last_text),
        ran,
        cached,
    }
}

/// Run one step (one element, for a `for_each` body) with the shared
/// cache-check-then-run logic. Returns the artifact + whether it was a cache hit.
async fn run_one(
    step: &Arc<dyn Step>,
    args: &ResolvedArgs,
    key: Option<&str>,
    cache: &Arc<dyn ArtifactCache>,
    ctx: &StepCtx,
) -> Result<(Artifact, bool)> {
    if let Some(k) = key {
        if let Some(art) = cache.get(k) {
            return Ok((art, true));
        }
    }
    let mut art = step.run(args, ctx).await?;
    // Stamp: merge the step's resolved `stamp` object into its output object —
    // the runner-level "stamp identity onto the result" (e.g. `chapter_id` from
    // `{element.index}`). Applied here so it covers both plain and per-`for_each`
    // runs, and is cached with the stamped output (the key already folds `stamp`
    // in via the resolved args, so a changed stamp invalidates cleanly).
    if let Some(stamp) = &args.stamp {
        art.output = apply_stamp(art.output, stamp);
    }
    if let Some(k) = key {
        cache.put(k, &art);
    }
    Ok((art, false))
}

/// Merge a resolved `stamp` object into a step's output object. Only when both
/// are JSON objects — a non-object output (text, a collection array) has nowhere
/// to receive the fields, so the stamp is left inert and logged (glassbox). The
/// stamp's keys override (the author-declared identity is authoritative, like the
/// real Phase-1 runner stamping `chapter_id` over whatever the model emitted).
fn apply_stamp(output: StepOutput, stamp: &serde_json::Value) -> StepOutput {
    match (output, stamp) {
        (
            StepOutput::Json(serde_json::Value::Object(mut obj)),
            serde_json::Value::Object(fields),
        ) => {
            for (k, v) in fields {
                obj.insert(k.clone(), v.clone());
            }
            StepOutput::Json(serde_json::Value::Object(obj))
        }
        (other, _) => {
            tracing::warn!(
                target: "workflow",
                "stamp: step output is not a JSON object — stamp left inert"
            );
            other
        }
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

/// Extract a step output as a JSON value, for collecting `for_each` results.
fn output_value(o: &StepOutput) -> serde_json::Value {
    match o {
        StepOutput::Text(s) => serde_json::Value::String(s.clone()),
        StepOutput::Json(v) => v.clone(),
        StepOutput::ReasonWithToolsResult { text, .. } => serde_json::Value::String(text.clone()),
        StepOutput::Jump(_) | StepOutput::Skipped => serde_json::Value::Null,
    }
}
