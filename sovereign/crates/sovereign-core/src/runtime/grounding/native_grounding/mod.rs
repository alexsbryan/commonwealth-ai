// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native grounding — the decode-rooted stack that replaces the judge
//! ladder, landing one stage at a time behind `SOVEREIGN_NATIVE_GROUNDING`.
//!
//! Spec: `sovereign/docs/specs/NATIVE_GROUNDING.md` (skunkworks). The
//! architectural contract the operator pinned for this integration:
//! **each stage does ONE job and hands TYPED, clean output to the next**,
//! so downstream stages stop re-checking upstream work.
//!
//! What is here today, and what is deliberately not:
//!
//! | Stage | Status |
//! |---|---|
//! | H1 admission ([`admission`]) | landed, dark behind the flag |
//! | H2b evidence-assembly checker | **not built, and will not be** — measured +0.0010 over the margin it would sit next to; the margin already carries the applicability signal (`bench/calibration/h2b/FINDINGS.md`) |
//! | H4 verification | **not built** — 0.7674 vs a 0.7955 naive ceiling; the incumbent judge ladder keeps per-claim verification, and span resolution is DISPLAY only |
//!
//! The flag is read in exactly one place — [`admission::native_grounding_enabled`]
//! — so "is the native path on?" has one answer and one log line
//! (ARCH §10.6, one decider one name).

pub(crate) mod admission;
pub mod span_resolver;
