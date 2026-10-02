// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rubric apparatus — per-criterion binary judging, shared.
//!
//! Four instruments were proven by the moral-reasoning lane and are
//! generic; moral reasoning was only their first tenant
//! (`SITUATED_FLYWHEEL.md` §"The claim"):
//!
//! 1. **Per-criterion binary judging with signed weights and evidence
//!    quotes** ([`judge`]) — grades the reasoning path, not just the
//!    outcome, so a lucky ungrounded answer can score below a
//!    well-grounded abstention.
//! 2. **The judge calibration gate** ([`judge::run_calibration`]) —
//!    hand-labeled items, sens/spec floors, could-not-judge
//!    first-class. Calibration is per criterion FAMILY: a judge
//!    certified on moral criteria is not certified on situatedness
//!    criteria.
//! 3. **Wilson-CI reporting with a disjoint-CI diff** ([`score`],
//!    [`report`]) — a delta counts only when the two intervals are
//!    disjoint. The gate refusing to separate two same-class models
//!    is the evidence that it is honest. That rule treats the arms as
//!    INDEPENDENT samples; when they ran the same bank they are not,
//!    so [`paired`] prints an exact McNemar over per-criterion flips
//!    beneath every diff. Both readings, always — see [`paired`] for
//!    why printing only the first one over-reads a dirty effect.
//! 4. **The deterministic bank format** — owned per lane (each lane
//!    has its own criteria vocabulary), but scored through here.
//!
//! A lane binds to this by (a) loading its own bank, (b) producing
//! [`score::CriterionOutcome`]s, and (c) implementing
//! [`score::RubricItem`] / [`report::RubricRun`] over its own report
//! structs. It never re-implements a formula, a threshold, or the
//! significance rule — ARCH_PRINCIPLES §10.6.
//!
//! Relocation note: when P5 moves the calibrated judge into the turn
//! loop, [`judge`] is the module that migrates to `sovereign-core`.
//! It is kept free of lane-specific and CLI-specific concerns so that
//! move stays mechanical.

pub mod judge;
pub mod paired;
pub mod report;
pub mod score;
#[cfg(test)]
pub mod test_support;
