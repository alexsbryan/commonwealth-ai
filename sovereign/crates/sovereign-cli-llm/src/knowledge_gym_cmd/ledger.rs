// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tool ledger — ONE record of what a replay's tool surface did, and
//! the ONE thing every predicate reads.
//!
//! # Why this exists
//!
//! Until 2026-09-07 the gym's predicates read the OpenAI `tool_calls` field
//! off a `/v1/chat/completions` response. That field is produced by the
//! daemon's native function-calling adapter and by NOTHING the product runs:
//! neither production path that offers `knowledge_lookup` speaks it. The
//! attached-doc handler
//! (`sovereign-core/src/runtime/handlers/attached_doc.rs`) renders tools as a
//! prose line and parses an inline `<tool_call>` marker back out, recording
//! what fired as `NarrationPhase::ToolInvocation{Start,Complete}`. The
//! executor's `ReasonWithTools` step
//! (`sovereign-core/src/executor.rs::execute_reason_with_tools`) does the same
//! parse and records `SearchLogEntry` rows. So "did the tool fire" had three
//! answers in three shapes, and the gym's was the one no user can reach.
//!
//! [`ToolLedger`] is the one answer. Every driver — raw endpoint, executor,
//! attached-doc — projects its own record into these rows, and
//! `runner::eval_block` reads only these. ARCH §10.6: one decider, one name.

use serde::Serialize;

/// Which surface a fixture is replayed through.
///
/// A closed set (ARCH §2.1). The two product members name the two code paths
/// that actually offer `knowledge_lookup` to a model; [`Self::Raw`] names the
/// endpoint the gym used to drive, which no product turn goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionPath {
    /// `Executor::execute_reason_with_tools` — the `ReasonWithTools` step a
    /// planner emits. Production prompt, production `<tool_call>` parse,
    /// production `ToolRegistry::call_cached` dispatch; the ledger is the
    /// step's own `search_log`.
    Executor,
    /// `Runtime::handle_attached_doc_turn` — the turn a conversation with a
    /// `DocumentSession` takes. Ledger is the session's narration log.
    AttachedDoc,
    /// `POST /v1/chat/completions` with an OpenAI `tools` array. Kept for
    /// model-only measurement (`--raw`) and labelled on the wire, because a
    /// number from here is a fact about the model and its function-calling
    /// adapter, not about the product.
    Raw,
}

/// Serialised THROUGH [`ProductionPath::as_str`], not by a `serde(rename_all)`
/// that would be a second spelling of the same three names (ARCH §10.6).
impl Serialize for ProductionPath {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl ProductionPath {
    /// The wire/TOML spelling. One name (ARCH §10.6) — the fixture declares
    /// it with this string, the report prints this string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Executor => "executor",
            Self::AttachedDoc => "attached-doc",
            Self::Raw => "raw",
        }
    }

    /// Parse a fixture's declaration. `None` for an unknown spelling — the
    /// caller refuses the fixture rather than defaulting it, because a typo
    /// that silently fell back to `raw` would put a fixture back on the
    /// surface this module exists to leave (ARCH §18.3).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "executor" => Some(Self::Executor),
            "attached-doc" => Some(Self::AttachedDoc),
            "raw" => Some(Self::Raw),
            _ => None,
        }
    }

    /// Every spelling a fixture may declare, for the refusal message.
    pub const ALL: [&'static str; 3] = ["executor", "attached-doc", "raw"];

    /// Does a turn a user can actually take go through this path?
    ///
    /// The report prints `not-the-product` beside every fixture for which
    /// this is false. A pass there says the model's function-calling adapter
    /// held a contract; it says nothing about the product.
    pub fn is_product(self) -> bool {
        !matches!(self, Self::Raw)
    }

    /// The wire label. `None` on a product path, so the JSON carries the
    /// caveat only where the caveat applies.
    pub fn caveat(self) -> Option<&'static str> {
        if self.is_product() {
            None
        } else {
            Some("not-the-product")
        }
    }
}

/// One tool invocation, as the path that ran it recorded it.
#[derive(Debug, Clone, Serialize)]
pub struct ToolLedgerEntry {
    /// Invocation index within its user turn. The raw driver's inner
    /// tool-loop iteration; the executor's `SearchLogEntry::iteration`.
    pub loop_idx: usize,
    /// Canonical tool id the path dispatched.
    pub name: String,
    /// The query argument the model supplied, when the path recorded one.
    pub query: Option<String>,
    /// Evidence handles the gym's canned envelope returned FOR THIS CALL.
    /// Recorded by the mock, not read back off the model — a predicate that
    /// asserted on a field the subject echoes back is not a check (ARCH
    /// §18.1).
    pub returned_evidence_ids: Vec<String>,
    /// Source-kind strings for each returned row, indexed alongside the ids.
    pub returned_evidence_kinds: Vec<String>,
    /// The canned envelope declared `cached: true` (Tier-4 hit).
    pub cached: bool,
    /// How many results the PATH counted in what it handed the model.
    ///
    /// `None` when the path keeps no count (the raw driver hands the envelope
    /// through verbatim). `Some(n)` is the production path's own tally, and it
    /// is the row that catches a path which dispatched the tool, got rows
    /// back, and delivered none of them to the model — which is invisible in
    /// every other field here.
    pub path_result_count: Option<usize>,
}

/// Every tool invocation in one user turn, in order.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TurnLedger {
    pub entries: Vec<ToolLedgerEntry>,
}

impl TurnLedger {
    pub fn push(&mut self, e: ToolLedgerEntry) {
        self.entries.push(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_spelling_is_the_toml_spelling() {
        // One name. A `serde(rename_all)` here would be a second decider, and
        // the report and the fixture would be free to drift apart.
        for s in ProductionPath::ALL {
            let p = ProductionPath::parse(s).expect("declared spelling parses");
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{s}\""));
        }
    }

    #[test]
    fn every_declared_spelling_parses_back_to_itself() {
        for s in ProductionPath::ALL {
            let p = ProductionPath::parse(s).unwrap_or_else(|| panic!("{s} must parse"));
            assert_eq!(p.as_str(), s);
        }
    }

    #[test]
    fn unknown_spelling_is_refused_not_defaulted() {
        // The failing input this guard exists for: a fixture that meant
        // `executor` and typed `execute` must not silently land on `raw`.
        assert!(ProductionPath::parse("execute").is_none());
        assert!(ProductionPath::parse("attached_doc").is_none());
        assert!(ProductionPath::parse("").is_none());
    }

    #[test]
    fn only_raw_carries_the_caveat() {
        assert_eq!(ProductionPath::Raw.caveat(), Some("not-the-product"));
        assert_eq!(ProductionPath::Executor.caveat(), None);
        assert_eq!(ProductionPath::AttachedDoc.caveat(), None);
        assert!(!ProductionPath::Raw.is_product());
        assert!(ProductionPath::Executor.is_product());
        assert!(ProductionPath::AttachedDoc.is_product());
    }
}
