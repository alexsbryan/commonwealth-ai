//! Built-in skills shipped with the desktop binary. Extracted from
//! `state.rs` in the §3.3 decomposition — the `skill.toml` contents are
//! embedded at compile time so the surviving modes are available on
//! every install regardless of filesystem layout.

use sovereign_core::SkillRegistry;

/// Skills shipped with the binary. Each entry is the raw `skill.toml`
/// contents embedded at compile time via `include_str!`. This keeps
/// the surviving modes available on every fresh install regardless
/// of filesystem layout, and survives Tauri bundle repackaging
/// without needing `bundle.resources` plumbing.
///
/// After the skills-as-menu retirement, only two modes survive:
///   - inner-work — reflective surface (relational register, local-only)
///   - recipe-author — workspace surface (bespoke tool set)
///
/// The other seven entries were retired because they were intent-
/// shape variants masquerading as user-selected skills. Intent-keyed
/// policy in `sovereign_core::intent_policy` now drives the
/// default-chat behavior they used to provide.
///
/// User-created skills (custom workflows on disk) still load from
/// `config.skills_dir` alongside these two bundled modes.
///
/// NOTE: these `include_str!` paths are relative to this file
/// (`src/state/builtin_skills.rs`), one directory deeper than the
/// former `src/state.rs` — hence the extra `../` versus the original.
const BUILTIN_SKILLS: &[&str] = &[
    include_str!("../../../../../modes/inner-work/skill.toml"),
    include_str!("../../../../../modes/recipe-author/skill.toml"),
];

pub(super) fn register_builtin_skills(skills: &mut SkillRegistry) {
    for (idx, toml) in BUILTIN_SKILLS.iter().enumerate() {
        match sovereign_core::skills::parse_skill_toml(toml) {
            Some(skill) => skills.register(skill),
            None => tracing::warn!(
                idx,
                "built-in skill #{idx}: failed to parse skill.toml — skipping"
            ),
        }
    }
}

/// Debug-only: look up the workspace `modes/` directory so developers
/// running `cargo tauri dev` can add a new mode TOML without needing
/// to rebuild the binary with a new `include_str!` entry. Returns
/// `None` outside the workspace layout (e.g. an installed debug build).
#[cfg(debug_assertions)]
pub(super) fn dev_workspace_skills_dir() -> Option<std::path::PathBuf> {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Some(
        manifest
            .parent()? // crates/sovereign-desktop/
            .parent()? // crates/
            .parent()? // <repo root>
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
            BUILTIN_SKILLS.len(),
            "every built-in skill.toml must parse successfully; \
             registered {} of {} — check logs for the malformed entries",
            reg.list().len(),
            BUILTIN_SKILLS.len(),
        );
    }

    #[test]
    fn registering_same_skill_twice_does_not_duplicate() {
        // In dev builds, `bootstrap()` first registers built-ins via
        // `include_str!` and then loads the workspace `skills/` directory
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
            BUILTIN_SKILLS.len(),
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
