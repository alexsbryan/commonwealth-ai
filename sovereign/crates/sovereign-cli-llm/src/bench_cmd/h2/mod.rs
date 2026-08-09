// SPDX-License-Identifier: AGPL-3.0-or-later
//! H2 — disagreement as the confabulation detector, offline
//! (`NATIVE_GROUNDING.md` §5 H2, §7.3 H2, and Appendix A).
//!
//! The harness half of H2: the library primitives live in
//! `sovereign_inference::k_sample` (the value decoder) and
//! `sovereign_core::runtime::native_grounding::meaning_cluster`
//! (the clusterer and the two statistics), and this module binds them to a real
//! reranker, real frozen artifacts, and a command line.
//!
//! | Piece | Where |
//! |---|---|
//! | [`rows`] | one typed reader for a scored chaos `*.jsonl`, and the hallucination-label port |
//! | [`pairs`] | the value-equivalence calibration set, built from frozen scored rows |
//! | [`calibrate`] | `svrn bench flywheel h2-calibrate` — fits the clustering floor, commits its curve |
//! | [`smoke`] | `svrn bench flywheel h2-smoke` — the instrument check: does the SAMPLER diverge at all? |
//! | [`gate`] | `svrn bench flywheel h2-gate` — §7.3's verdict on the sampling variant, or its refusal |
//! | [`counterfactual`] | **H2b** — `h2b-arms` / `h2b-gate`, the evidence counterfactual that replaced the sampling axis |
//!
//! **H2 and H2b are one family, not two.** The sampling variant was measured
//! non-viable — 1 distinct value in 5 on every turn at every coherent
//! temperature (`bench/calibration/h2/FINDINGS.md` §2, commit `9900da95`) — and
//! moved to the spec's Appendix A. H2b changes only *what is perturbed*: the
//! evidence rather than the temperature. It reuses this module's decoder, its
//! pinned seeds, its clusterer and its committed floor rather than forking them,
//! which is why it lives here and not beside it.
//!
//! **The reranker adapter is `h4::scorer`, reused, not reimplemented.** H4
//! already resolves the model path, runs the capacity fit gate, refuses by name
//! when the model is absent, and wraps `StandaloneReranker` behind
//! `SentenceScorer`. H2 needs exactly that; a second copy would be principle
//! 8's smell and a second place for the refusal to rot.
//!
//! Everything here reads frozen artifacts. The only commands that load a
//! generator are [`smoke`] and [`counterfactual::arms`], and both draw over
//! evidence read out of frozen artifacts — no new test-bank generation.

pub mod calibrate;
pub mod counterfactual;
pub mod gate;
pub mod pairs;
pub mod rows;
pub mod smoke;
