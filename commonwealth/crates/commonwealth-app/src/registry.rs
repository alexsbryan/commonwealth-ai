//! In-memory app registry. Kept up-to-date via gossip.

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

    /// Merge a manifest received via gossip. No-op if app_id is already present
    /// with the same or newer version (version compared lexicographically for now).
    /// Returns true if the registry was updated.
    pub async fn merge(&self, manifest: MeshAppManifest) -> bool {
        let mut apps = self.apps.write().await;
        match apps.get(&manifest.app_id) {
            Some(existing) if existing.version >= manifest.version => false,
            _ => {
                apps.insert(manifest.app_id.clone(), manifest);
                true
            }
        }
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
    async fn merge_newer_wins() {
        let reg = AppRegistry::new();
        reg.register(manifest("com.test.app", "1.0.0")).await;
        assert!(reg.merge(manifest("com.test.app", "2.0.0")).await);
        assert_eq!(reg.get("com.test.app").await.unwrap().version, "2.0.0");
    }

    #[tokio::test]
    async fn merge_older_rejected() {
        let reg = AppRegistry::new();
        reg.register(manifest("com.test.app", "2.0.0")).await;
        assert!(!reg.merge(manifest("com.test.app", "1.0.0")).await);
        assert_eq!(reg.get("com.test.app").await.unwrap().version, "2.0.0");
    }

    #[tokio::test]
    async fn unregister() {
        let reg = AppRegistry::new();
        reg.register(manifest("com.test.app", "1.0.0")).await;
        assert!(reg.unregister("com.test.app").await);
        assert!(reg.get("com.test.app").await.is_none());
    }
}
