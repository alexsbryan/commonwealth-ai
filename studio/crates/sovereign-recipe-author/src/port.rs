// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's implementor of the recipe-project port
//! (`sovereign_contracts::recipe::project`, pb-ingest-rehome-daemon): the
//! store and the project model this crate owns, over the notes svrn hands in.
//! The route behaviour svrn's tests drive on the port's double is re-asserted
//! here, on the real store.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::recipe::notes::RecipeNotes;
use sovereign_contracts::recipe::project::{
    ProjectSummary, RecipeProjectHandle, RecipeProjectPort, RecipeProjectRow,
};

use crate::project::{ArtifactKind, CheckpointMeta, RecipeProject};
use crate::recipe_project_store::{RecipeProjectError, RecipeProjectStore};

/// The recipe-project port over one `features.db` and svrn's notes.
pub struct ProjectStorePort {
    notes: Arc<dyn RecipeNotes>,
    store: Arc<RecipeProjectStore>,
}

impl ProjectStorePort {
    /// The port over `store`, composing projects with `notes`.
    pub fn new(notes: Arc<dyn RecipeNotes>, store: Arc<RecipeProjectStore>) -> Self {
        Self { notes, store }
    }
}

/// The store's error as the contract's: a precondition the caller can fix
/// stays `InvalidInput` in the store's own words.
fn store_err(e: RecipeProjectError) -> Error {
    match e {
        RecipeProjectError::InvalidInput(s) => Error::InvalidInput(s),
        other => Error::Storage(other.to_string()),
    }
}

#[async_trait]
impl RecipeProjectHandle for RecipeProject {
    fn feature_id(&self) -> &str {
        RecipeProject::feature_id(self)
    }

    fn read_summary(&self) -> Result<ProjectSummary> {
        RecipeProject::read_summary(self)
    }

    fn write_summary(&self, summary: &ProjectSummary) -> Result<()> {
        RecipeProject::write_summary(self, summary)
    }

    fn list_checkpoints(&self) -> Result<Vec<CheckpointMeta>> {
        RecipeProject::list_checkpoints(self)
    }

    async fn restore_checkpoint(
        &self,
        checkpoint_id: &str,
        artifact_id: Option<&str>,
        session_id: &str,
    ) -> Result<String> {
        crate::checkpoint::restore_checkpoint(self, checkpoint_id, artifact_id, None, session_id)
            .await
            .map(|outcome| outcome.checkpoint_id)
    }

    async fn situated_context(&self) -> Result<String> {
        crate::situated_context::render(self).await
    }
}

#[async_trait]
impl RecipeProjectPort for ProjectStorePort {
    async fn list(&self, include_archived: bool) -> Result<Vec<RecipeProjectRow>> {
        self.store.list(include_archived).await.map_err(store_err)
    }

    async fn get(&self, id: &str) -> Result<Option<RecipeProjectRow>> {
        self.store.get(id).await.map_err(store_err)
    }

    async fn provision(&self, id: &str, title: &str, charter_md: &str) -> Result<RecipeProjectRow> {
        self.store
            .provision_recipe_project(id, title, charter_md)
            .await
            .map_err(store_err)
    }

    async fn create(
        &self,
        title: &str,
        charter_md: &str,
        kind: ArtifactKind,
    ) -> Result<Box<dyn RecipeProjectHandle>> {
        let project = RecipeProject::new_with_kind(
            title,
            charter_md,
            kind,
            Arc::clone(&self.notes),
            Arc::clone(&self.store),
        )
        .await?;
        Ok(Box::new(project))
    }

    async fn load(&self, feature_id: &str) -> Result<Box<dyn RecipeProjectHandle>> {
        let project =
            RecipeProject::load(feature_id, Arc::clone(&self.notes), Arc::clone(&self.store))
                .await?;
        Ok(Box::new(project))
    }

    fn artifact_root(&self, kind: ArtifactKind) -> Option<PathBuf> {
        match kind {
            ArtifactKind::Recipe => crate::local_recipes_dir().ok(),
            ArtifactKind::Workflow => crate::local_workflows_dir().ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home_test_lock;
    use crate::test_support::InMemoryRecipeNotes;

    fn port_in(dir: &std::path::Path) -> ProjectStorePort {
        let store = RecipeProjectStore::open(&dir.join("features.db")).expect("open features.db");
        ProjectStorePort::new(Arc::new(InMemoryRecipeNotes::default()), Arc::new(store))
    }

    /// The store half of svrn's `/v1/features/projects` round trip
    /// (d6_surface_e2e drives it on the double): a row answers with its
    /// timestamps, reads back whole, lists, and a taken id is the caller's
    /// `InvalidInput` naming the conflict, which the route renders 409.
    #[tokio::test]
    async fn provision_reads_back_and_a_taken_id_is_invalid_input() {
        let dir = tempfile::tempdir().expect("tempdir");
        let port = port_in(dir.path());
        assert!(port.list(false).await.expect("list").is_empty());

        let row = port
            .provision("feat-quiet-hours", "Quiet hours", "# Charter\n\nWhole.")
            .await
            .expect("provision");
        assert!(row.created_at > 0);
        let one = port
            .get("feat-quiet-hours")
            .await
            .expect("get")
            .expect("row");
        assert_eq!(one.charter_md, "# Charter\n\nWhole.");
        assert_eq!(port.list(false).await.expect("list").len(), 1);
        assert!(port.get("nope").await.expect("get").is_none());

        match port.provision("feat-quiet-hours", "Again", "").await {
            Err(Error::InvalidInput(why)) => assert!(why.contains("already exists"), "{why}"),
            other => panic!("a taken id must be InvalidInput, got {other:?}"),
        }
        match port.provision("", "Empty", "").await {
            Err(Error::InvalidInput(why)) => assert!(!why.contains("already exists"), "{why}"),
            other => panic!("an empty id must be InvalidInput, got {other:?}"),
        }
    }

    /// The model half of svrn's `/v1/recipe-projects` routes (d8_surface_e2e
    /// drives them on the double): a created project lays its tree down under
    /// the artifact root's sibling, lists, loads with a recipe summary and no
    /// artifact, and renders its situated context; an unknown id does not load.
    #[tokio::test]
    async fn a_created_project_lays_down_its_tree_lists_and_loads() {
        let _guard = home_test_lock();
        let home = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOME", home.path());
        let port = port_in(home.path());

        let created = port
            .create(
                "Roman coin hoards",
                "Catalogue the hoards.",
                ArtifactKind::Recipe,
            )
            .await
            .expect("create");
        let id = created.feature_id().to_string();
        assert!(home
            .path()
            .join(".svrnmesh")
            .join("recipe-projects")
            .join(&id)
            .exists());
        assert_eq!(port.list(false).await.expect("list")[0].id, id);

        let loaded = port.load(&id).await.expect("load");
        let summary = loaded.read_summary().expect("summary");
        assert_eq!(summary.artifact_kind, ArtifactKind::Recipe);
        assert!(summary.recipe_id.is_none());
        assert!(loaded.list_checkpoints().is_ok());
        assert!(loaded
            .situated_context()
            .await
            .expect("render")
            .contains("Roman coin hoards"));
        assert_eq!(
            port.artifact_root(ArtifactKind::Recipe),
            Some(home.path().join(".svrnmesh").join("recipes"))
        );
        assert!(port.load("nope").await.is_err());
    }
}
