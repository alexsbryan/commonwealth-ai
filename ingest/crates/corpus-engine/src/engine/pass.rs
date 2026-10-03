// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrichment-pass port — the plugin seam between the recipe pipeline and
//! the enrichment subsystem, owned by the engine.
//!
//! A recipe's `[enrichment] type` names ONE pass. Before this module, five
//! sites in three crates switched on that string and gave four different
//! answers for a value none of them recognised: the ingest dispatch ran
//! `field_model`, the health-check stamp said "expected", the drift probe
//! said "unverifiable", and the desktop's "enrich now" ran `tiered`. That is
//! ARCH §10.6's duplicated decider exactly, and §4.3's silently-shaved
//! behaviour. Now there is one table and every question the pipeline asks
//! about a type is a method on the pass it resolves to. An unrecognised type
//! is refused by name at recipe load (`recipe_parsing::check_enrichment_type`),
//! never defaulted (§18.3).
//!
//! The set is OPEN by intent — a third party should be able to register a
//! pass — which is why this is a registry and not an enum (§2.1 vs §4). The
//! shape is copied field-for-field from
//! [`DomainRegistry`](crate::enrichment::domain_registry::DomainRegistry); do
//! not invent a third.
//!
//! The engine is the caller — `engine/ingest.rs` runs a pass at install and
//! `CorpusEngine`'s drift and resume checks ask the same registry — so the
//! trait, its context and the registry live here (DE "Direction: Understanding
//! reads Ingest and Retrieval, never the reverse", "The pass port is the
//! engine's"). The built-in passes implement the port in `understanding-host`,
//! and the assembly of the built-in registry is a host concern that moves
//! there; until then the four impls and `builtin()` live in
//! [`crate::enrichment::pass`].
//!
//! Questions the trait answers, and who asks:
//!
//! | method | asked by |
//! |---|---|
//! | [`EnrichmentPass::runs_at_install`] | `engine/ingest.rs` — run it now, or stamp "deferred" and move on |
//! | [`EnrichmentPass::deferred_hint`]   | the same site — what to tell the user instead |
//! | [`EnrichmentPass::declared_artifacts`] | `CorpusEngine::enrichment_drift` — is a promised artifact on disk |
//! | [`EnrichmentPass::resumable_at_boot`] | `conversation_enrichment_is_resumable` — re-kick after a crash |
//! | [`EnrichmentPass::produces_atoms`]   | `Recipe::produces_enriched_atoms` — the dashboard readiness lint |
//! | [`EnrichmentPass::run`]              | `engine/ingest.rs`, for passes that run at install |

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::enrichment::tiered::{ChunkEntityExtractorHandle, TieredProviderHandle};
use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::progress::ProgressCallback;
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
    /// The artifacts a BUILT enrichment of this type may write, relative to
    /// the index dir. Drives `enrichment_drift`, which reports drift only when
    /// NONE of them is on disk; empty means "no verifiable artifact" and drift
    /// stays silent rather than asserting what it cannot check.
    ///
    /// A LIST rather than one path because `field_model` writes a different
    /// artifact per domain since ei-7b (2026-09-05) — `atlas/atoms.json` for
    /// an `AtlasAtoms` domain, `field_skeleton.json` for the three
    /// KnowledgeView ones. Naming only one of the two would have reported
    /// every corpus on the other arm as drifted: a false alarm indistinguishable
    /// from a real one.
    fn declared_artifacts(&self) -> &'static [&'static str] {
        &[]
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
/// [`DomainRegistry`](crate::enrichment::domain_registry::DomainRegistry).
pub struct EnrichmentPassRegistry {
    passes: HashMap<String, Arc<dyn EnrichmentPass>>,
}

impl EnrichmentPassRegistry {
    /// An empty registry. The built-in passes are registered by the assembler
    /// that knows them (`builtin()` today, `understanding-host` when the
    /// passes move); this constructor names none of them.
    pub fn new() -> Self {
        Self {
            passes: HashMap::new(),
        }
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

impl Default for EnrichmentPassRegistry {
    fn default() -> Self {
        Self::new()
    }
}
