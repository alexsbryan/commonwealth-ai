// SPDX-License-Identifier: AGPL-3.0-or-later
//! Judgement reds — the compile-fail proofs that a verdict cannot be reported
//! without a reason, and that a reason cannot be a placeholder.
//!
//! The invariant is not "these fields are spelled private". It is: a caller in
//! `sovereign`, `corpus-engine` or `commonwealth` — the three domains this
//! crate sits beneath — cannot construct a `Judgement` that says something
//! failed while saying nothing about why. Each fixture under `tests/ui/` is
//! compiled as an EXTERNAL crate against `kernel_types`, which is the only
//! level at which that claim means anything: inside the crate the fields are
//! visible and a unit test could not tell the difference.
//!
//! `tests/ui/harness_positive_control.rs` names no kernel type and cannot
//! compile under any feature resolution, so it must always be reported as
//! failing. If it is ever reported as compiling, this suite is judging
//! nothing (ARCH §18.4). The sibling suite `corpus-engine/tests/evidence_reds.rs`
//! records the hour that control was written for: five fixtures once reported
//! "expected to fail, but SUCCEEDED" because the dependency crate itself did
//! not build, and nothing in trybuild's output said so.
//!
//! Regenerate after an intentional change to the type's surface:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p kernel-types --test judgement_reds
//! ```

#[test]
fn a_verdict_cannot_be_reported_without_a_reason() {
    let t = trybuild::TestCases::new();
    // A struct literal is a door, and the fields are private.
    t.compile_fail("tests/ui/judgement_by_struct_literal.rs");
    // The reason is a positional argument, not an Option: omitting it is an
    // arity error, not a defaulted empty string.
    t.compile_fail("tests/ui/judgement_failed_without_a_reason.rs");
    // A `&str` is not a `Reason`, so the placeholder refusal in
    // `Reason::new` / `Reason::literal` cannot be routed around.
    t.compile_fail("tests/ui/judgement_never_ran_with_a_bare_string.rs");
    // Nor can it be routed around by constructing the newtype directly.
    t.compile_fail("tests/ui/reason_by_tuple_struct.rs");
    // Not a mutable accumulator: the verdict cannot be reassigned once made.
    t.compile_fail("tests/ui/judgement_verdict_is_not_assignable.rs");
    t.compile_fail("tests/ui/harness_positive_control.rs");
}
