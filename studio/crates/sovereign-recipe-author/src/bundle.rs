// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-authoring tool family as a [`ToolBundle`]: ingest's, beside the
//! tools it registers (moved from `sovereign_tools::bundles`,
//! pb-ingest-rehome-daemon, which re-exports it at that path).

use std::sync::Arc;

use async_trait::async_trait;
use sovereign_contracts::tool_bundle::{BundleReport, ToolBundle};
use sovereign_contracts::ToolRegistry;

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
    features: Option<Arc<crate::recipe_project_store::RecipeProjectStore>>,
    /// The tester, the variant-catalog descriptor and the bundled registry
    /// snapshot — ingest's, handed in by the host that composes it
    /// (`corpus_engine::recipe_tester::recipe_author_seams`); the package
    /// tools take the values and never read the repo.
    seams: sovereign_contracts::recipe::testing::RecipeAuthorSeams,
}

impl RecipeAuthoringTools {
    /// The seven tools that need no store.
    pub fn new(seams: sovereign_contracts::recipe::testing::RecipeAuthorSeams) -> Self {
        Self {
            notes: None,
            features: None,
            seams,
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
    pub fn with_features(
        mut self,
        features: Arc<crate::recipe_project_store::RecipeProjectStore>,
    ) -> Self {
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
        use crate::{
            CapabilityRequestTool, CheckpointTool, DecisionLogTool, ProbeUrlTool, RecipeReadTool,
            RecipeTestTool, RecipeValidateTool, RecipeWriteStructuredTool, RecipeWriteTool,
            RegistryBrowseTool, ResearchFindingTool,
        };

        let mut r = BundleReport::new(self.name());
        r = r.record(reg.register_reporting(Box::new(RecipeReadTool::new())));
        r = r.record(reg.register_reporting(Box::new(RecipeWriteTool::new())));
        r = r.record(
            reg.register_reporting(Box::new(RecipeWriteStructuredTool::new(
                Arc::clone(&self.seams.tester),
                self.seams.descriptor_json,
            ))),
        );
        r = r.record(
            reg.register_reporting(Box::new(RecipeValidateTool::new(Arc::clone(
                &self.seams.tester,
            )))),
        );
        r = r.record(
            reg.register_reporting(Box::new(RecipeTestTool::new(Arc::clone(
                &self.seams.tester,
            )))),
        );
        r = r.record(
            reg.register_reporting(Box::new(RegistryBrowseTool::new(self.seams.registry_toml))),
        );
        r = r.record(reg.register_reporting(Box::new(ProbeUrlTool::new())));

        match &self.notes {
            Some(notes) => {
                r =
                    r.record(reg.register_reporting(Box::new(DecisionLogTool::with_notes(
                        Arc::clone(notes),
                    ))));
                r = r.record(
                    reg.register_reporting(Box::new(ResearchFindingTool::with_notes(Arc::clone(
                        notes,
                    )))),
                );
                match &self.features {
                    Some(features) => {
                        r = r.record(reg.register_reporting(Box::new(
                            CheckpointTool::with_stores(Arc::clone(notes), Arc::clone(features)),
                        )));
                        // The inbox directory is derived from the sovereign
                        // root, not supplied by the host — so wiring it here
                        // is what makes a submitted capability request land
                        // where `svrn maintainer inbox` reads it on EVERY
                        // host. Only the desktop called this, so a request
                        // submitted through the server or the daemon was
                        // written and then unreadable (ARCH §10.6).
                        let mut cap = CapabilityRequestTool::with_stores(
                            Arc::clone(notes),
                            Arc::clone(features),
                        );
                        match crate::maintainer_inbox_dir() {
                            Ok(dir) => cap = cap.with_inbox_dir(dir),
                            Err(e) => {
                                r = r.withheld(
                                    "capability_request:inbox",
                                    format!("maintainer inbox dir unresolved: {e}"),
                                )
                            }
                        }
                        r = r.record(reg.register_reporting(Box::new(cap)));
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
