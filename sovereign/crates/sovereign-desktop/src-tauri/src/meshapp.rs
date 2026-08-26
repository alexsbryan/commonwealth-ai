// SPDX-License-Identifier: AGPL-3.0-or-later
//! MeshApp bridge — host-side authorization model for sandboxed mesh-app
//! webviews (e.g. the SF land-value-tax explorer).
//!
//! A mesh app runs in a dedicated `meshapp-<app_id>` webview window. The
//! host exposes a tiny, permission-gated bridge (the `meshapp_*` Tauri
//! commands in `commands::meshapp`); this module owns the *authorization*
//! half:
//!
//! 1. **Who is calling** — the app id is derived from the calling
//!    webview's *label*, which the host sets at window creation. It is
//!    NEVER taken from a JS argument: code inside the sandbox cannot
//!    change its own window label, so it cannot impersonate another app.
//!
//! 2. **What it may do** — the GRANTED permission subset recorded at
//!    install time (in `DesktopConfig.meshapp_installs`) is authoritative,
//!    not whatever the manifest requested. An app with no install record
//!    is denied everything (fail-closed).
//!
//! Everything here is pure and unit-tested; the Tauri command layer is a
//! thin wrapper that calls [`authorize`] before doing any work.

use serde::{Deserialize, Serialize};

/// The four capabilities a mesh app can be granted. Mirrors
/// `commonwealth_app::AppPermissions` (kept local to avoid a
/// desktop → `commonwealth-app` dependency for a 4-bool struct; the
/// gossip-path manifest type stays decoupled from the desktop host).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeshAppPermissions {
    #[serde(default)]
    pub mesh_store_read: bool,
    #[serde(default)]
    pub mesh_store_write: bool,
    #[serde(default)]
    pub inference_access: bool,
    #[serde(default)]
    pub knowledge_access: bool,
}

/// Trust level of an installed app, from its manifest signature. v0 LVT
/// ships `Unsigned`; the consent sheet surfaces the badge so the user
/// grants with eyes open.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeshAppTrust {
    #[default]
    Unsigned,
    Signed,
}

/// A recorded install decision. The `granted` subset is what the bridge
/// enforces — it may be narrower than the manifest's request, because the
/// user can decline individual permissions at the consent sheet.
/// Persisted in `DesktopConfig.meshapp_installs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshAppInstall {
    pub app_id: String,
    pub name: String,
    pub granted: MeshAppPermissions,
    #[serde(default)]
    pub trust: MeshAppTrust,
    pub recorded_at_unix: i64,
}

/// One bridge capability, named so the gate reads declaratively at each
/// call site (`authorize(.., Permission::MeshStoreRead)`).
///
/// NOT `sovereign_contracts::types::routing::Permission` (the tool-consent
/// set: Network, FileRead, Shell, …); this is the four mesh-app BRIDGE
/// capabilities, over the local `MeshAppPermissions` mirror above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    MeshStoreRead,
    MeshStoreWrite,
    InferenceAccess,
    KnowledgeAccess,
}

impl Permission {
    fn granted_by(self, p: &MeshAppPermissions) -> bool {
        match self {
            Permission::MeshStoreRead => p.mesh_store_read,
            Permission::MeshStoreWrite => p.mesh_store_write,
            Permission::InferenceAccess => p.inference_access,
            Permission::KnowledgeAccess => p.knowledge_access,
        }
    }
}

/// Window-label prefix for mesh-app webviews. The host creates each app's
/// window with label `meshapp-<app_id>`, and `capabilities/meshapp.json`
/// scopes the bridge commands to `windows: ["meshapp-*"]` — so the bridge
/// is unreachable from the main window, and the main window's ~175
/// commands are unreachable from a mesh app. That exclusion is the
/// bidirectional isolation.
pub const MESHAPP_LABEL_PREFIX: &str = "meshapp-";

/// Derive the calling app's id from its webview label. `None` for any
/// label that isn't a mesh-app window (e.g. `main`) or is malformed —
/// such a caller resolves to "no app" and is denied.
pub fn app_id_from_label(label: &str) -> Option<String> {
    label
        .strip_prefix(MESHAPP_LABEL_PREFIX)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// The installed grant for `app_id`, if any. Fail-closed: an app with no
/// install record returns `None`.
pub fn resolve_grant<'a>(
    installs: &'a [MeshAppInstall],
    app_id: &str,
) -> Option<&'a MeshAppInstall> {
    installs.iter().find(|i| i.app_id == app_id)
}

/// The single authorization decision every bridge command makes before
/// doing any work. Given the calling webview's `label`, the installed
/// set, and the `needs` permission, returns the resolved `app_id` on
/// success or a human-readable denial reason (for the Tauri
/// `Result<_, String>`). Fail-closed across all three failure modes:
/// not-a-mesh-app-window, app-not-installed, permission-not-granted.
pub fn authorize(
    installs: &[MeshAppInstall],
    label: &str,
    needs: Permission,
) -> Result<String, String> {
    let app_id = app_id_from_label(label)
        .ok_or_else(|| "denied: caller is not a mesh-app window".to_string())?;
    let grant = resolve_grant(installs, &app_id)
        .ok_or_else(|| format!("denied: app `{app_id}` is not installed"))?;
    if needs.granted_by(&grant.granted) {
        Ok(app_id)
    } else {
        Err(format!("denied: app `{app_id}` was not granted {needs:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(app_id: &str, perms: MeshAppPermissions) -> MeshAppInstall {
        MeshAppInstall {
            app_id: app_id.to_string(),
            name: app_id.to_string(),
            granted: perms,
            trust: MeshAppTrust::Unsigned,
            recorded_at_unix: 0,
        }
    }

    #[test]
    fn app_id_derives_only_from_meshapp_labels() {
        assert_eq!(
            app_id_from_label("meshapp-com.sovereign.lvt").as_deref(),
            Some("com.sovereign.lvt")
        );
        // The host window and malformed labels are not mesh apps.
        assert_eq!(app_id_from_label("main"), None);
        assert_eq!(app_id_from_label("meshapp-"), None);
        assert_eq!(app_id_from_label(""), None);
    }

    #[test]
    fn authorize_grants_only_installed_and_permitted() {
        let installs = vec![install(
            "com.sovereign.lvt",
            MeshAppPermissions {
                mesh_store_read: true,
                ..Default::default()
            },
        )];
        // Installed + granted → ok, returns the app id.
        assert_eq!(
            authorize(
                &installs,
                "meshapp-com.sovereign.lvt",
                Permission::MeshStoreRead
            ),
            Ok("com.sovereign.lvt".to_string())
        );
    }

    #[test]
    fn authorize_is_fail_closed() {
        let installs = vec![install(
            "com.sovereign.lvt",
            MeshAppPermissions {
                mesh_store_read: true,
                ..Default::default()
            },
        )];
        // (a) granted read but not inference → denied.
        assert!(authorize(
            &installs,
            "meshapp-com.sovereign.lvt",
            Permission::InferenceAccess
        )
        .is_err());
        // (b) a different, uninstalled app → denied even for a perm some
        // other app has.
        assert!(authorize(&installs, "meshapp-com.evil.app", Permission::MeshStoreRead).is_err());
        // (c) the host main window cannot reach the bridge at all.
        assert!(authorize(&installs, "main", Permission::MeshStoreRead).is_err());
        // (d) empty install set → everything denied.
        assert!(authorize(&[], "meshapp-com.sovereign.lvt", Permission::MeshStoreRead).is_err());
    }

    #[test]
    fn permissions_serde_default_all_false() {
        // A bare manifest (no permissions block) grants nothing.
        let p: MeshAppPermissions = serde_json::from_str("{}").unwrap();
        assert_eq!(p, MeshAppPermissions::default());
        assert!(!p.mesh_store_read && !p.inference_access);
    }
}
