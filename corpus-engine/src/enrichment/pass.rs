// SPDX-License-Identifier: AGPL-3.0-or-later
//! The four built-in enrichment passes — implementations of the engine-owned
//! port [`crate::engine::pass`].
//!
//! `FieldModelPass`, `TieredPass`, `AtlasPass` and `InvestigationPass`
//! implement [`EnrichmentPass`] and are assembled by
//! [`EnrichmentPassRegistry::builtin`]. They are Understanding's: this is the
//! `enrichment/` half that becomes `understanding-host`, so the passes move
//! there and `builtin()` becomes a host free function (an inherent impl cannot
//! cross a crate line). Until then the historical
//! `crate::enrichment::pass::<item>` paths resolve through the re-exports
//! below.
//!
//! The port's questions and their askers are documented in
//! [`crate::engine::pass`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::clustering::EnrichmentProgress;
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::error::{Error, Result};
use crate::progress::{IngestProgress, ProgressCallback};
use crate::types::InferenceFn;

// The port — the trait, its context and the registry — is the engine's and
// lives in `crate::engine::pass`. These re-exports keep every historical
// `crate::enrichment::pass::<item>` path resolving while the passes below
// wait for `understanding-host`.
pub use crate::engine::pass::{
    EnrichmentContext, EnrichmentPass, EnrichmentPassRegistry, ATLAS, FIELD_MODEL, INVESTIGATION,
    TIERED,
};

impl EnrichmentPassRegistry {
    /// A registry pre-loaded with the four built-in passes. Assembly of the
    /// built-ins belongs to whoever owns the impls — this module today,
    /// `understanding-host` once the passes move there.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(FieldModelPass));
        registry.register(Arc::new(TieredPass));
        registry.register(Arc::new(AtlasPass));
        registry.register(Arc::new(InvestigationPass));
        registry
    }
}

/// A pass that never runs at install, asked to run at install anyway.
fn refuse_deferred(pass: &dyn EnrichmentPass) -> Error {
    Error::InvalidInput(format!(
        "enrichment pass `{}` does not run at install — {}",
        pass.id(),
        pass.deferred_hint().unwrap_or("it needs an explicit build"),
    ))
}

// ── field_model ───────────────────────────────────────────────────────────

/// The field-model pipeline (`FieldModelEngine`): skeleton extraction,
/// clustering, cluster labelling. Runs at install; publishes its canonical
/// questions and their positions into the corpus atlas as `Question` and
/// `Position` atoms (ei-7b).
///
/// Before 2026-09-05 it declared `field_skeleton.json` — a parallel artifact
/// beside the index with exactly one reader. It declares `atlas/atoms.json`
/// now, the same artifact the atlas pass declares, because the two write the
/// same file and a pass that appends atoms is a pass that produces atoms.
pub struct FieldModelPass;

#[async_trait]
impl EnrichmentPass for FieldModelPass {
    fn id(&self) -> &'static str {
        FIELD_MODEL
    }
    fn runs_at_install(&self) -> bool {
        true
    }
    fn declared_artifacts(&self) -> &'static [&'static str] {
        // Two, because the domain decides which: `AtlasAtoms` publishes the
        // first, `JsonAndLance` the second. See `SkeletonStorage`.
        &["atlas/atoms.json", "field_skeleton.json"]
    }
    fn produces_atoms(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &EnrichmentContext<'_>) -> Result<()> {
        // Count the pipeline's inference calls and how many of them failed.
        //
        // The pipeline absorbs per-call errors by design — a few unparseable
        // cluster labels should not kill an ingest. The failure mode that
        // creates is a TOTAL outage: every call errors, `enrich` returns `Ok`
        // with zero field-model tables, and the ingest reports "Ingestion
        // complete". That is success-shaped for something nobody asked for
        // (§18.3).
        //
        // It does NOT become an `Err`: the chunks are real and the ingest
        // genuinely succeeded — saying otherwise would be its own lie, and
        // would throw away work the user can use. What it must not do is
        // stay SILENT. These two counters are the evidence the completion
        // WARN below is built from.
        let inference_calls = Arc::new(AtomicU64::new(0));
        let inference_failures = Arc::new(AtomicU64::new(0));
        let counted_inference: InferenceFn = {
            let inner = ctx.inference.clone();
            let calls = inference_calls.clone();
            let failures = inference_failures.clone();
            Arc::new(move |prompt: &ChatPrompt, max_tokens: Option<u32>| {
                calls.fetch_add(1, Ordering::Relaxed);
                let failures = failures.clone();
                let call = inner(prompt, max_tokens);
                Box::pin(async move {
                    let outcome = call.await;
                    if outcome.is_err() {
                        failures.fetch_add(1, Ordering::Relaxed);
                    }
                    outcome
                })
            })
        };
        let field_engine = super::field_engine::FieldModelEngine::from_recipe(
            ctx.recipe,
            ctx.embed.clone(),
            counted_inference,
        )?;
        let corpus_id = ctx.recipe.corpus.id.clone();
        let progress_fn = bridge_field_model_progress(corpus_id.clone(), ctx.progress);
        let enrich_outcome = field_engine.enrich(ctx.index, &progress_fn).await;

        // Report at completion, on both the Ok and Err paths, before the
        // outcome propagates.
        let calls = inference_calls.load(Ordering::Relaxed);
        let failed = inference_failures.load(Ordering::Relaxed);
        tracing::debug!(
            corpus = %corpus_id,
            inference_calls = calls,
            inference_failures = failed,
            "enrichment: inference tally"
        );
        if calls > 0 && failed == calls {
            // Name the substitution out loud (§18.3). The corpus is installed
            // and searchable; what it is NOT is enriched, and every other
            // line this ingest emits says "complete". `EnrichmentChecker` is
            // the standing surface for the same fact — this WARN is what
            // puts it in the log at the moment it happens.
            tracing::warn!(
                corpus = %corpus_id,
                inference_calls = calls,
                inference_failures = failed,
                "enrichment requested and produced nothing: \
                 {failed}/{calls} inference calls failed"
            );
        }
        enrich_outcome.map(|_| ())
    }
}

/// Bridge field-model phase events to the outer `IngestProgress` channel so
/// HTTP consumers (desktop UI, CLI poll) see real-time phase transitions
/// during Phase 1 / 1b / 2 / clustering / 3 instead of staring at the last
/// `Embedding` event. Without this bridge a long enrichment phase looked like
/// a hang (observed 2026-05-20: conversations-anthropic ingest stuck at
/// "Embedding chunks…" while HDBSCAN clustered 16326×1024 silently).
///
/// The stderr render is unchanged from the pre-bridge shape so log consumers
/// see the same lines. Mapping rules for the channel: `Phase` variants emit
/// `Enriching` with a stable machine-token phase name the desktop maps to
/// display labels; numeric progress sets `fraction` so bars can move.
fn bridge_field_model_progress<'a>(
    id: String,
    outer: Option<&'a ProgressCallback>,
) -> impl Fn(EnrichmentProgress) + Send + Sync + 'a {
    move |p: EnrichmentProgress| {
        use EnrichmentProgress as EP;
        match &p {
            EP::Phase { phase, name, note } => {
                if note.is_empty() {
                    eprintln!("[{id}] Phase {phase}: {name}");
                } else {
                    eprintln!("[{id}] Phase {phase}: {name} ({note})");
                }
            }
            EP::PhaseSkipped { phase, name } => {
                eprintln!("[{id}] Phase {phase}: {name} — skipped (checkpoint)")
            }
            EP::Resuming { from_phase } => {
                eprintln!("[{id}] Resuming enrichment from {from_phase}")
            }
            EP::ClusteringStarted { total_chunks } => {
                eprintln!("[{id}] Clustering {total_chunks} chunks...")
            }
            EP::ClusteringStep { step, detail } => eprintln!("[{id}] ↳ {step}: {detail}"),
            EP::ClusteringComplete {
                cluster_count,
                noise_chunks,
            } => eprintln!(
                "[{id}] Clustering complete: {cluster_count} clusters, {noise_chunks} noise"
            ),
            EP::Phase1Progress {
                batches_done,
                batches_total,
            } => eprintln!("[{id}] Skeleton extraction: {batches_done}/{batches_total} batches"),
            EP::Phase2bProgress {
                clusters_done,
                clusters_total,
                clusters_failed,
                consecutive_failures,
                last_error,
            } => {
                if *consecutive_failures >= 4 {
                    eprintln!(
                        "[{id}] Cluster labeling: {clusters_done}/{clusters_total} — \
                         {consecutive_failures} consecutive failures (last: {})",
                        last_error.as_deref().unwrap_or("?"),
                    );
                } else if *clusters_done == *clusters_total || clusters_done % 16 == 0 {
                    eprintln!(
                        "[{id}] Cluster labeling: {clusters_done}/{clusters_total} \
                         ({clusters_failed} failed)"
                    );
                }
            }
            EP::Phase2bComplete { labeled_count } => {
                eprintln!("[{id}] Cluster labeling complete: {labeled_count} clusters labeled")
            }
        }

        let Some(cb) = outer else { return };
        let evt = match &p {
            EP::Phase { phase, name, note } => {
                let detail = if note.is_empty() {
                    format!("Phase {phase}: {name}")
                } else {
                    format!("Phase {phase}: {name} ({note})")
                };
                Some(IngestProgress::Enriching {
                    phase: format!("phase-{phase}"),
                    detail,
                    fraction: None,
                })
            }
            EP::PhaseSkipped { phase, name } => Some(IngestProgress::Enriching {
                phase: format!("phase-{phase}-skipped"),
                detail: format!("Phase {phase}: {name} — skipped (checkpoint)"),
                fraction: None,
            }),
            EP::Resuming { from_phase } => Some(IngestProgress::Enriching {
                phase: "resuming".into(),
                detail: format!("Resuming enrichment from {from_phase}"),
                fraction: None,
            }),
            EP::ClusteringStarted { total_chunks } => Some(IngestProgress::Enriching {
                phase: "clustering".into(),
                detail: format!("Clustering {total_chunks} chunks…"),
                fraction: None,
            }),
            EP::ClusteringStep { step, detail } => Some(IngestProgress::Enriching {
                phase: "clustering".into(),
                detail: format!("{step}: {detail}"),
                fraction: None,
            }),
            EP::ClusteringComplete {
                cluster_count,
                noise_chunks,
            } => Some(IngestProgress::Enriching {
                phase: "clustering-complete".into(),
                detail: format!(
                    "Clustering complete: {cluster_count} clusters, {noise_chunks} noise"
                ),
                fraction: Some(1.0),
            }),
            EP::Phase1Progress {
                batches_done,
                batches_total,
            } => {
                let frac = if *batches_total > 0 {
                    Some(*batches_done as f32 / *batches_total as f32)
                } else {
                    None
                };
                Some(IngestProgress::Enriching {
                    phase: "skeleton-extraction".into(),
                    detail: format!("Skeleton extraction: {batches_done}/{batches_total} batches"),
                    fraction: frac,
                })
            }
            EP::Phase2bProgress {
                clusters_done,
                clusters_total,
                clusters_failed,
                consecutive_failures,
                last_error,
            } => {
                let frac = if *clusters_total > 0 {
                    Some(*clusters_done as f32 / *clusters_total as f32)
                } else {
                    None
                };
                let detail = if *consecutive_failures >= 4 {
                    format!(
                        "Cluster labeling: {clusters_done}/{clusters_total} \
                         ({clusters_failed} failed, {consecutive_failures} consecutive — last: {})",
                        last_error.as_deref().unwrap_or("?"),
                    )
                } else {
                    format!(
                        "Cluster labeling: {clusters_done}/{clusters_total} \
                         ({clusters_failed} failed)"
                    )
                };
                Some(IngestProgress::Enriching {
                    phase: "cluster-labeling".into(),
                    detail,
                    fraction: frac,
                })
            }
            EP::Phase2bComplete { labeled_count } => Some(IngestProgress::Enriching {
                phase: "cluster-labeling-complete".into(),
                detail: format!("Cluster labeling complete: {labeled_count} clusters labeled"),
                fraction: Some(1.0),
            }),
        };
        if let Some(evt) = evt {
            cb(evt);
        }
    }
}

// ── tiered ────────────────────────────────────────────────────────────────

/// The tiered RAPTOR + entity build (spec
/// `sovereign/docs/specs/CONV_TIERED_PORT.md`). Runs at install through the
/// injected [`TieredProviderHandle`]; resumable after a process restart.
///
/// No declared artifact: its real outputs are SQLite tables written by the
/// injected provider, not a file, so `enrichment_drift` cannot verify it.
/// The registry makes that visible rather than hiding it in a `_ => None`
/// arm; giving it one is a separate call.
pub struct TieredPass;

#[async_trait]
impl EnrichmentPass for TieredPass {
    fn id(&self) -> &'static str {
        TIERED
    }
    fn runs_at_install(&self) -> bool {
        true
    }
    fn resumable_at_boot(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &EnrichmentContext<'_>) -> Result<()> {
        // Two tiered variants: the conv-grouping one (`run_tiered_enrichment`)
        // buckets chunks by `conv_uuid` (per the conv corpora schema), and
        // the folder-grouping one (`run_folder_tiered_enrichment`) buckets by
        // `source_doc_id` (one bag per file, what watched-folder and vault
        // corpora produce). Pick by the recipe's display.category — vault +
        // watched folders take the folder variant.
        let display_category = ctx
            .recipe
            .display
            .as_ref()
            .and_then(|d| d.category.as_deref())
            .unwrap_or("");
        let is_folder_shape = matches!(display_category, "vault" | "watched_folder");
        if is_folder_shape {
            super::tiered::run_folder_tiered_enrichment(
                &ctx.recipe.corpus.id,
                ctx.index_path,
                ctx.tiered_provider,
                ctx.entity_extractor,
            )
            .await?;
        } else {
            super::tiered::run_tiered_enrichment(
                ctx.recipe,
                ctx.index_path,
                ctx.tiered_provider,
                ctx.entity_extractor,
            )
            .await?;
        }
        Ok(())
    }
}

// ── atlas ─────────────────────────────────────────────────────────────────

/// The atlas build — a separate, explicit step (`sovereign enrich init <id>
/// --from-corpus <id> --pipeline <…_atlas>` then `enrich build <id>`), run
/// from the registry of `*_atlas` pipelines, NOT the field-model domain
/// registry. Skipped at install for two reasons: running the field-model
/// enricher would DUPLICATE work the atlas build redoes, and an atlas
/// recipe's `enrichment.domain` selects an atlas pipeline
/// (literary/philosophy), which is not a registered field-model domain, so
/// `from_recipe` would trip `UnknownEnrichmentDomain`. The desktop
/// "Build & enrich" flow bridges install → atlas via
/// `recipe_enrich_init_from_corpus`.
pub struct AtlasPass;

#[async_trait]
impl EnrichmentPass for AtlasPass {
    fn id(&self) -> &'static str {
        ATLAS
    }
    fn runs_at_install(&self) -> bool {
        false
    }
    fn deferred_hint(&self) -> Option<&'static str> {
        Some(
            "run `sovereign enrich init <id> --from-corpus <id> --pipeline <…_atlas>` \
             then `enrich build <id>` to enrich",
        )
    }
    fn declared_artifacts(&self) -> &'static [&'static str] {
        &["atlas/atoms.json"]
    }
    fn produces_atoms(&self) -> bool {
        true
    }

    async fn run(&self, _ctx: &EnrichmentContext<'_>) -> Result<()> {
        Err(refuse_deferred(self))
    }
}

// ── investigation ─────────────────────────────────────────────────────────

/// The typed entity/relationship pipeline — an explicit, opt-in step
/// (`sovereign enrich investigation build <id>`), NOT the field-model domain
/// registry. Skipped at install so an investigation-type recipe installs and
/// finalizes cleanly instead of tripping `UnknownEnrichmentDomain` when its
/// `enrichment.domain` isn't a registered field-model domain.
pub struct InvestigationPass;

#[async_trait]
impl EnrichmentPass for InvestigationPass {
    fn id(&self) -> &'static str {
        INVESTIGATION
    }
    fn runs_at_install(&self) -> bool {
        false
    }
    fn deferred_hint(&self) -> Option<&'static str> {
        Some("run `sovereign enrich investigation build <id>` to enrich")
    }
    fn produces_atoms(&self) -> bool {
        true
    }

    async fn run(&self, _ctx: &EnrichmentContext<'_>) -> Result<()> {
        Err(refuse_deferred(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one table, pinned: every derived view for every built-in id, so a
    /// change to any answer is a visible diff here rather than a surprise
    /// at one of the five former call sites.
    #[test]
    fn builtin_passes_answer_the_pipelines_questions() {
        let reg = EnrichmentPassRegistry::builtin();
        assert_eq!(reg.ids(), vec![ATLAS, FIELD_MODEL, INVESTIGATION, TIERED]);

        // (id, runs_at_install, declared_artifacts, resumable_at_boot, produces_atoms, has_hint)
        let expected = [
            (
                FIELD_MODEL,
                true,
                &["atlas/atoms.json", "field_skeleton.json"][..],
                false,
                true,
                false,
            ),
            (TIERED, true, &[][..], true, false, false),
            (ATLAS, false, &["atlas/atoms.json"][..], false, true, true),
            (INVESTIGATION, false, &[][..], false, true, true),
        ];
        for (id, install, artifact, resumable, atoms, hint) in expected {
            let p = reg.get(id).unwrap_or_else(|| panic!("missing pass: {id}"));
            assert_eq!(p.id(), id);
            assert_eq!(p.runs_at_install(), install, "{id}: runs_at_install");
            assert_eq!(p.declared_artifacts(), artifact, "{id}: declared_artifacts");
            assert_eq!(p.resumable_at_boot(), resumable, "{id}: resumable_at_boot");
            assert_eq!(p.produces_atoms(), atoms, "{id}: produces_atoms");
            assert_eq!(p.deferred_hint().is_some(), hint, "{id}: deferred_hint");
            // A deferred pass always says how to run it; an install pass never
            // needs to.
            assert_eq!(!p.runs_at_install(), p.deferred_hint().is_some(), "{id}");
        }
    }

    /// §4.3 — an unknown id is refused by name, with the valid set listed.
    #[test]
    fn unknown_type_is_refused_by_name_with_the_valid_set() {
        let reg = EnrichmentPassRegistry::builtin();
        assert!(reg.get("foo").is_none());
        let err = match reg.resolve("foo") {
            Err(e) => e.to_string(),
            Ok(p) => panic!("`foo` resolved to `{}`", p.id()),
        };
        assert!(err.contains("\"foo\""), "{err}");
        for id in [ATLAS, FIELD_MODEL, INVESTIGATION, TIERED] {
            assert!(err.contains(id), "{err} lacks {id}");
        }
        // Exact match only: the registry never folds case, so a recipe that
        // says `Atlas` is refused at load rather than routed by one site and
        // not another.
        assert!(reg.get("Atlas").is_none());
    }

    /// A third party can register a pass, and the registry answers for it
    /// like any built-in — the point of a registry over an enum (§4).
    #[test]
    fn a_registered_pass_is_first_class() {
        struct Custom;
        #[async_trait]
        impl EnrichmentPass for Custom {
            fn id(&self) -> &'static str {
                "custom"
            }
            fn runs_at_install(&self) -> bool {
                false
            }
            fn deferred_hint(&self) -> Option<&'static str> {
                Some("run custom-build")
            }
            async fn run(&self, _ctx: &EnrichmentContext<'_>) -> Result<()> {
                Ok(())
            }
        }
        let mut reg = EnrichmentPassRegistry::builtin();
        reg.register(Arc::new(Custom));
        assert_eq!(reg.ids().len(), 5);
        assert_eq!(
            reg.resolve("custom").unwrap().deferred_hint(),
            Some("run custom-build")
        );
    }
}
