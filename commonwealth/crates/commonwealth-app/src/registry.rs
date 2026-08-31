// SPDX-License-Identifier: AGPL-3.0-or-later
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

    /// Merge a manifest received via gossip. No-op when the app is already
    /// present at the same or a newer version. Returns true if the registry
    /// was updated.
    ///
    /// Versions compare **numerically per component**, not as strings. The
    /// string compare this replaced said `"10.0.0" < "9.0.0"`, so the tenth
    /// release of an app could never displace the ninth — and it failed
    /// silently, in the direction that keeps the OLD bundle.
    pub async fn merge(&self, manifest: MeshAppManifest) -> bool {
        let mut apps = self.apps.write().await;
        match apps.get(&manifest.app_id) {
            Some(existing) if !is_newer(&manifest.version, &existing.version) => false,
            _ => {
                apps.insert(manifest.app_id.clone(), manifest);
                true
            }
        }
    }
}

/// Is `candidate` a strictly newer version than `current`?
///
/// Dotted numeric components, compared left to right, missing components
/// treated as zero (`1.2` and `1.2.0` are the same version). A component that
/// is not a number sorts as zero rather than making the whole comparison
/// error — a manifest is a peer's data, and refusing to merge on a malformed
/// version would let one bad publish freeze an app's updates forever.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|c| {
                c.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    let (a, b) = (parts(candidate), parts(current));
    let len = a.len().max(b.len());
    for i in 0..len {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

impl Default for AppRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect this replaced, stated as a test: a string compare put the
    /// tenth release BELOW the ninth, so an app could never pass version 9.
    #[test]
    fn ten_is_newer_than_nine() {
        assert!(is_newer("10.0.0", "9.0.0"));
        assert!("10.0.0" < "9.0.0", "the string compare really was backwards");
    }

    #[test]
    fn version_comparison_is_per_component_and_not_reflexive() {
        assert!(is_newer("1.2.1", "1.2.0"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.2.0", "1.2.0"), "same version is not newer");
        assert!(!is_newer("1.2.0", "1.2.1"));
        assert!(!is_newer("1.2", "1.2.0"), "missing components are zero");
        assert!(is_newer("1.2.1", "1.2"));
    }

    /// A peer's malformed version must not freeze an app's updates.
    #[test]
    fn a_malformed_component_sorts_as_zero_rather_than_refusing() {
        assert!(is_newer("1.0.1", "1.0.x"));
        assert!(!is_newer("1.0.x", "1.0.1"));
        assert!(is_newer("2.0.0-beta", "1.0.0"));
    }
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
