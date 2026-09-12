// SPDX-License-Identifier: AGPL-3.0-or-later
//! In-memory app registry.
//!
//! NOT kept up to date via gossip, and never was. `merge` — a
//! last-writer-by-version reconciliation for manifests arriving from a
//! peer — was called only by `POST /internal/app/registry`, a receiver
//! with no sender anywhere in the workspace. Both went with cw-lift rung
//! 2c. What remains is the local half: the daemon's own
//! `register`/`unregister`/`get`/`list` over apps this node installed.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::manifest::MeshAppManifest;

/// In-memory registry of known mesh apps. Thread-safe.
#[derive(Clone)]
pub struct AppRegistry {
    apps: Arc<RwLock<HashMap<String, MeshAppManifest>>>,
}

impl AppRegistry {
    pub fn new() -> Self {
        Self {
            apps: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register or update an app manifest.
    pub async fn register(&self, manifest: MeshAppManifest) {
        self.apps
            .write()
            .await
            .insert(manifest.app_id.clone(), manifest);
    }

    /// Remove an app from the registry. Returns true if it existed.
    pub async fn unregister(&self, app_id: &str) -> bool {
        self.apps.write().await.remove(app_id).is_some()
    }

    /// Look up an app by ID.
    pub async fn get(&self, app_id: &str) -> Option<MeshAppManifest> {
        self.apps.read().await.get(app_id).cloned()
    }

    /// List all registered apps.
    pub async fn list(&self) -> Vec<MeshAppManifest> {
        self.apps.read().await.values().cloned().collect()
    }
}

impl Default for AppRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::manifest::MeshAppManifest;

    fn manifest(id: &str, version: &str) -> MeshAppManifest {
        MeshAppManifest {
            app_id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            entrypoint: "/bin/app".to_string(),
            permissions: Default::default(),
            required_capabilities: Default::default(),
        }
    }

    #[tokio::test]
    async fn register_and_list() {
        let reg = AppRegistry::new();
        reg.register(manifest("com.test.app", "1.0.0")).await;
        let list = reg.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].app_id, "com.test.app");
    }

    #[tokio::test]
    async fn unregister() {
        let reg = AppRegistry::new();
        reg.register(manifest("com.test.app", "1.0.0")).await;
        assert!(reg.unregister("com.test.app").await);
        assert!(reg.get("com.test.app").await.is_none());
    }
}
