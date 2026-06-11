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

/// Alias policy for one canonical slot role.
pub struct SlotAliasPolicy {
    /// The canonical role name (`primary`, `fast`, `embed`, `code`).
    pub role: &'static str,
    /// Extra synonyms resolvable on inbound requests, beyond the
    /// bare role (e.g. operators say "coder" where OICP says "code").
    pub synonyms: &'static [&'static str],
    /// Whether `build_self_manifest` advertises this role's aliases
    /// as mesh-routable `ProviderModel` rows. `false` is a deliberate
    /// policy decision and must carry a rationale comment on the row.
    pub mesh_advertised: bool,
}

/// The canonical table. Every alias either site knows about derives
/// from here.
pub const SLOT_ALIAS_POLICY: &[SlotAliasPolicy] = &[
    SlotAliasPolicy {
        role: "primary",
        synonyms: &[],
        mesh_advertised: true,
    },
    SlotAliasPolicy {
        role: "fast",
        synonyms: &[],
        mesh_advertised: true,
    },
    SlotAliasPolicy {
        role: "embed",
        // Deliberately not advertised: the embed slot is never a
        // chat-completion candidate and peer selection never consults
        // it (see `build_self_manifest`'s module doc). Local
        // resolution still wants the alias so `/v1/embeddings`-side
        // callers can address the slot by role.
        synonyms: &[],
        mesh_advertised: false,
    },
    SlotAliasPolicy {
        role: "code",
        // Deliberately not advertised AS AN ALIAS today: the code
        // slot is advertised under its concrete GGUF id (with a
        // `code` capability hint) but shares the lazy chat mutex
        // with the primary — first request pays a 5–30s hot-swap.
        // Advertising a stable `coder` alias would invite latency-
        // sensitive mesh traffic onto a cold slot. Revisit when the
        // code slot gets its own residency. NOTE: this means a peer
        // requesting literal "coder" 503s by policy — if that bites,
        // flip this to `true` and wire the advertisement block (the
        // parity test will walk you through it).
        synonyms: &["coder"],
        mesh_advertised: false,
    },
];

/// Alias keys the daemon must RESOLVE for a registered slot role:
/// the bare role + `commonwealth/<role>`, ditto for each synonym.
/// Returns empty for non-canonical roles (`primary_<i>` pool members,
/// `extras:<name>`) — those are routed by their literal key.
pub fn resolution_alias_keys(role: &str) -> Vec<String> {
    let Some(policy) = SLOT_ALIAS_POLICY.iter().find(|p| p.role == role) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    for name in std::iter::once(policy.role).chain(policy.synonyms.iter().copied()) {
        keys.push(name.to_string());
        keys.push(format!("commonwealth/{name}"));
    }
    keys
}

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
    vec![format!("commonwealth/{}", policy.role), policy.role.to_string()]
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
