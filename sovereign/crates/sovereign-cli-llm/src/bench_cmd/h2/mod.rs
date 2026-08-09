// SPDX-License-Identifier: AGPL-3.0-or-later
//! H2 — semantic entropy, offline (`NATIVE_GROUNDING.md` §5 H2, §7.3 H2).
//!
//! The harness half of H2: the library primitives live in
//! `sovereign_inference::k_sample` (the k-sample value decoder) and
//! `sovereign_core::runtime::native_grounding::meaning_cluster`
//! (the clusterer and the two statistics), and this module binds them to a real
//! reranker, real frozen artifacts, and a command line.
//!
//! | Piece | Where |
//! |---|---|
//! | [`pairs`] | the value-equivalence calibration set, built from frozen scored rows |
//! | [`calibrate`] | `svrn bench flywheel h2-calibrate` — fits the clustering floor, commits its curve |
//! | [`smoke`] | `svrn bench flywheel h2-smoke` — the instrument check: does the sampler diverge at all? |
//! | [`gate`] | `svrn bench flywheel h2-gate` — §7.3's verdict, or its refusal |
//!
//! **The reranker adapter is `h4::scorer`, reused, not reimplemented.** H4
//! already resolves the model path, runs the capacity fit gate, refuses by name
//! when the model is absent, and wraps `StandaloneReranker` behind
//! `SentenceScorer`. H2 needs exactly that; a second copy would be principle
//! 8's smell and a second place for the refusal to rot.
//!
//! Everything here reads frozen artifacts. The only command that loads a
//! generator is [`smoke`], and it draws over evidence read out of a frozen
//! transcript — no new test-bank generation.

pub mod calibrate;
pub mod gate;
pub mod pairs;
pub mod smoke;
