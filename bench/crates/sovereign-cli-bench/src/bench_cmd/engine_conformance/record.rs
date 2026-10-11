// SPDX-License-Identifier: AGPL-3.0-or-later
//! One line of a battery pass: what one target did with one case.
//!
//! The driver writes these, the judge only reads them, and this file is the
//! contract between the two. A facet the driver could not observe on a target
//! stays `None`, and the judge reports that check could-not-judge rather than
//! reading absence as agreement.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What one target did with one case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRecord {
    /// Stable id of the case in the bank.
    pub case_id: String,
    /// The target's label, e.g. `embedded`, `llama-server`, `mesh-llm`.
    pub target: String,
    /// Inventory rows this case exercises.
    pub rows: Vec<String>,
    /// The `CompletionRequest` as replayed, for checks that read the request
    /// itself (an allow-list, a think budget).
    #[serde(default)]
    pub request: Value,
    /// How the call ended.
    pub outcome: Outcome,
    /// What could be observed.
    #[serde(default)]
    pub facets: Facets,
}

/// How a call ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Outcome {
    /// The call answered.
    Ok,
    /// The call refused the request by name (an `InvalidInput`, an
    /// unsupported shape).
    Refused {
        /// The refusal as the caller saw it.
        message: String,
    },
    /// The call failed for another reason.
    Error {
        /// The error as the caller saw it.
        message: String,
    },
}

/// Everything the driver could observe for one call. Each check reads the
/// facets it names and nothing else.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Facets {
    /// The token ids of the prompt the model was given.
    pub prompt_ids: Option<Vec<i64>>,
    /// The sampler as resolved for this call.
    pub sampler: Option<Sampler>,
    /// The grammar text the decode was constrained by, if any.
    pub grammar: Option<String>,
    /// Greedy token ids generated.
    pub greedy_tokens: Option<Vec<i64>>,
    /// Top-k `(token, logprob)` at each generated position.
    pub logprobs: Option<Vec<Vec<(i64, f64)>>>,
    /// Forced-choice label probabilities.
    pub label_probs: Option<BTreeMap<String, f64>>,
    /// Embedding vectors, one per input text, in input order.
    pub embeddings: Option<Vec<Vec<f32>>>,
    /// Rerank scores, in document order.
    pub rerank_scores: Option<Vec<f32>>,
    /// `count_tokens` of the case text.
    pub token_count: Option<u64>,
    /// The response as the caller received it.
    pub output: Option<Output>,
    /// Tokens generated inside the think block.
    pub reasoning_tokens: Option<u64>,
    /// Prompt tokens actually evaluated, cache hits excluded.
    pub prefill_evaluated: Option<u64>,
    /// Aggregate generated tokens per second (timed cases only).
    pub tokens_per_s: Option<f64>,
    /// Resident bytes of the model set.
    pub resident_bytes: Option<u64>,
    /// What the provider reports about itself, by key.
    #[serde(default)]
    pub host_reported: BTreeMap<String, Value>,
    /// The serving process's own state for the same keys.
    #[serde(default)]
    pub host_observed: BTreeMap<String, Value>,
    /// Why the driver left a facet unobserved, by facet name, so a
    /// could-not-judge cell carries the cause rather than only the absence.
    #[serde(default)]
    pub unobserved: BTreeMap<String, String>,
}

/// A resolved sampler: named parameters and the order the stages run in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sampler {
    /// Parameter values under canonical names (`temperature`, `top_k`, …).
    pub params: BTreeMap<String, f64>,
    /// Stage names in the order they run.
    #[serde(default)]
    pub order: Vec<String>,
}

/// A response as the caller received it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Output {
    /// The answer text.
    pub text: String,
    /// The reasoning, where the caller receives it apart from the text.
    pub reasoning: Option<String>,
    /// Tool calls, parsed.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// The finish reason as the caller sees it.
    pub finish: Option<String>,
    /// Token usage as the caller sees it.
    pub usage: Option<Usage>,
    /// Kinds of the stream frames received (`token`, `reasoning`,
    /// `tool-calls`, `usage`, `error`, `finish`), for streamed cases.
    #[serde(default)]
    pub frames: Vec<String>,
}

/// One parsed tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The function name.
    pub name: String,
    /// The arguments, parsed.
    pub arguments: Value,
}

/// Token usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens.
    pub prompt: u64,
    /// Completion tokens.
    pub completion: u64,
}
