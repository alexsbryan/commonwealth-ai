// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atoms reds — the compile-fail proof that `atoms.json` has ONE door.
//!
//! Bar `dm-atom-outside` (domains campaign): *"No consumer outside
//! enrichment/ declares its own Atom"*, made STRUCTURAL by `domains-8`'s
//! check 10, corrected in `quality/DOMAINS.md` §10.5 "The correction":
//! `pub(crate)` on `AtomsFile.atoms` does NOT close the door, because the
//! derived `Deserialize` stays public and `serde_json::from_str::<AtomsFile>`
//! still works outside vocab. The structural form is the `Evidence` pattern —
//! a private wire twin that deserialises, `AtomsFile` itself not
//! `Deserialize`, and `vocab::read` the only constructor.
//!
//! # The fixtures assert the SEALED door
//!
//! `dm-vocab-compile-fail-test` added this suite BEFORE the door was sealed, and
//! O8 step 2 said to *"watch it PASS today (i.e. the illegal construction
//! compiles) — that is the red"*. The two fixtures then built an `AtomsFile`
//! outside `vocab::read` and compiled, because `AtomsFile` derived `Deserialize`
//! (`atoms.rs:1527`) and both its fields were `pub` (`atoms.rs:1529-1530`).
//!
//! `REVIEW-build-vocab-seal` sealed it: `AtomsFile` no longer derives
//! `Deserialize` (the crate-private `AtomsFileWire` carries it) and the `atoms`
//! field is `pub(crate)`, so the fixtures no longer compile. The suite is green
//! for the opposite reason — the illegal constructions are refused — which is
//! what `scripts/nc-thesis.py` calls "the reds are still red". The `.stderr`
//! files were recorded from the SEALED type, never against the open door (ARCH
//! principle 5).
//!
//! # The positive control gates the reading
//!
//! `tests/ui/harness_positive_control.rs` names no vocab type and cannot
//! compile under any feature resolution, so a working harness must always
//! report it failing. If it is ever reported as compiling, this suite is not
//! evaluating fixtures at all and every other verdict in it is worthless —
//! the hour recorded in `corpus-engine/tests/evidence_reds.rs` (2026-08-20),
//! when five fixtures reported "expected to fail, but SUCCEEDED" because the
//! dependency crate itself did not build and nothing in trybuild's output
//! said so. Validate the instrument before the result (ARCH principle 7).
//!
//! Regenerate the `.stderr` files from the sealed type:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p corpus-engine-vocab --test atoms_reds
//! ```

#[test]
fn atoms_file_has_exactly_one_door() {
    let t = trybuild::TestCases::new();

    // The `Deserialize` door: SEALED by `REVIEW-build-vocab-seal`. `AtomsFile`
    // no longer derives `Deserialize`; the crate-private `AtomsFileWire`
    // carries it and `read_atlas_atoms` is the only parser. This fixture no
    // longer compiles — the red the seal was watched at.
    t.compile_fail("tests/ui/atoms_by_deserialize.rs");
    // The struct-literal door: SEALED. `atoms` is `pub(crate)`, so a caller
    // outside vocab cannot fill the struct in. This fixture no longer compiles.
    t.compile_fail("tests/ui/atoms_by_struct_literal.rs");

    // Always a compile failure, before and after the seal.
    t.compile_fail("tests/ui/harness_positive_control.rs");
}
