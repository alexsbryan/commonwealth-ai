//! Mesh app manifest — the static description of a mesh application.

use serde::{Deserialize, Serialize};

/// Full description of a mesh application. Gossiped across all nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshAppManifest {
    /// Reverse-DNS style unique identifier, e.g. `com.example.myapp`.
    pub app_id: String,
    /// Human-readable display name.
    pub name: String,
    /// Semver version string, e.g. `"1.2.3"`.
    pub version: String,
    /// Path to the binary, or a URL for remote apps.
    pub entrypoint: String,
    /// Permissions this app requires on the mesh.
    #[serde(default)]
    pub permissions: AppPermissions,
    /// Hardware capabilities this app needs in order to run.
    #[serde(default)]
    pub required_capabilities: RequiredCapabilities,
}

/// Permissions that a mesh app may request.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AppPermissions {
    /// App needs read access to MeshStore.
    #[serde(default)]
    pub mesh_store_read: bool,
    /// App needs write access to MeshStore.
    #[serde(default)]
    pub mesh_store_write: bool,
    /// App needs access to the inference endpoints.
    #[serde(default)]
    pub inference_access: bool,
    /// App needs access to the knowledge/search endpoints.
    #[serde(default)]
    pub knowledge_access: bool,
}

/// Minimum hardware a node must have to host this app.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RequiredCapabilities {
    pub min_vram_gb: Option<u32>,
    pub min_ram_gb: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_serde_roundtrip() {
        let m = MeshAppManifest {
            app_id: "com.example.test".into(),
            name: "Test App".into(),
            version: "1.0.0".into(),
            entrypoint: "/usr/bin/test-app".into(),
            permissions: AppPermissions {
                mesh_store_read: true,
                mesh_store_write: true,
                inference_access: false,
                knowledge_access: false,
            },
            required_capabilities: RequiredCapabilities {
                min_vram_gb: None,
                min_ram_gb: Some(4),
            },
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: MeshAppManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
