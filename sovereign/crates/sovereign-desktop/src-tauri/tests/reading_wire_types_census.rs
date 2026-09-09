// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface rung 4's reading family census: the reading surface's
//! response shapes ARE `sovereign_mesh::reading_http`'s wire types — the
//! hand-kept `*Dto` mirror set (44 refs, "byte-compatible by comment") is
//! deleted and cannot come back.
//!
//! # The state this makes unrepresentable
//!
//! A second spelling of a reading wire shape in the desktop. The mirrors
//! were hand-copied to keep in-process JSON byte-compatible with the
//! daemon's, and they had ALREADY drifted when this census was minted:
//! they lacked the wire types' `skip_serializing_if` attributes, so an
//! in-process chunk emitted `"title": null` where the attach path's
//! verbatim daemon JSON omitted the key — the exact byte-compat break
//! the mirrors existed to prevent. Hand-kept compatibility is not
//! compatibility; importing the one type is.
//!
//! Watched to fail: define any struct or enum in `commands/reading.rs`
//! (the old mirrors were `*Dto`, but a mirror can be named anything) and
//! this goes red naming the rule. Sabotage-verified at landing
//! (a planted mirror struct, watched red, reverted).

use std::path::Path;

fn reading_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/reading.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A struct/enum *definition* starts a line (after optional whitespace and
/// a visibility prefix). English prose — "construct and serialize",
/// "reconstruction" — cannot trip this; only a real definition does.
fn is_definition(line: &str) -> bool {
    let mut rest = line.trim_start();
    for vis in ["pub(crate) ", "pub(super) ", "pub "] {
        if let Some(stripped) = rest.strip_prefix(vis) {
            rest = stripped;
            break;
        }
    }
    rest.starts_with("struct ") || rest.starts_with("enum ")
}

#[test]
fn the_reading_surface_serializes_the_wire_types_not_a_mirror() {
    let src = reading_source();
    let definitions: Vec<&str> = src.lines().filter(|l| is_definition(l)).collect();
    assert!(
        definitions.is_empty(),
        "sv-surface rung 4: a struct/enum is defined in commands/reading.rs again \
         ({definitions:?}). The reading surface's response shapes ARE \
         sovereign_mesh::reading_http's wire types — a local type (mirror \
         or otherwise) is a second spelling of a wire shape, and the last \
         one had already drifted when it was deleted."
    );
    assert!(
        src.contains("use sovereign_mesh::reading_http::"),
        "the reading surface must import the wire types it serializes"
    );
}
