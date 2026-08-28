// SPDX-License-Identifier: AGPL-3.0-or-later
//! Evidence reds — the compile-fail proofs that `Evidence` has ONE door.
//!
//! Bar `nc-thesis` (noun-convergence): "the product claim is a TYPE, not a
//! runtime gate", proven by "compile-fail tests, one per illegal
//! construction ... Each ships red-first (ARCH §18.1)".
//!
//! Each fixture under `tests/ui/` is compiled as an EXTERNAL crate against
//! `corpus_engine`, which is the only level at which the claim means
//! anything: the invariant is not "these fields are spelled private", it is
//! "a caller in `sovereign` or `commonwealth` cannot make one". A unit test
//! inside this crate could not tell the difference, because inside the crate
//! `pub(crate) fn acquired` is reachable and the fields are visible.
//!
//! # This suite was watched failing, and the failure was not the expected one
//!
//! On its first run all five fixtures reported "expected test case to fail to
//! compile, but it SUCCEEDED" — the exact shape of a green produced by an
//! absence (ARCH §18.3). The cause was that `corpus-engine` itself did not
//! build at that moment (a concurrent edit elsewhere in the crate), so the
//! fixtures were judged against a dependency that never compiled, and nothing
//! in trybuild's output said so. No `.stderr` was recorded at that point;
//! recording then would have minted five tests that pass forever without
//! testing anything.
//!
//! `tests/ui/harness_positive_control.rs` exists because of that hour. It
//! names no corpus-engine type and cannot compile under any feature
//! resolution, so it must always be reported as failing — and if it is ever
//! reported as compiling, this suite is not evaluating anything and says so
//! out loud. Validate the instrument before the result (ARCH §18.4).
//!
//! The recorded `.stderr` files were taken only after the dependency was
//! green AND every fixture failed for a reason nameable in one line:
//! E0451/private-field-construction naming the missing `origin` and `custody`
//! respectively, E0599 for `new`, E0624 for `acquired`, E0616 for assignment.
//!
//! Regenerate after an intentional change to the type's surface:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p corpus-engine --test evidence_reds
//! ```

#[test]
fn evidence_has_exactly_one_door() {
    let t = trybuild::TestCases::new();
    // No origin: the field is private AND absent. A caller who does not know
    // where content came from cannot mint evidence for it.
    t.compile_fail("tests/ui/evidence_without_an_origin.rs");
    // No custody: same, for where it stands for sharing.
    t.compile_fail("tests/ui/evidence_without_a_custody.rs");
    // Fully populated, still refused — a struct literal is a door, and the
    // fields are private, so there is no "but I filled everything in" path.
    t.compile_fail("tests/ui/evidence_by_struct_literal.rs");
    // No public constructor of any spelling.
    t.compile_fail("tests/ui/evidence_has_no_public_constructor.rs");
    // Not a mutable accumulator: `ScoredChunk`'s score is reassigned at
    // twelve production sites in sovereign; an `Evidence`'s cannot be.
    t.compile_fail("tests/ui/evidence_is_not_mutable.rs");
    t.compile_fail("tests/ui/harness_positive_control.rs");
}
