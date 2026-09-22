// SPDX-License-Identifier: AGPL-3.0-or-later
//! Single source of truth for slot-role aliases.
//!
//! Two surfaces must agree about aliases, and historically didn't:
//!
//! 1. **Resolution** — `daemon.rs::register_local_model_slots` builds
//!    the `slot_aliases` map that inbound `/v1/chat/completions` and
//!    `list_models` use to translate `"primary"` /
//!    `"commonwealth/fast"` / `"coder"` into the loaded GGUF's name.
//! 2. **Advertisement** — `oicp_synthesis.rs::build_self_manifest`
//!    emits alias `ProviderModel` rows so *peers* see this node as a
//!    candidate when a request names an alias instead of a concrete
//!    GGUF id.
//!
//! When a role is resolvable but not advertised, every mesh request
//! for that alias 503s with "no node advertises model X" even though
//! the serving node would have handled it fine — observed 2026-05-19
//! for `fast`/`commonwealth/fast` (search-gym judge calls bounced
//! until the advertisement block landed). The fix was made twice, in
//! two files, with nothing enforcing agreement. This module is that
//! enforcement: both files now derive their alias sets from
//! [`SLOT_ALIAS_POLICY`], and the tests below pin the two derived
//! views against each other.
//!
//! **Adding a new slot role?** Add one row to [`SLOT_ALIAS_POLICY`].
//! If you mark it `mesh_advertised: true`, the parity test will fail
//! until `build_self_manifest` actually emits the alias rows — that
//! failure is the point; it's the reminder the 2026-05-19 bug never
//! got.

pub use sovereign_contracts::venue::{resolution_alias_keys, SlotAliasPolicy, SLOT_ALIAS_POLICY};

/// Alias ids `build_self_manifest` must ADVERTISE for a role —
/// namespaced form first (canonical), bare form second (the
/// OpenAI-client shortcut). Empty when the role isn't mesh-advertised.
pub fn advertised_alias_ids(role: &str) -> Vec<String> {
    let Some(policy) = SLOT_ALIAS_POLICY.iter().find(|p| p.role == role) else {
        return Vec::new();
    };
    if !policy.mesh_advertised {
        return Vec::new();
    }
    vec![
        format!("commonwealth/{}", policy.role),
        policy.role.to_string(),
    ]
}

#[cfg(test)]
mod parity_tests {
    use super::*;

    /// Every advertised alias must be resolvable by the daemon.
    /// If this fails: a peer's scheduler can SELECT this node for the
    /// alias, the request arrives, and the serving daemon can't map
    /// it to a slot — the inverse of the 2026-05-19 bug, worse
    /// because it fails after routing instead of before.
    #[test]
    fn every_advertised_alias_is_resolvable() {
        for policy in SLOT_ALIAS_POLICY {
            let resolvable = resolution_alias_keys(policy.role);
            for advertised in advertised_alias_ids(policy.role) {
                assert!(
                    resolvable.contains(&advertised),
                    "role `{}` advertises alias `{advertised}` to mesh peers but the \
                     daemon's slot_aliases map cannot resolve it — inbound requests \
                     for it will 404/503 AFTER a peer routes here. Add the form to \
                     resolution_alias_keys (it derives from SLOT_ALIAS_POLICY; this \
                     usually means role/synonym spelling drifted).",
                    policy.role
                );
            }
        }
    }

    /// Pin the exact resolution sets so a refactor of
    /// `register_local_model_slots` can't silently change what
    /// operators and opencode configs can address.
    #[test]
    fn resolution_keys_pinned() {
        let pin = |role: &str, expect: &[&str]| {
            let mut got = resolution_alias_keys(role);
            let mut want: Vec<String> = expect.iter().map(|s| s.to_string()).collect();
            got.sort();
            want.sort();
            assert_eq!(
                got, want,
                "resolution alias set for `{role}` changed — every key here is a \
                 published addressing contract (opencode provider maps, operator \
                 scripts). Removing one breaks existing clients; if intentional, \
                 update this pin AND the deprecation notes in SYSTEM_OVERVIEW §4."
            );
        };
        pin("primary", &["primary", "commonwealth/primary"]);
        pin("fast", &["fast", "commonwealth/fast"]);
        pin("embed", &["embed", "commonwealth/embed"]);
        pin(
            "code",
            &["code", "commonwealth/code", "coder", "commonwealth/coder"],
        );
        // Pool members and extras are addressed by literal key — no
        // alias indirection (their names are already operator-stable).
        pin("primary_0", &[]);
        pin("extras:scratch", &[]);
    }

    /// Advertised ids are pinned too: peers cache manifests, so the
    /// advertised vocabulary is a cross-node wire contract.
    #[test]
    fn advertised_ids_pinned() {
        assert_eq!(
            advertised_alias_ids("primary"),
            vec!["commonwealth/primary".to_string(), "primary".to_string()]
        );
        assert_eq!(
            advertised_alias_ids("fast"),
            vec!["commonwealth/fast".to_string(), "fast".to_string()]
        );
        assert!(
            advertised_alias_ids("embed").is_empty(),
            "embed must not be mesh-advertised — it is not a chat candidate \
             (build_self_manifest module doc). If you are intentionally making \
             embed routable, update SLOT_ALIAS_POLICY's rationale comment."
        );
        assert!(
            advertised_alias_ids("code").is_empty(),
            "code-role ALIASES are deliberately unadvertised (cold hot-swap slot; \
             see SLOT_ALIAS_POLICY rationale). The concrete code GGUF id is still \
             advertised with a `code` hint. If you flip mesh_advertised, you must \
             also add the advertisement block in build_self_manifest — see \
             manifest_advertises_every_mesh_advertised_role in oicp_synthesis.rs."
        );
    }
}
