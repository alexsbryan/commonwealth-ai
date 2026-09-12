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

/// Every Rust file under the desktop's `src-tauri/src`, concatenated.
///
/// Was one file — `state/builtin_skills.rs` — until 2026-09-11, when
/// svt-3b (504c6b6d3) deleted it along with the desktop's `Runtime`
/// commission. A surface that commissions nothing has no skill pass to
/// delegate, so the pin below is on the whole crate: no registration
/// call, no embed, anywhere.
fn desktop_rust_source() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../sovereign-desktop/src-tauri/src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    assert!(
        files.len() >= 20,
        "found only {} .rs files under {} — the walk is broken, not the desktop",
        files.len(),
        root.display()
    );
    files
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())))
        .collect::<Vec<_>>()
        .join("\n")
}

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
    {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
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
fn the_desktop_registers_no_skills_at_all() {
    // Until 2026-09-11 this test pinned the desktop's builtin pass to the
    // shared home. The pass is gone with the commission (svt-3b), so the
    // state to make unrepresentable moved: a surface that quietly grows a
    // skill registry of its own again is the re-fork, and it would start
    // with one of these two spellings.
    let src = desktop_rust_source();
    assert_eq!(
        src.match_indices("register_builtin_skills(").count(),
        0,
        "the desktop commissions no Runtime and so has no skill pass; a \
         registration call here means a second host grew back (svt-3b)."
    );
    let toml_embeds: Vec<&str> = src
        .lines()
        .filter(|l| l.contains("include_str!(") && l.contains(".toml"))
        .collect();
    assert!(
        toml_embeds.is_empty(),
        "no host-local skill embeds — the TOMLs live in exactly one place \
         (sovereign-contracts). This needle fires on the re-fork. (Anchored \
         on `include_str!(` of a `.toml`: the crate legitimately embeds a JS \
         shim and its own sources for a self-census, and the rung-4 census's \
         first draft fired on prose — §18.4.) Found:\n{}",
        toml_embeds.join("\n")
    );
}
