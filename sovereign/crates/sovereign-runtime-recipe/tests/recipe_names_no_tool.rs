// SPDX-License-Identifier: AGPL-3.0-or-later
//! Falsifier for `quality/TOPOLOGY.md` §10 phase 7b.
//!
//! # The state this makes unrepresentable
//!
//! A host that adopts the shared recipe and thereby LOSES a tool it had.
//!
//! That was not hypothetical, and it is why the phase stalled: measured
//! 2026-08-25, `sovereign-server` registered 31 tools by type name and this
//! recipe 11, and the sets were not nested in either direction. Adoption meant
//! deleting ~20 tools from the hub — code intel, notes, recipe authoring — so
//! the phase's remaining work read as a security question ("which twenty
//! belong to every host?") when the actual defect was structural: **the shared
//! recipe owned the list**, so no host could add a family without editing a
//! file every other host shares.
//!
//! The seam is `sovereign_contracts::tool_bundle`. A host composes
//! `Vec<Box<dyn ToolBundle>>`; the recipe folds it. The property that keeps it
//! honest is the one asserted here — **the recipe names no tool** — because a
//! single re-introduced `register(Box::new(SomeTool::new(..)))` silently
//! restores the coupling and nothing else would notice.
//!
//! Watched to fail: adding `tools.register(Box::new(shell::ShellTool))` back
//! into `build_tools` fails this test.

use std::path::Path;

/// Registration calls the recipe is allowed to make. `install` is the fold
/// over the host's bundles; MCP is named as a non-bundle in `build_tools` and
/// registers by protocol rather than by type name.
const ALLOWED: &[&str] = &["tool_bundle::install", "load_from_setup_config"];

fn recipe_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Strip doc comments and line comments: this census is about CODE, and the
/// module docs legitimately discuss tools by name.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("//") || t.starts_with("*") || t.starts_with("/*"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_recipe_registers_no_tool_by_name() {
    let code = code_only(&recipe_source());

    let offenders: Vec<(usize, String)> = code
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(".register(") || l.contains(".register_arc("))
        .map(|(i, l)| (i + 1, l.trim().to_string()))
        .collect();

    assert!(
        offenders.is_empty(),
        "the shared recipe registered a tool by name — that is the coupling \
         phase 7b removed, and it means adopting this recipe can again cost a \
         host a capability. Give the tool a `ToolBundle` in the crate that \
         owns it and let the host compose it.\n{offenders:#?}"
    );
}

#[test]
fn the_recipe_still_folds_the_host_bundles() {
    // The other half of the bar. A recipe that registers nothing AND folds
    // nothing would pass the test above while wiring an empty registry, which
    // is why "zero registrations" alone is not the property (ARCH §18.1 — a
    // check with no failing input you can name).
    let code = code_only(&recipe_source());
    for needle in ALLOWED {
        assert!(
            code.contains(needle),
            "the recipe no longer calls `{needle}` — with the fold gone, every \
             host's tool bundles are silently dropped and every turn runs with \
             an empty registry"
        );
    }
}

#[test]
fn shell_is_not_a_recipe_concern_any_more() {
    // `ShellAccess` was deleted by this phase: it could express exactly ONE
    // policy fork while every other family stayed hardcoded. Its replacement
    // is `Withheld`, which any family can use. Re-introducing the enum would
    // mean the second mechanism is back (ARCH §10.6).
    let code = code_only(&recipe_source());
    assert!(
        !code.contains("ShellAccess"),
        "`ShellAccess` is back in the recipe. A withheld family is \
         `sovereign_contracts::tool_bundle::Withheld`, which works for every \
         family rather than for shell alone."
    );
}
