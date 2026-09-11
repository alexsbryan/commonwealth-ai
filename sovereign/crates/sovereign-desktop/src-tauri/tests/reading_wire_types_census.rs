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
//! sv-surface D1 widened this from "no second SPELLING" to "no second
//! PATH". The four commands used to fork on `is_attach_mode()` and keep a
//! full local reader behind the Local arm — nine private DTO helpers over
//! `corpus_engine` — which is a mirror of the ROUTE rather than of its
//! types, and drifts the same way. They are deleted: the commands return
//! the daemon's bytes verbatim in both boot modes, because Local means the
//! daemon is in-process (same `corpus_engine`, same `state_store`, bound on
//! `client_port` before bootstrap returns).
//!
//! Watched to fail: define any struct or enum in `commands/reading.rs` (the
//! old mirrors were `*Dto`, but a mirror can be named anything), or bring
//! back a boot-mode fork or a direct `corpus_engine` read, and this goes red
//! naming the rule. Sabotage-verified at landing (a planted mirror struct,
//! watched red, reverted).

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
fn the_reading_surface_has_one_path_and_no_mirror() {
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
    // D1: one path in both boot modes. The desktop no longer SERIALIZES a
    // reading shape at all — it returns the daemon's response bytes verbatim
    // — so "imports the wire types" is no longer the property to pin. These
    // two are: no boot-mode fork, and no local reader to fork to.
    //
    // CODE lines only. The first cut of these two scanned the whole file and
    // went red on this module's own comment — the one that explains WHY the
    // local reader is gone names `corpus_engine` to do so (§18.1: the failing
    // input was its own documentation). Prose that mentions a needle is
    // documentation; only a real read is a second path. Same calibration
    // `conversation_wire_census`'s URL needle already uses.
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("is_attach_mode"),
        "sv-surface D1: commands/reading.rs forks on the boot mode again. \
         The reading surface is ONE path — `daemon_reading_get` over \
         reading_http's four routes — in both modes; Local means the daemon \
         is in-process, not that there is a second reader."
    );
    assert!(
        !code.contains("corpus_engine"),
        "sv-surface D1: commands/reading.rs reads corpus_engine directly \
         again. That is a mirror of the ROUTE rather than of its types, and \
         drifts the same way the nine deleted *Dto helpers did — the decider \
         is reading_http's handler, reached over client_base_url()."
    );
}
