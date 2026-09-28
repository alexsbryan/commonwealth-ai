// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `work-atlas` tool bundle (phase-b pb-code-server): the four claim
//! tools over one store the host built. The store dials cw-rails' KV
//! (pb-atlas-kv), and the bundle registers whether or not cw-rails is up: a
//! refused dial answers at call time, naming `svrn mesh up`, so a cw-rails
//! that comes up later is used without a restart.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sovereign_contracts::registry::ToolRegistry;
use sovereign_contracts::tool_bundle::{BundleReport, ToolBundle};

use super::{
    ClaimBroadcaster, DeclareScopeTool, ReleaseScopeTool, ResourceMayITool, WorkInFlightTool,
};
use crate::{WorkAtlasConfig, WorkAtlasStore};

/// `declare_scope`, `release_scope`, `work_in_flight` and `resource_may_i`.
pub struct WorkAtlasTools {
    store: Arc<WorkAtlasStore>,
    config: WorkAtlasConfig,
    broadcaster: Arc<dyn ClaimBroadcaster>,
    repo_root: PathBuf,
    repo_id: String,
    branch: Option<String>,
}

impl WorkAtlasTools {
    /// Claims are scoped to `repo_id` (empty when the host found no repo,
    /// which `declare_scope` refuses by name) on `branch`.
    pub fn new(
        store: Arc<WorkAtlasStore>,
        config: WorkAtlasConfig,
        broadcaster: Arc<dyn ClaimBroadcaster>,
        repo_root: PathBuf,
        repo_id: String,
        branch: Option<String>,
    ) -> Self {
        Self {
            store,
            config,
            broadcaster,
            repo_root,
            repo_id,
            branch,
        }
    }
}

#[async_trait]
impl ToolBundle for WorkAtlasTools {
    fn name(&self) -> &'static str {
        "work-atlas"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name())
            .record(
                reg.register_reporting(Box::new(
                    DeclareScopeTool::new(
                        Arc::clone(&self.store),
                        self.config.clone(),
                        Arc::clone(&self.broadcaster),
                        self.repo_root.clone(),
                        self.repo_id.clone(),
                        self.branch.clone(),
                    )
                    .declared(),
                )),
            )
            .record(
                reg.register_reporting(Box::new(
                    ReleaseScopeTool::new(Arc::clone(&self.store), Arc::clone(&self.broadcaster))
                        .declared(),
                )),
            )
            .record(reg.register_reporting(Box::new(
                WorkInFlightTool::new(Arc::clone(&self.store)).declared(),
            )))
            .record(reg.register_reporting(Box::new(
                ResourceMayITool::new(Arc::clone(&self.store)).declared(),
            )))
    }
}
