// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the compiled-in mode TOMLs declare.
//!
//! # Where the declarations live, and why these tests read them there
//!
//! The two shipped skill TOMLs (inner-work, recipe-author) are embedded in
//! `sovereign-contracts::skills` — from `src/skills_data/`, inside the
//! crate, since 2026-09-09. Until then they sat in `sovereign/modes/` and
//! these tests reached that directory on disk, which was a boundary
//! violation in both of its spellings: the `include_str!` in contracts
//! (compile-time, flagged by `boundary-gate`) and these very tests
//! (runtime `std::fs`, which rule 3c had already chased out of
//! `sovereign-contracts` on 2026-09-04 — a lifted package builds
//! standalone, and neither spelling resolves without the monorepo's
//! directory shape).
//!
//! Both tests now read the ONE home, `builtin_skill_tomls()` — the same
//! strings every linked binary registers. That is the stronger pin, not
//! a convenience: it asserts what SHIPS rather than what sits in a tree
//! beside the source, so a drift between the two is unrepresentable
//! (there is no second copy to drift).
//!
//! They were NOT repaired by skipping when the data is absent. A check
//! that passes because it could not find its subject is a gate that
//! cannot fail (ARCH §18.1) — and with the data in the crate, absence
//! is a build failure anyway.
//!
//! `sovereign/modes/` still exists, holding what is NOT embedded: the
//! voice-eval case fixtures under `inner-work/tests/cases/`,
//! recipe-author's `examples/`, and workflow-author's unshipped TOML.

use sovereign_contracts::skills::builtin_skill_tomls;
use sovereign_core::skills::{parse_skill_toml, SkillRegister};

/// The parsed compiled-in skill with `id == inner-work`.
///
/// PANICS, by name, when the set does not carry it. A test that reads
/// shipped data has to say what it wanted; failing later on an
/// unexplained `None` is how a registry edit gets misdiagnosed (ARCH
/// §18.3 — absence is reported, never defaulted, and never skipped
/// past).
fn inner_work_skill() -> sovereign_core::skills::Skill {
    builtin_skill_tomls()
        .iter()
        .find_map(|toml| {
            let skill = parse_skill_toml(toml)?;
            (skill.id == "inner-work").then_some(skill)
        })
        .expect("the compiled-in set must carry inner-work")
}

/// After the skill-retirement work, only two TOMLs ship. This test pins
/// their shape so a future edit doesn't accidentally widen inner-work's
/// tool surface or rename recipe-author's required tools without
/// updating the `intent_policy::policy_for` mode arms. Each assertion
/// comes from the principled design, not from an audited count.
#[test]
fn surviving_modes_declare_expected_tool_shape() {
    let inner_work = inner_work_skill();
    assert_eq!(inner_work.id, "inner-work");
    assert_eq!(inner_work.inference.register, SkillRegister::Relational);
    assert!(
        inner_work.tool_config.required.is_empty() && inner_work.tool_config.optional.is_empty(),
        "inner-work declares no tools by design — reflective work \
         is not tool-mediated"
    );

    let recipe_author = builtin_skill_tomls()
        .iter()
        .find_map(|toml| {
            let skill = parse_skill_toml(toml)?;
            (skill.id == "recipe-author").then_some(skill)
        })
        .expect("the compiled-in set must carry recipe-author");
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
