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
//! # The fixtures assert the OPEN door today (O8 step 2)
//!
//! `dm-vocab-compile-fail-test` adds this suite BEFORE the door is sealed, and
//! O8 step 2 says to *"watch it PASS today (i.e. the illegal construction
//! compiles) — that is the red"*. The two fixtures below build an `AtomsFile`
//! outside `vocab::read` and they compile, because `AtomsFile` derives
//! `Deserialize` (`atoms.rs:1527`) and both its fields are `pub`
//! (`atoms.rs:1529-1530`). `t.pass` records exactly that: the suite is green
//! today, and the green is the red — a green produced by an open door.
//!
//! `REVIEW-build-vocab-seal` flips both to `t.compile_fail` in the commit that
//! gives `AtomsFile` the private wire twin and removes the derive. The
//! `.stderr` files are recorded THEN, from the sealed type, never before: a
//! `.stderr` recorded against an open door would pin the wrong error, and a
//! gate you have not watched fail is not a gate (ARCH principle 5). After the
//! flip the suite stays green for the opposite reason — the fixtures no longer
//! compile, which is what `scripts/nc-thesis.py` calls "the reds are still
//! red".
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
//! Regenerate after `REVIEW-build-vocab-seal` seals the type:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p corpus-engine-vocab --test atoms_reds
//! ```

#[test]
fn atoms_file_has_exactly_one_door() {
    let t = trybuild::TestCases::new();

    // The `Deserialize` door: `AtomsFile` derives it at `atoms.rs:1527`, so
    // any caller outside vocab can mint one from a string. Compiles today;
    // `REVIEW-build-vocab-seal` removes the derive and flips this line.
    t.pass("tests/ui/atoms_by_deserialize.rs");
    // The struct-literal door: both fields are `pub` at `atoms.rs:1529-1530`,
    // so a caller who fills everything in is not refused. Compiles today;
    // the seal privatises the fields (the private wire twin) and flips this.
    t.pass("tests/ui/atoms_by_struct_literal.rs");

    // Always a compile failure, before and after the seal.
    t.compile_fail("tests/ui/harness_positive_control.rs");
}
