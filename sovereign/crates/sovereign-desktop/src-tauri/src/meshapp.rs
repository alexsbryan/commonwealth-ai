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
/// `sovereign_meshapp_registry::AppPermissions` (kept local to avoid a
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
/// window with label `meshapp-<app_id>`.
///
/// Tauri 2.11 does NOT gate app commands per window — the ACL check in
/// `webview/mod.rs` only applies to a crate carrying an app manifest, and
/// this crate's `build.rs` is a bare `tauri_build::build()` with no
/// `permissions/` directory — so `capabilities/meshapp.json` cannot decide
/// WHICH app commands a `meshapp-*` window may invoke. [`bridge_refusal`]
/// below is what actually decides that, at the one invoke closure in
/// `main.rs`.
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

/// The host commands a mesh-app window may invoke: exactly the names
/// `meshapp_shim.js` spells on `window.meshApp`, which is the only host
/// surface a first-party bundle calls.
///
/// This list is never hand-kept beside the shim —
/// `the_bridge_allowlist_is_exactly_the_shims_commands` in
/// `commands::meshapp` parses the `meshapp_*` names back out of the shim
/// source and asserts set-equality, so adding a command here without
/// adding it to `window.meshApp` (or the reverse) fails the test.
pub const MESHAPP_BRIDGE_COMMANDS: [&str; 18] = [
    "meshapp_capabilities",
    "meshapp_claims",
    "meshapp_corpus_stats",
    "meshapp_document_feed",
    "meshapp_findings",
    "meshapp_graph",
    "meshapp_node",
    "meshapp_open_outer_work",
    "meshapp_parcel_analytics",
    "meshapp_questions",
    "meshapp_read_chunk",
    "meshapp_read_corpus",
    "meshapp_reconciliation",
    "meshapp_search_entities",
    "meshapp_search_parcels",
    "meshapp_subgraph",
    "meshapp_timeline",
    "meshapp_wrapped_artifact",
];

/// Should this invoke be refused? Label and command in, refusal out:
/// `None` allows, `Some(sentence)` refuses and says why.
///
/// A window that is not a mesh app (`main`, and every other label) is
/// unchanged — it keeps the whole command surface. A `meshapp-*` window
/// may invoke only a name in [`MESHAPP_BRIDGE_COMMANDS`]; every other
/// command, including the host-only install management ones, is refused
/// before the command body runs. This is the ONE decider for that
/// question — the per-command hand guards that used to ask it in
/// `commands::meshapp` were deleted when this landed.
pub fn bridge_refusal(label: &str, command: &str) -> Option<String> {
    if app_id_from_label(label).is_none() {
        return None;
    }
    if MESHAPP_BRIDGE_COMMANDS.contains(&command) {
        return None;
    }
    Some(format!(
        "`{command}` is not one of the {} commands the mesh-app bridge offers, \
         so the app window `{label}` may not invoke it",
        MESHAPP_BRIDGE_COMMANDS.len()
    ))
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
    fn a_mesh_app_window_may_invoke_the_bridge_and_nothing_else() {
        // (a) a bridge command from an app window → allowed.
        assert!(bridge_refusal("meshapp-com.sovereign.lvt", "meshapp_read_corpus").is_none());
        // (b) a host command from an app window → refused, and the
        // sentence names the command so the devtools error says what
        // was asked for.
        let refusal = bridge_refusal("meshapp-com.sovereign.lvt", "mcp_set_token")
            .expect("a host command from an app window is refused");
        assert!(refusal.contains("mcp_set_token"), "{refusal}");
        // (c) install management is host-only — the three commands that
        // used to hand-check this are now decided here.
        for host_only in [
            "meshapp_record_install",
            "meshapp_uninstall",
            "meshapp_stage_corpus_recipe",
        ] {
            assert!(
                bridge_refusal("meshapp-com.sovereign.lvt", host_only).is_some(),
                "{host_only} must not be reachable from a mesh-app window"
            );
        }
        // (d) every other window is unchanged — the host's own windows
        // keep the whole command surface. A bare `meshapp-` resolves to
        // no app and is one of them; every mesh-app window the host
        // opens is labelled `meshapp-<app_id>` for an app that is
        // already installed (`meshapp_open` in `commands/meshapp.rs`).
        assert!(bridge_refusal("main", "mcp_set_token").is_none());
        assert!(bridge_refusal("meshapp-", "mcp_set_token").is_none());
    }

    #[test]
    fn permissions_serde_default_all_false() {
        // A bare manifest (no permissions block) grants nothing.
        let p: MeshAppPermissions = serde_json::from_str("{}").unwrap();
        assert_eq!(p, MeshAppPermissions::default());
        assert!(!p.mesh_store_read && !p.inference_access);
    }
}
