// SPDX-License-Identifier: AGPL-3.0-or-later
//! One case of the bank `bench/lanes/engine-swap/cases.py` builds: a recorded
//! or written engine call, and the inventory rows it exercises.

use serde::Deserialize;
use serde_json::Value;
use sovereign_contracts::engine_observe::ReplayMethod;

/// One case.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    /// Stable id.
    pub case_id: String,
    /// A synthetic case's label.
    #[serde(default)]
    pub label: String,
    /// What the driver does with it.
    pub method: CaseMethod,
    /// The method's input: a `CompletionRequest`, or the method's own shape.
    #[serde(default)]
    pub input: Value,
    /// A fault the driver must inject around the call.
    #[serde(default)]
    pub fault: Option<String>,
    /// Inventory rows the case exercises.
    #[serde(default)]
    pub rows: Vec<String>,
}

/// What the driver does with a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseMethod {
    /// Top-k logprobs over greedy steps from a token-id prompt (the compute
    /// row): the engine's probe route, or the server's `/completion`.
    Probe,
    /// Concurrent fast completions, timed.
    Throughput,
    /// One provider call through the daemon's replay route.
    #[serde(untagged)]
    Replay(ReplayMethod),
}
