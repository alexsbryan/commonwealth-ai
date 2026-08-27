// SPDX-License-Identifier: AGPL-3.0-or-later
//! The settings panel offers exactly the families Rust knows about.
//!
//! # The state this makes unrepresentable
//!
//! A checkbox with no family behind it, or a family with no checkbox.
//!
//! `ToolFamily` is a closed set (ARCH §2) and `ToolFamily::ALL` is its ONE
//! declaration — the config default and the setup flow both derive from it.
//! The TypeScript list cannot derive from it: there is no codegen across that
//! boundary, so the fourth copy is hand-kept and this census is what keeps it
//! honest.
//!
//! Hand-kept copies of this list have already drifted once: before
//! 2026-08-26 the setup flow omitted `knowledge_lookup`, so a user who
//! completed setup with an empty `enabled_tools` silently lost a tool
//! documented as default-on. That copy is gone; this one cannot be, so it is
//! pinned instead.
//!
//! Watched to fail: add an id to `TOOL_OPTS` with no `ToolFamily` arm, or add
//! an arm with no checkbox, and this goes red naming the offender.

use sovereign_contracts::tool_bundle::ToolFamily;

/// Ids offered by the settings panel, read from `TOOL_OPTS`.
fn ui_offered_ids() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/lib/components/SettingsPanel.svelte");
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    let start = src.find("const TOOL_OPTS = [").expect(
        "SettingsPanel.svelte no longer declares TOOL_OPTS — if the tool \
                 checkboxes moved, point this census at their new home rather than \
                 deleting it",
    );
    let rest = &src[start..];
    let end = rest.find("] as const;").expect(
        "TOOL_OPTS is not terminated by `] as const;` — the scan would \
                 otherwise run past it and pick up unrelated `id:` fields",
    );

    let mut ids = Vec::new();
    for line in rest[..end].lines() {
        if let Some(at) = line.find("id: \"") {
            let after = &line[at + 5..];
            if let Some(close) = after.find('"') {
                ids.push(after[..close].to_string());
            }
        }
    }
    ids
}

#[test]
fn the_settings_panel_offers_exactly_the_families_rust_knows() {
    let ui = ui_offered_ids();
    assert!(
        !ui.is_empty(),
        "the scan found no tool ids — a census that matches nothing would pass \
         vacuously while the two lists drifted freely"
    );

    let rust: Vec<String> = ToolFamily::ALL
        .iter()
        .map(|f| f.wire_id().to_string())
        .collect();

    let missing_checkbox: Vec<&String> = rust.iter().filter(|f| !ui.contains(f)).collect();
    assert!(
        missing_checkbox.is_empty(),
        "a ToolFamily has no checkbox, so a user cannot switch it off and the \
         registry gate will always permit it: {missing_checkbox:?}"
    );

    let orphan_checkbox: Vec<&String> = ui.iter().filter(|i| !rust.contains(i)).collect();
    assert!(
        orphan_checkbox.is_empty(),
        "the panel offers a switch no ToolFamily claims, so unchecking it does \
         nothing at all — `ToolPermissions::from_wire_ids` will report it as an \
         unknown id: {orphan_checkbox:?}"
    );
}
