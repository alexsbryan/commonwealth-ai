// SPDX-License-Identifier: AGPL-3.0-or-later
//! Enrichment passes — the plugin seam between the recipe pipeline and the
//! enrichment subsystem.
//!
//! A recipe's `[enrichment] type` names ONE pass. Before this module, five
//! sites in three crates switched on that string and gave four different
//! answers for a value none of them recognised: the ingest dispatch ran
//! `field_model`, the health-check stamp said "expected", the drift probe
//! said "unverifiable", and the desktop's "enrich now" ran `tiered`. That is
//! ARCH §10.6's duplicated decider exactly, and §4.3's silently-shaved
//! behaviour. Now there is one table — [`EnrichmentPassRegistry::builtin`] —
//! and every question the pipeline asks about a type is a method on the pass
//! it resolves to. An unrecognised type is refused by name at recipe load
//! (`recipe_parsing::check_enrichment_type`), never defaulted (§18.3).
//!
//! The set is OPEN by intent — a third party should be able to register a
//! pass — which is why this is a registry and not an enum (§2.1 vs §4). The
//! shape is copied field-for-field from [`super::domain_registry`]; do not
//! invent a third.
//!
//! Questions the trait answers, and who asks:
//!
//! | method | asked by |
//! |---|---|
//! | [`EnrichmentPass::runs_at_install`] | `engine/ingest.rs` — run it now, or stamp "deferred" and move on |
//! | [`EnrichmentPass::deferred_hint`]   | the same site — what to tell the user instead |
//! | [`EnrichmentPass::declared_artifact`] | `CorpusEngine::enrichment_drift` — is the promised artifact on disk |
//! | [`EnrichmentPass::resumable_at_boot`] | `conversation_enrichment_is_resumable` — re-kick after a crash |
//! | [`EnrichmentPass::produces_atoms`]   | `Recipe::produces_enriched_atoms` — the dashboard readiness lint |
//! | [`EnrichmentPass::run`]              | `engine/ingest.rs`, for passes that run at install |

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::clustering::EnrichmentProgress;
use super::tiered::{ChunkEntityExtractorHandle, TieredProviderHandle};
use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::progress::{IngestProgress, ProgressCallback};
use crate::recipe::Recipe;
use crate::types::{EmbedFn, InferenceFn};

/// The four built-in pass ids. These are the ONLY place the literals live;
/// a site that needs to name a pass compares against these, never a string.
pub const FIELD_MODEL: &str = "field_model";
pub const TIERED: &str = "tiered";
pub const ATLAS: &str = "atlas";
pub const INVESTIGATION: &str = "investigation";

/// Everything an install-time pass may need, handed in by the ingest so a
/// pass never reaches back into `CorpusEngine`.
pub struct EnrichmentContext<'a> {
    pub recipe: &'a Recipe,
    pub index_path: &'a Path,
    pub index: &'a CorpusIndex,
    pub embed: EmbedFn,
    pub inference: InferenceFn,
    pub tiered_provider: Option<&'a TieredProviderHandle>,
    pub entity_extractor: Option<&'a ChunkEntityExtractorHandle>,
    pub progress: Option<&'a ProgressCallback>,
}

/// One enrichment type, as the pipeline sees it. Seven methods, four of them
/// defaulted — deliberately under ARCH §5.1's ~8 line.
#[async_trait]
pub trait EnrichmentPass: Send + Sync {
    /// The `[enrichment] type` value this pass answers to.
    fn id(&self) -> &'static str;
    /// Does install-time ingest run this, or does it need an explicit verb?
    fn runs_at_install(&self) -> bool;
    /// What an explicit run looks like, when `runs_at_install()` is false.
    fn deferred_hint(&self) -> Option<&'static str> {
        None
    }
    /// The artifact a BUILT enrichment of this type writes, relative to the
    /// index dir. Drives `enrichment_drift`; `None` means "no single
    /// verifiable artifact" and drift stays silent rather than asserting
    /// what it cannot check.
    fn declared_artifact(&self) -> Option<&'static str> {
        None
    }
    /// May a boot-time resume re-enter this pass mid-flight?
    fn resumable_at_boot(&self) -> bool {
        false
    }
    /// Does a completed build of this pass write graph atoms?
    fn produces_atoms(&self) -> bool {
        false
    }
    /// Run the pass at install. Only reached when `runs_at_install()`.
    async fn run(&self, ctx: &EnrichmentContext<'_>) -> Result<()>;
}

/// Registry mapping `[enrichment] type` ids to passes. Same shape as
/// [`super::domain_registry::DomainRegistry`].
pub struct EnrichmentPassRegistry {
    passes: HashMap<String, Arc<dyn EnrichmentPass>>,
}

impl EnrichmentPassRegistry {
    /// A registry pre-loaded with the four built-in passes.
    pub fn builtin() -> Self {
        let mut registry = Self {
            passes: HashMap::new(),
        };
        registry.register(Arc::new(FieldModelPass));
        registry.register(Arc::new(TieredPass));
        registry.register(Arc::new(AtlasPass));
        registry.register(Arc::new(InvestigationPass));
        registry
    }

    /// Register a pass under its own `id()`.
    pub fn register(&mut self, pass: Arc<dyn EnrichmentPass>) {
        self.passes.insert(pass.id().to_string(), pass);
    }

    /// Look up a pass by id. `None` if unregistered.
    pub fn get(&self, id: &str) -> Option<Arc<dyn EnrichmentPass>> {
        self.passes.get(id).cloned()
    }

    /// Look up a pass by id, or refuse by name with the valid set listed.
    pub fn resolve(&self, id: &str) -> Result<Arc<dyn EnrichmentPass>> {
        self.get(id).ok_or_else(|| Error::UnknownEnrichmentType {
            got: id.to_string(),
            valid: self.ids().join(", "),
        })
    }

    /// All registered ids, sorted so error messages are stable.
    pub fn ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.passes.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
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

/// The legacy field-model pipeline (`FieldModelEngine`): skeleton extraction,
/// clustering, cluster labelling. Runs at install; writes
/// `field_skeleton.json`.
pub struct FieldModelPass;

#[async_trait]
impl EnrichmentPass for FieldModelPass {
    fn id(&self) -> &'static str {
        FIELD_MODEL
    }
    fn runs_at_install(&self) -> bool {
        true
    }
    fn declared_artifact(&self) -> Option<&'static str> {
        Some("field_skeleton.json")
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
            Arc::new(move |prompt: &str, schema: Option<&serde_json::Value>| {
                calls.fetch_add(1, Ordering::Relaxed);
                let failures = failures.clone();
                let call = inner(prompt, schema);
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
    fn declared_artifact(&self) -> Option<&'static str> {
        Some("atlas/atoms.json")
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

        // (id, runs_at_install, declared_artifact, resumable_at_boot, produces_atoms, has_hint)
        let expected = [
            (
                FIELD_MODEL,
                true,
                Some("field_skeleton.json"),
                false,
                false,
                false,
            ),
            (TIERED, true, None, true, false, false),
            (ATLAS, false, Some("atlas/atoms.json"), false, true, true),
            (INVESTIGATION, false, None, false, true, true),
        ];
        for (id, install, artifact, resumable, atoms, hint) in expected {
            let p = reg.get(id).unwrap_or_else(|| panic!("missing pass: {id}"));
            assert_eq!(p.id(), id);
            assert_eq!(p.runs_at_install(), install, "{id}: runs_at_install");
            assert_eq!(p.declared_artifact(), artifact, "{id}: declared_artifact");
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
