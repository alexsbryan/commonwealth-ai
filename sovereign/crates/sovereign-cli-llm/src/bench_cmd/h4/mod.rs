// SPDX-License-Identifier: AGPL-3.0-or-later
//! H4 — mechanical attribution, offline (`NATIVE_GROUNDING.md` §5 H4, §7.3 H4).
//!
//! The harness half of H4: the library primitives live in
//! `sovereign_core::runtime::native_grounding` (span resolver, sentence sweep)
//! and this module binds them to a real reranker, a real frozen transcript, and
//! a command line.
//!
//! | Piece | Where |
//! |---|---|
//! | [`transcript`] | one typed reader for a chaos `*.transcripts.jsonl`, shared by every H4 subcommand |
//! | [`scorer`] | the `StandaloneReranker` adapter, its capacity gate, and the refusal when the model is absent |
//! | [`sweep`] | `svrn bench flywheel h4-sweep` — margins per sentence |
//! | [`gate`] | `svrn bench flywheel h4-gate` — §7.3's calibrate-then-score verdict |
//!
//! Everything here reads frozen artifacts. Nothing in this module invokes a
//! judge, a Critic, or the production grounding gate; the incumbent stack is
//! untouched by construction, and H4's agreement measurement replays the
//! verdicts that stack already froze.

pub mod gate;
pub mod scorer;
pub mod sweep;
pub mod transcript;
