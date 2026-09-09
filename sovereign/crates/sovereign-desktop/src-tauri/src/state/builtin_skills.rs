// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in skills for the desktop bootstrap. Extracted from `state.rs`
//! in the §3.3 decomposition; since sv-surface rung 6 commit B the
//! compiled-in TOMLs live in ONE home — `sovereign_contracts::skills` —
//! shared with the daemon's commission, so a conversation tagged with a
//! builtin skill routes the same agent loop whichever host answers it.
//! The dev overlay and the user skills dir below remain desktop-shape
//! concerns (a settings panel and a `cargo tauri dev` workflow the
//! daemon does not have).

use sovereign_core::SkillRegistry;

pub(super) fn register_builtin_skills(skills: &mut SkillRegistry) {
    sovereign_contracts::skills::register_builtin_skills(skills);
}

/// Debug-only: look up the workspace `modes/` directory so developers
/// running `cargo tauri dev` can add a new mode TOML without needing to
/// rebuild the binary with a new `include_str!` entry. Returns `None`
/// outside the workspace layout (e.g. an installed debug build).
#[cfg(debug_assertions)]
pub(super) fn dev_workspace_skills_dir() -> Option<std::path::PathBuf> {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Some(
        manifest
            .parent()? // crates/sovereign-desktop/
            .parent()? // crates/
            .parent()? // sovereign/
            .join("modes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_skills_all_parse() {
        // include_str! paths are resolved at compile time, but the
        // skill.toml contents must still parse at runtime. Require
        // that EVERY built-in skill parses — a malformed one would
        // silently be skipped at runtime and the user would see a
        // shorter-than-expected Skills list with no explanation.
        //
        // A previous build had 7/8 TOMLs using PascalCase privacy
        // variants that serde (`rename_all = "snake_case"`) rejected;
        // `builtin_skills_all_parse` with a `>= 1` assertion let the
        // bug ship. Keep this strict.
        let mut reg = sovereign_core::SkillRegistry::new();
        register_builtin_skills(&mut reg);
        assert_eq!(
            reg.list().len(),
            sovereign_contracts::skills::builtin_skill_tomls().len(),
            "every built-in skill.toml must parse successfully; \
             registered {} of {} — check logs for the malformed entries",
            reg.list().len(),
            sovereign_contracts::skills::builtin_skill_tomls().len(),
        );
    }

    #[test]
    fn the_desktops_builtin_set_is_the_shared_set() {
        // The drift guard for the shared home: this host's registry ids ARE
        // the contracts set's ids, byte for byte. If someone re-forks the
        // content here (local include_str!s again), the desktop and the
        // daemon can disagree about which agent loops exist — the exact
        // C2 divergence rung 6 exists to close.
        let mut mine = sovereign_core::SkillRegistry::new();
        register_builtin_skills(&mut mine);
        let mut shared = sovereign_core::SkillRegistry::new();
        sovereign_contracts::skills::register_builtin_skills(&mut shared);
        let mine_ids: Vec<String> = mine.list().iter().map(|s| s.id.clone()).collect();
        let shared_ids: Vec<String> = shared.list().iter().map(|s| s.id.clone()).collect();
        assert_eq!(
            crate::state::builtin_skills::sorted(mine_ids),
            crate::state::builtin_skills::sorted(shared_ids),
            "the desktop's built-in skills must be exactly the shared set"
        );
    }

    #[test]
    fn registering_same_skill_twice_does_not_duplicate() {
        // In dev builds, `bootstrap()` first registers built-ins via
        // the shared set and then loads the workspace `modes/` directory
        // as a live overlay. If these two paths register the same skill
        // id, the registry must treat the second as an *override*, not
        // an append. Svelte's `{#each (skill.id)}` crashes on duplicate
        // keys and bails mid-render — users saw "Loading skills…"
        // freeze on screen in browser console `each_key_duplicate`.
        let mut reg = sovereign_core::SkillRegistry::new();
        register_builtin_skills(&mut reg);
        register_builtin_skills(&mut reg); // duplicate pass
        assert_eq!(
            reg.list().len(),
            sovereign_contracts::skills::builtin_skill_tomls().len(),
            "registering the same built-ins twice must not double the count"
        );
        let mut ids: Vec<&str> = reg.list().iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        let before_dedupe = ids.len();
        ids.dedup();
        assert_eq!(
            ids.len(),
            before_dedupe,
            "registry must contain no duplicate ids after double-registration"
        );
    }
}

/// Test-only sorted clone helper (sorted-by-value, not in-place on borrowed).
#[cfg(test)]
pub(super) fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}
