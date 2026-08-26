// SPDX-License-Identifier: AGPL-3.0-or-later
//! Answer reds — the compile-fail proofs that a released turn cannot be
//! assembled without the things that make it trustworthy.
//!
//! Bar `nc-thesis` (noun-convergence): *"the product claim is a TYPE, not a
//! runtime gate"*, proven by *"compile-fail tests, one per illegal
//! construction … Each ships red-first (ARCH §18.1)"*. Rung `nc-11-answer`
//! closes three of the bar's five declared rows and each has a fixture here:
//!
//! | declared construction | fixture |
//! |---|---|
//! | an `Answer` with no `Judgement` | `answer_without_a_judgement` |
//! | a `Citation` not pointing into a sealed `EvidenceSet` | `citation_without_a_seal` |
//! | a non-shareable `Evidence` in a peer-bound reply | `non_shareable_evidence_in_a_peer_reply` |
//!
//! Each fixture under `tests/ui/` is compiled as an EXTERNAL crate against
//! `kernel_types`, which is the only level at which the claim means anything:
//! the invariant is not "these fields are spelled private", it is "a caller in
//! `sovereign`, `corpus-engine` or `commonwealth` cannot write this". A unit
//! test inside this crate could not tell the difference, because inside the
//! crate the fields are visible.
//!
//! # What a compile-fail can and cannot prove here
//!
//! It proves there is **no door**. It does not prove what the door does when
//! you walk through it — that a `Personal` citation is refused at a
//! `public-web` floor, that a summary may not be quoted, that a roll-up over
//! nothing is a could-not-judge. Those are runtime facts and they are pinned
//! by the unit tests in `src/answer.rs`. The two halves are not
//! interchangeable and neither is sufficient: a door that cannot be skipped
//! but decides wrongly is as bad as no door. Both are in the definition-of-
//! done sweep.
//!
//! # The positive control gates the reading (ARCH §18.4)
//!
//! `tests/ui/harness_positive_control.rs` names no kernel type and cannot
//! compile under any feature resolution, so a working harness must always
//! report it failing. It is wired into BOTH suites in this crate deliberately:
//! `judgement_reds`'s control validates `judgement_reds`, and this suite needs
//! its own or it is an uncontrolled instrument that happens to sit next to a
//! controlled one. The hour that earned this rule is recorded in
//! `corpus-engine/tests/evidence_reds.rs`, where five fixtures reported
//! "expected to fail, but SUCCEEDED" because the dependency crate itself did
//! not build and nothing in trybuild's output said so.
//!
//! Regenerate after an intentional change to the surface:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p kernel-types --test answer_reds
//! ```

#[test]
fn a_released_turn_cannot_skip_what_makes_it_trustworthy() {
    let t = trybuild::TestCases::new();

    // ── The three declared rows ──────────────────────────────────────────
    // No judgement: the fields are private AND the field is absent, and the
    // release door takes the judgements positionally rather than as an
    // Option, so omitting them is an arity error and not an empty verdict.
    t.compile_fail("tests/ui/answer_without_a_judgement.rs");
    // No seal: `pointing_into` is the only door and it takes one, so a
    // citation cannot be minted from a bare string plus an origin it never
    // came out of.
    t.compile_fail("tests/ui/citation_without_a_seal.rs");
    // No custody sweep: `PeerAnswer` is what the mesh path accepts and its
    // only door returns a Result, so neither the sweep nor its refusal can be
    // dropped.
    t.compile_fail("tests/ui/non_shareable_evidence_in_a_peer_reply.rs");

    // ── Beyond the declared list ─────────────────────────────────────────
    // Real proof, and deliberately not counted by `scripts/nc-thesis.py`:
    // that instrument scores the DECLARED list, and letting extra fixtures
    // inflate it would let the bar be moved by adding tests next to the
    // claims already proven rather than by proving the open ones. Same
    // treatment `nc-4-evidence`'s three extra Evidence reds received.
    //
    // No public constructor of any other spelling. Split from
    // `citation_without_a_seal` because rustc suppresses that file's
    // private-field diagnostic when method resolution in the same body has
    // already failed — one fixture would have recorded one error and hidden
    // the other.
    t.compile_fail("tests/ui/citation_has_no_public_constructor.rs");
    // A draft's text cannot be read — "no surface returns a pre-release
    // draft" as construction.
    t.compile_fail("tests/ui/draft_text_is_not_readable.rs");
    // Deserialize is a constructor, so it is not derived on any of these.
    t.compile_fail("tests/ui/answer_by_deserialize.rs");

    t.compile_fail("tests/ui/harness_positive_control.rs");
}
