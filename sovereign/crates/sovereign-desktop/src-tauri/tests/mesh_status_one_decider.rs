// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface rung 4's mesh-status family census: the member-status
//! STRING set has one decider — `MemberStatus`'s own serde repr
//! (`rename_all = "lowercase"` in sovereign-mesh types). The desktop's
//! hand `match m.status.as_str()` (string → enum, `_ => Offline`) was
//! the twin, and its catch-all made it worse than a twin: a daemon
//! newer than the desktop (a new status variant) silently rendered
//! its members as OFFLINE instead of surfacing the unknown string.
//!
//! # The state this makes unrepresentable
//!
//! A second spelling of the status-string set in the desktop. The
//! wire parse goes through `MemberStatus::deserialize` — the same
//! derive that serializes the local-mode enum — so a new variant on
//! the daemon either parses or refuses loudly, never substitutes.
//!
//! Watched to fail: re-introduce a hand `"online" =>` (or sibling)
//! match arm in mesh_commands.rs and this goes red naming the rule.
//! Sabotage-verified at landing (a planted match, watched red,
//! reverted).
//!
//! Note: the DAEMON's own hand stringify (`mesh_http.rs`, enum →
//! string beside this) is the other half of the retired pair — it
//! lives in sovereign-mesh, is byte-identical to the serde repr, and
//! is recorded on the campaign row as the cw-lift coordination item.

use std::path::Path;

/// The whole `mesh_commands` module: the parent file plus its child files
/// (the wire parse lives in `mesh_commands/state_response.rs` since e85076537).
fn mesh_commands_source() -> String {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let read = |p: &Path| {
        std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
    };
    let mut out = read(&src.join("mesh_commands.rs"));
    let dir = src.join("mesh_commands");
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let p = entry.expect("dir entry").path();
        if p.extension().is_some_and(|x| x == "rs") {
            out.push_str(&read(&p));
        }
    }
    out
}

#[test]
fn member_status_strings_have_one_decider_the_enums_serde_repr() {
    let src = mesh_commands_source();
    // The hand-match arm spellings — each one names the failing input
    // it forbids (the lesson of rung 2's census: a check that does not
    // name the retreat's spelling sails it through green).
    for needle in [
        "\"online\" =>",
        "\"busy\" =>",
        "\"away\" =>",
        "\"offline\" =>",
    ] {
        assert!(
            !src.contains(needle),
            "sv-surface rung 4: a hand member-status match arm is back in \
             mesh_commands.rs (matched `{needle}`). The one decider for the \
             status-string set is MemberStatus's own serde repr — parse via \
             MemberStatus::deserialize, which refuses an unknown variant \
             instead of silently rendering the member offline (§18.3)."
        );
    }
    assert!(
        src.contains("MemberStatus::deserialize"),
        "the wire member-status parse must go through the enum's own serde repr"
    );
}
