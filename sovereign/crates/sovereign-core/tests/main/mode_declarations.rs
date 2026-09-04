// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the checked-in mode TOMLs under `sovereign/modes/` declare.
//!
//! # Why this is not in `sovereign-contracts`, where the parser lives
//!
//! It was, until 2026-09-04: `skills.rs`'s own `#[cfg(test)] mod tests` read
//! `CARGO_MANIFEST_DIR/../../modes` at RUNTIME. `sovereign-contracts` is a
//! `[[package_leaf]]` (`quality/ARCH_LAYERS.toml`), so every declared package
//! carries it and each must build standalone WITH ITS TESTS — and `modes/` is
//! not part of the crate. A lifted `sovereign-contracts` has no
//! `sovereign/modes/` at all, so both tests panicked on the read. Same class,
//! same week, as the two generators that left `kernel-types` in `c1116b31e`,
//! and `boundary-gate` could see neither: it greps for `build.rs` and
//! `include_str!`, and this is a runtime `std::fs` reach-out. Rule 3c, added
//! with this move, sees it.
//!
//! They were NOT repaired by skipping when `modes/` is absent. A check that
//! passes because it could not find its subject is a gate that cannot fail
//! (ARCH §18.1).
//!
//! `sovereign-core` is in no package, so no lift carries it — and it is
//! already the crate that reads these same files from this same place, in
//! `voice_prompt_shape.rs`'s end-to-end pin one module over. That pin now
//! shares the [`modes_dir`] below rather than carrying a second copy of the
//! hop (ARCH §10.6).
//!
//! `inner_work_mode_parses_with_relational_register` did not travel with them.
//! Its two assertions — `skill.id == "inner-work"` and the register is
//! `Relational` — are both made below, on the same file, in the same read. It
//! was a second decider for a fact this test already states.

use std::path::PathBuf;

use sovereign_core::skills::{parse_skill_toml, SkillRegister};

/// `sovereign/modes/` — the checked-in mode declarations, two directories up
/// from this crate.
///
/// PANICS, by name, when the directory is not where it should be. A test that
/// reads repo data has to say which path it wanted; failing later on an
/// unexplained `ENOENT` is how a relayout gets misdiagnosed (ARCH §18.3 —
/// absence is reported, never defaulted, and never skipped past).
pub fn modes_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the sovereign-core manifest has no grandparent")
        .join("modes");
    assert!(
        dir.join("inner-work").join("skill.toml").is_file(),
        "modes/ did not resolve: {} holds no inner-work/skill.toml. \
         These tests pin the checked-in mode declarations and cannot run without them.",
        dir.display()
    );
    dir
}

/// After the skill-retirement work, only two TOMLs live under
/// `sovereign/modes/`. This test pins their shape so a future edit doesn't
/// accidentally widen inner-work's tool surface or rename recipe-author's
/// required tools without updating the `intent_policy::policy_for` mode arms.
/// Each assertion comes from the principled design, not from an audited count.
#[test]
fn surviving_modes_declare_expected_tool_shape() {
    let modes_dir = modes_dir();

    let inner_work_toml = std::fs::read_to_string(modes_dir.join("inner-work/skill.toml"))
        .expect("read modes/inner-work/skill.toml");
    let inner_work = parse_skill_toml(&inner_work_toml).expect("parse modes/inner-work/skill.toml");
    assert_eq!(inner_work.id, "inner-work");
    assert_eq!(inner_work.inference.register, SkillRegister::Relational);
    assert!(
        inner_work.tool_config.required.is_empty() && inner_work.tool_config.optional.is_empty(),
        "inner-work declares no tools by design — reflective work \
         is not tool-mediated"
    );

    let recipe_author_toml = std::fs::read_to_string(modes_dir.join("recipe-author/skill.toml"))
        .expect("read modes/recipe-author/skill.toml");
    let recipe_author =
        parse_skill_toml(&recipe_author_toml).expect("parse modes/recipe-author/skill.toml");
    assert_eq!(recipe_author.id, "recipe-author");
    // Spot-check the must-have recipe tools (matches the
    // intent_policy::recipe_author_tools() table).
    let required: std::collections::HashSet<&str> = recipe_author
        .tool_config
        .required
        .iter()
        .map(String::as_str)
        .collect();
    for needed in ["recipe_validate", "recipe_test", "decision_log"] {
        assert!(
            required.contains(needed),
            "recipe-author must require '{needed}' (intent_policy table depends on it)"
        );
    }
}
