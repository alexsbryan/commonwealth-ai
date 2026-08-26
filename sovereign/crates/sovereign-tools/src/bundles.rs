// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tool families a host composes into its turn registry.
//!
//! Each type here is a [`ToolBundle`](sovereign_contracts::tool_bundle::ToolBundle):
//! it holds the collaborators its tools need, and it knows nothing about the
//! host, the recipe, or the other bundles. A host says what it carries by
//! building a `Vec<Box<dyn ToolBundle>>`; the shared recipe folds that vec
//! into a registry and never names a tool itself.
//!
//! # Why families, and why these families
//!
//! `quality/TOPOLOGY.md` §10 phase 7b. The grouping is not by subject matter
//! — it is by **the reason a host would or would not carry it**, which is the
//! only grouping that makes the composed list readable:
//!
//! - [`CoreTurnTools`] — corpus-grounded answering. A host without these
//!   cannot answer a question at all, so every host has them.
//! - [`WebTools`] — reaches the open internet. A zero-egress deployment says
//!   so by not composing it.
//! - [`ShellTools`] — runs commands as the invoking user. A privilege, and
//!   §10 "Decisions taken" 1 keeps it out of a long-lived daemon.
//! - [`KnowledgeFrontDoor`] — the unified `knowledge_lookup` envelope plus
//!   attached-document search.
//! - [`CodeIntelTools`] — reads a SCIP graph and a code corpus. Gated by
//!   CONSTRUCTION: a host can only compose this from a graph handle it owns,
//!   which is what makes "code intel over another tenant's workspace"
//!   unrepresentable rather than merely disallowed.
//! - [`NotesTools`] — writes to a note store the host opened.
//! - [`ComputeTools`], [`DocumentOperations`] — single-tool families, kept
//!   separate because the hosts that have them differ.
//!
//! # These are today's memberships, deliberately
//!
//! Composing a bundle a host did not previously have CHANGES WHAT THE MODEL
//! MAY CALL, which changes answers — a §18.6 re-baseline, not a refactor. So
//! this module's arrival preserves every host's set exactly. What it changes
//! is that the sets are now readable side by side as bundle lists, so closing
//! the divergence is a one-line diff with a bench behind it instead of a
//! 215-line block nobody could diff.

use std::sync::Arc;

use sovereign_contracts::tool_bundle::{BundleReport, ToolBundle};
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::ToolRegistry;

use async_trait::async_trait;

/// Whether this host's turn can reach the open internet, and if not, why.
///
/// A closed set of two as an enum rather than an `Option<Client>`, because
/// the arms are not symmetric: one is a capability, the other is a decision
/// with a reason an operator has to be able to find (ARCH §2.1, §18.3).
pub enum WebReach {
    /// An HTTP client built by the host's egress boundary. `sovereign-tools`
    /// is contract-side and does not construct an egress-capable client
    /// itself (order deep-research-t2a).
    Granted(reqwest::Client),
    /// No egress, and the reason: a `--no-default-features` build, an
    /// air-gapped deployment. Travels into the [`BundleReport`].
    Withheld(&'static str),
}

/// Corpus-grounded answering — the tools a turn cannot be a turn without.
///
/// `search` is here rather than in [`WebTools`] because corpus search IS the
/// product; the open-web fallback is a separate capability layered onto the
/// same tool, which is why [`WebReach`] is a field rather than a sibling
/// bundle.
pub struct CoreTurnTools {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
    corpus_engine: Arc<corpus_engine::CorpusEngine>,
    web: WebReach,
}

impl CoreTurnTools {
    /// Build the family. `web` decides whether `search` gets its open-web
    /// fallback.
    pub fn new(
        store: Arc<dyn StateStore>,
        inference: Arc<dyn InferenceProvider>,
        corpus_engine: Arc<corpus_engine::CorpusEngine>,
        web: WebReach,
    ) -> Self {
        Self {
            store,
            inference,
            corpus_engine,
            web,
        }
    }
}

#[async_trait]
impl ToolBundle for CoreTurnTools {
    fn name(&self) -> &'static str {
        "core-turn"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        let mut r = BundleReport::new(self.name());

        r = r.registered(reg.register_reporting(Box::new(
            crate::document::DocumentTool::new(
                Arc::clone(&self.store),
                Arc::clone(&self.inference),
            )
            .declared(),
        )));
        r = r.registered(reg.register_reporting(Box::new(
            crate::ClaimSearchTool::new(Arc::clone(&self.corpus_engine)).declared(),
        )));
        r = r.registered(reg.register_reporting(Box::new(
            crate::EpistemicLandscapeTool::new(Arc::clone(&self.corpus_engine)).declared(),
        )));
        // Deterministic land-value-tax analytics over parcel corpora — pre-cited
        // figures the ComplexTask synthesizer quotes verbatim.
        r = r.registered(reg.register_reporting(Box::new(
            crate::parcel_analytics::ParcelAnalyticsTool::new(Arc::clone(&self.corpus_engine))
                .declared(),
        )));
        // Typed SEC-filing figures with basis + accession, or first-class
        // refusals (FINANCIAL_CORPORA §6).
        r = r.registered(reg.register_reporting(Box::new(
            crate::sec_facts::SecFactsTool::new(Arc::clone(&self.corpus_engine)).declared(),
        )));

        match &self.web {
            WebReach::Granted(client) => {
                r = r.registered(reg.register_reporting(Box::new(
                    crate::search::SearchTool::with_web(
                        Arc::clone(&self.store),
                        Arc::clone(&self.inference),
                        client.clone(),
                        // DuckDuckGo — free, no key required.
                        crate::web::search::SearchBackend::DuckDuckGo,
                    ),
                )));
            }
            WebReach::Withheld(why) => {
                r = r.registered(reg.register_reporting(Box::new(
                    crate::search::SearchTool::new(
                        Arc::clone(&self.store),
                        Arc::clone(&self.inference),
                    ),
                )));
                r = r.withheld("search:web-fallback", *why);
            }
        }

        r
    }
}

/// Tools that reach the open internet.
///
/// `web_fetch` retrieves ANY url the model emits — validation is scheme-only
/// and there is no host allowlist — and `wikipedia_fetch` reaches
/// en.wikipedia.org. A deployment that must not egress declares
/// [`Withheld`](sovereign_contracts::tool_bundle::Withheld) in place of this.
pub struct WebTools {
    corpus_engine: Arc<corpus_engine::CorpusEngine>,
}

impl WebTools {
    /// Build the family. The corpus engine backs `wikipedia_fetch`'s local
    /// cache lookup before it reaches the network.
    pub fn new(corpus_engine: Arc<corpus_engine::CorpusEngine>) -> Self {
        Self { corpus_engine }
    }
}

#[async_trait]
impl ToolBundle for WebTools {
    fn name(&self) -> &'static str {
        "web"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name())
            .registered(reg.register_reporting(Box::new(crate::web::WebFetchTool::new())))
            .registered(reg.register_reporting(Box::new(
                crate::WikipediaFetchTool::new(Arc::clone(&self.corpus_engine)).declared(),
            )))
    }
}

/// Shell execution, as the invoking user, in the invoking directory.
///
/// Correct for an interactive CLI running one command someone is watching.
/// `quality/TOPOLOGY.md` §10 "Decisions taken" 1 keeps it out of a long-lived
/// daemon running as a different user with a different cwd — a daemon
/// composes `Withheld::new("shell", …)` in this slot, so the decision stays
/// a value someone wrote down.
pub struct ShellTools;

#[async_trait]
impl ToolBundle for ShellTools {
    fn name(&self) -> &'static str {
        "shell"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name())
            .registered(reg.register_reporting(Box::new(crate::shell::ShellTool)))
    }
}

/// The unified knowledge front door plus attached-document search.
///
/// `knowledge_lookup` returns a single Evidence envelope across the corpus,
/// memory and note channels (Tool-Mastery Phase 5).
/// `attached_document_search` is registered unconditionally: on a
/// conversation with no `DocumentSession` its `execute()` returns a clear
/// "no document attached" payload, so the model can probe it harmlessly
/// (decision 7693f16b — attached docs as Tool, not parallel pipeline).
pub struct KnowledgeFrontDoor {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
}

impl KnowledgeFrontDoor {
    /// Build the family.
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self { store, inference }
    }
}

#[async_trait]
impl ToolBundle for KnowledgeFrontDoor {
    fn name(&self) -> &'static str {
        "knowledge-front-door"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name())
            .registered(reg.register_reporting(Box::new(
                crate::KnowledgeLookupTool::new(
                    Arc::clone(&self.store),
                    Arc::clone(&self.inference),
                )
                .declared(),
            )))
            .registered(reg.register_reporting(Box::new(
                crate::AttachedDocumentSearchTool::new(
                    Arc::clone(&self.store),
                    Arc::clone(&self.inference),
                )
                .declared(),
            )))
    }
}

/// Code intelligence over a SCIP graph and a code corpus.
///
/// Behind `treesitter`, like the tools it registers: a build without it has
/// no code-intel surface to compose.
///
/// **The privilege is the handle.** This bundle cannot be constructed without
/// a [`ScipGraphHandle`](crate::ScipGraphHandle) and a corpus engine, so a
/// host may only offer code intel over an index it actually owns. That is why
/// "should the shared registry carry code intel on a multi-tenant hub?" is not
/// a policy question: a tenant-scoped host has no other tenant's handle to
/// compose from.
#[cfg(feature = "treesitter")]
pub struct CodeIntelTools {
    corpus_engine: Arc<corpus_engine::CorpusEngine>,
    inference: Arc<dyn InferenceProvider>,
    scip_graph: crate::ScipGraphHandle,
}

#[cfg(feature = "treesitter")]
impl CodeIntelTools {
    /// Build the family over a graph handle the host owns.
    pub fn new(
        corpus_engine: Arc<corpus_engine::CorpusEngine>,
        inference: Arc<dyn InferenceProvider>,
        scip_graph: crate::ScipGraphHandle,
    ) -> Self {
        Self {
            corpus_engine,
            inference,
            scip_graph,
        }
    }
}

#[cfg(feature = "treesitter")]
#[async_trait]
impl ToolBundle for CodeIntelTools {
    fn name(&self) -> &'static str {
        "code-intel"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        let health = Arc::new(crate::IndexHealthChecker::new(Arc::clone(&self.scip_graph)));
        BundleReport::new(self.name())
            .registered(reg.register_reporting(Box::new(
                crate::SymbolLookupTool::new(
                    Arc::clone(&self.corpus_engine),
                    Arc::clone(&self.scip_graph),
                )
                .with_health_checker(Arc::clone(&health))
                .declared(),
            )))
            .registered(reg.register_reporting(Box::new(
                crate::CodeSearchTool::new(Arc::clone(&self.corpus_engine))
                    .with_inference(Arc::clone(&self.inference))
                    .declared(),
            )))
            .registered(reg.register_reporting(Box::new(
                crate::RecentChangesTool::new(Arc::clone(&self.corpus_engine)).declared(),
            )))
            .registered(reg.register_reporting(Box::new(
                crate::FindCalleesTool::new(
                    Arc::clone(&self.corpus_engine),
                    Arc::clone(&self.scip_graph),
                )
                .with_health_checker(Arc::clone(&health))
                .declared(),
            )))
            .registered(reg.register_reporting(Box::new(
                crate::FindCallersTool::new(
                    Arc::clone(&self.corpus_engine),
                    Arc::clone(&self.scip_graph),
                )
                .with_health_checker(Arc::clone(&health))
                .declared(),
            )))
            .registered(reg.register_reporting(Box::new(
                crate::CapabilityMapTool::new().declared(),
            )))
    }
}

/// Working notes — persist across sessions, used for session attribution.
///
/// Takes an ALREADY-OPEN store: one writer per data root (TOPOLOGY phase 1),
/// so a bundle never opens a database.
#[cfg(feature = "treesitter")]
pub struct NotesTools {
    notes: Arc<corpus_engine_notes::NoteStore>,
}

#[cfg(feature = "treesitter")]
impl NotesTools {
    /// Build the family over a note store the host opened.
    pub fn new(notes: Arc<corpus_engine_notes::NoteStore>) -> Self {
        Self { notes }
    }
}

#[cfg(feature = "treesitter")]
#[async_trait]
impl ToolBundle for NotesTools {
    fn name(&self) -> &'static str {
        "notes"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name())
            .registered(reg.register_reporting(Box::new(
                crate::WriteNoteTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .registered(reg.register_reporting(Box::new(
                crate::ReadNotesTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .registered(reg.register_reporting(Box::new(
                crate::DeleteNoteTool::new(Arc::clone(&self.notes)).declared(),
            )))
    }
}

/// Sandboxed script execution (`compute`).
pub struct ComputeTools;

#[async_trait]
impl ToolBundle for ComputeTools {
    fn name(&self) -> &'static str {
        "compute"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name())
            .registered(reg.register_reporting(Box::new(crate::compute::ComputeTool.declared())))
    }
}

/// Structured edits over an attached document (`document_operation`).
pub struct DocumentOperations {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
}

impl DocumentOperations {
    /// Build the family.
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self { store, inference }
    }
}

#[async_trait]
impl ToolBundle for DocumentOperations {
    fn name(&self) -> &'static str {
        "document-operations"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name()).registered(reg.register_reporting(Box::new(
            crate::DocumentOperationTool::new(
                Arc::clone(&self.store),
                Arc::clone(&self.inference),
            )
            .declared(),
        )))
    }
}

/// The recipe-authoring workspace, driven headlessly over the conversation
/// API by a conversation tagged `skill_id = "recipe-author"`.
///
/// Two of its stores are optional and their absence is a DEGRADATION, not a
/// decision: `notes.db` backs the decision-log and research-finding tools,
/// `features.db` backs checkpoint and capability-request. A host that could
/// not open one composes the bundle without it, and the missing tools come
/// back in the [`BundleReport`] with the reason — which is what the server's
/// scattered `tracing::warn!` calls used to do, in a place nothing could read
/// back (ARCH §18.3).
pub struct RecipeAuthoringTools {
    notes: Option<Arc<dyn sovereign_contracts::recipe::notes::RecipeNotes>>,
    features: Option<Arc<sovereign_recipe_author::recipe_project_store::RecipeProjectStore>>,
}

impl Default for RecipeAuthoringTools {
    fn default() -> Self {
        Self::new()
    }
}

impl RecipeAuthoringTools {
    /// The seven tools that need no store.
    pub fn new() -> Self {
        Self {
            notes: None,
            features: None,
        }
    }

    /// Add the note-backed tools. The adapter, not the concrete store: the
    /// recipe-author tools take the `RecipeNotes` contract.
    pub fn with_notes(
        mut self,
        notes: Arc<dyn sovereign_contracts::recipe::notes::RecipeNotes>,
    ) -> Self {
        self.notes = Some(notes);
        self
    }

    /// Add the feature-store-backed tools. Requires notes as well — both
    /// `CheckpointTool` and `CapabilityRequestTool` take the pair.
    pub fn with_features(mut self, features: Arc<sovereign_recipe_author::recipe_project_store::RecipeProjectStore>) -> Self {
        self.features = Some(features);
        self
    }
}

#[async_trait]
impl ToolBundle for RecipeAuthoringTools {
    fn name(&self) -> &'static str {
        "recipe-authoring"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        use crate::recipe_author::{
            CapabilityRequestTool, CheckpointTool, DecisionLogTool, ProbeUrlTool, RecipeReadTool,
            RecipeTestTool, RecipeValidateTool, RecipeWriteStructuredTool, RecipeWriteTool,
            RegistryBrowseTool, ResearchFindingTool,
        };
        use crate::recipe_tester_adapter::CorpusEngineRecipeTester;

        let mut r = BundleReport::new(self.name());
        r = r.registered(reg.register_reporting(Box::new(RecipeReadTool::new())));
        r = r.registered(reg.register_reporting(Box::new(RecipeWriteTool::new())));
        r = r.registered(reg.register_reporting(Box::new(RecipeWriteStructuredTool::new(
            Arc::new(CorpusEngineRecipeTester::new()),
        ))));
        r = r.registered(reg.register_reporting(Box::new(RecipeValidateTool::new(Arc::new(
            CorpusEngineRecipeTester::new(),
        )))));
        r = r.registered(reg.register_reporting(Box::new(RecipeTestTool::new(Arc::new(
            CorpusEngineRecipeTester::new(),
        )))));
        r = r.registered(reg.register_reporting(Box::new(RegistryBrowseTool)));
        r = r.registered(reg.register_reporting(Box::new(ProbeUrlTool::new())));

        match &self.notes {
            Some(notes) => {
                r = r.registered(reg.register_reporting(Box::new(DecisionLogTool::with_notes(
                    Arc::clone(notes),
                ))));
                r = r.registered(reg.register_reporting(Box::new(
                    ResearchFindingTool::with_notes(Arc::clone(notes)),
                )));
                match &self.features {
                    Some(features) => {
                        r = r.registered(reg.register_reporting(Box::new(
                            CheckpointTool::with_stores(Arc::clone(notes), Arc::clone(features)),
                        )));
                        r = r.registered(reg.register_reporting(Box::new(
                            CapabilityRequestTool::with_stores(
                                Arc::clone(notes),
                                Arc::clone(features),
                            ),
                        )));
                    }
                    None => {
                        r = r.withheld(
                            "checkpoint, capability_request",
                            "no recipe feature store on this host",
                        );
                    }
                }
            }
            None => {
                r = r.withheld(
                    "decision_log, research_finding, checkpoint, capability_request",
                    "no note store on this host",
                );
            }
        }

        r
    }
}
