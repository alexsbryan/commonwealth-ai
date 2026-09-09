// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface rung 6 commit B's family census: the builtin SKILL set and
//! the recipe-authoring TOOL bundle each have ONE home, and every host
//! that commissions a Runtime loads them from there.
//!
//! # The state this makes unrepresentable
//!
//! A daemon whose Runtime carries an EMPTY skill registry (or no
//! recipe-authoring tools) beside a desktop that ships both. That was the
//! measured shape until rung 6 B: `daemon_cmd` passed
//! `SkillRegistry::new()` at the commission site while the desktop
//! embedded two skill TOMLs beside its bootstrap — so a conversation
//! tagged `skill_id = "recipe-author"` routed its agent loop from one
//! host and ran as plain chat from the other. The C2 divergence, on the
//! skill axis, silently.
//!
//! The one homes: `sovereign_contracts::skills::register_builtin_skills`
//! (the compiled-in set) and `sovereign_tools::bundles::
//! RecipeAuthoringTools` (the bundle). This census pins both hosts load
//! them and the retired spellings cannot return.
//!
//! Watched to fail: restore the empty-registry commission, delete the
//! daemon's bundle push, or re-fork the skill TOMLs into a host-local
//! embed — each goes red naming the rule. Sabotage-verified at landing:
//! the empty-registry spelling re-planted, watched red, reverted.

use std::path::Path;

fn daemon_cmd_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon_cmd/mod.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn contracts_skills_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../sovereign-contracts/src/skills.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn desktop_builtin_skills_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sovereign-desktop/src-tauri/src/state/builtin_skills.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_daemon_commissions_the_shared_skill_set_not_an_empty_registry() {
    let src = daemon_cmd_source();
    assert_eq!(
        src.match_indices("sovereign_contracts::skills::register_builtin_skills")
            .count(),
        1,
        "sv-surface rung 6 B: the daemon's commission must load the shared \
         builtin skill set. A conversation tagged with a builtin skill \
         routes its agent loop ONLY when the answering host's registry \
         carries it — the empty-registry commission was the C2 divergence \
         on the skill axis."
    );
    assert_eq!(
        src.match_indices("skills: Arc::new(sovereign_core::SkillRegistry::new()),")
            .count(),
        0,
        "the empty-registry commission spelling is back in daemon_cmd. The \
         registry is built and loaded above the commission now; this arm \
         would silently override it with an empty one."
    );
}

#[test]
fn the_daemon_pushes_the_recipe_authoring_bundle() {
    let src = daemon_cmd_source();
    assert_eq!(
        src.match_indices("bundles::RecipeAuthoringTools::new()")
            .count(),
        1,
        "the daemon's tool list must carry the recipe-authoring bundle — \
         the same one the desktop pushes, so a recipe-author-tagged \
         conversation has its tools whichever host answers."
    );
    assert_eq!(
        src.match_indices("with_features(Arc::clone(fs))").count(),
        1,
        "the bundle must be wired with the features store when one opened"
    );
}

#[test]
fn the_compiled_in_set_is_the_measured_two_and_lives_in_contracts() {
    let src = contracts_skills_source();
    assert_eq!(
        src.match_indices("skills_data/inner-work.toml").count(),
        1,
        "the shared builtin set must embed inner-work — the desktop's \
         measured set, now both hosts'. The TOMLs moved into the crate \
         (src/skills_data/) when boundary-gate flagged the out-of-tree \
         embed on 2026-09-09"
    );
    assert_eq!(
        src.match_indices("skills_data/recipe-author.toml").count(),
        1,
        "the shared builtin set must embed recipe-author"
    );
    assert_eq!(
        src.match_indices("skills_data/workflow-author.toml")
            .count(),
        0,
        "workflow-author's TOML ships in NO registry (its TOOLS ship, via \
         WorkflowAuthoringTools; the file itself still sits unembedded in \
         `sovereign/modes/`). Adding it to the compiled-in set is a \
         both-hosts decision — this pin makes the change a deliberate one, \
         not a drift."
    );
}

#[test]
fn the_desktop_delegates_rather_than_re_embedding() {
    let src = desktop_builtin_skills_source();
    assert!(
        src.contains("sovereign_contracts::skills::register_builtin_skills"),
        "the desktop's builtin pass must call the shared home. A local \
         re-embed is how the two hosts' skill sets fork again."
    );
    assert_eq!(
        src.match_indices("include_str!(\"").count(),
        0,
        "no host-local skill embeds — the TOMLs live in exactly one place \
         (sovereign-contracts). This needle fires on the re-fork. (Anchored \
         on the call shape `include_str!(\"`, not the bare word: the file's \
         own comments say the word, and the rung-4 census's first draft \
         fired on prose — §18.4.)"
    );
}
