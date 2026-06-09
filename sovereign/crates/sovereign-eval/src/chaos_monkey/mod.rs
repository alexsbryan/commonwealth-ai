// SPDX-License-Identifier: AGPL-3.0-or-later
//! Chaos-Monkey: **grounded calibration under adversarial pressure.**
//!
//! Every other bench in the suite measures *competence when the deck is
//! stacked in the model's favour* — it only asks questions the corpus can
//! answer. This one measures *calibration when it isn't*: the situated-agent
//! property that the system must answer capably **with provenance when it
//! has the facts in persistence**, and have the **humility to say what it
//! doesn't know** when it doesn't — without being fooled by plausible
//! distractors.
//!
//! It is hard but **fair**: a sealed, known corpus; a question bank whose
//! every "absent" item is certified to have no supporting passage (so
//! abstention is genuinely correct, not a trick) and whose every
//! "answerable" item ships the witness that an answer exists
//! ([`question::ChaosBank::validate`]). Abstention must be *selective* — a
//! model that declines everything fails the competence red-line, so blanket
//! humility cannot game it.
//!
//! Scoring is **two independent red-lines, never blended** ([`score`]):
//! competence-when-present and honesty-when-absent must both clear their
//! gate. Confident hallucination on an absent fact is the cardinal sin and
//! carries its own ceiling.
//!
//! This module is pure logic (schema, fairness validation, scorer) so it
//! rebuilds and unit-tests in seconds. The elicitation adapter — driving the
//! live chat path, classifying answer-vs-abstain, and checking citation
//! fidelity (reusing the forced-choice / attribution primitives from
//! [`crate::mechanism_fidelity`]) — lives in the `sovereign-cli-llm`
//! `bench_cmd` orchestrator.

pub mod question;
pub mod score;

pub use question::{BankMeta, ChaosBank, ChaosQuestion, ExpectedAction, QuestionType};
pub use score::{score, AgentAction, CalibrationReport, ConfusionCounts, Gates, ResultRow, Verdict};
