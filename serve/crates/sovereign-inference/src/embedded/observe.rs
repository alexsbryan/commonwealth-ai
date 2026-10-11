// SPDX-License-Identifier: AGPL-3.0-or-later
//! The embedded engine's records for the conformance battery
//! (`sovereign_contracts::engine_observe`). Each is a no-op unless the
//! daemon's replay route installed a sink around the call.

use sovereign_contracts::engine_observe::{observe, Observation};

use crate::llama::cpp::token::LlamaToken;

/// The prompt's token ids, as the model is given them.
pub(crate) fn prompt_tokens(tokens: &[LlamaToken]) {
    observe(|| Observation::PromptTokens {
        ids: tokens.iter().map(|t| i64::from(t.0)).collect(),
    });
}

/// Prompt tokens evaluated and reused by one prefill. `kept` tokens were
/// already in the context, restored from a pin or kept from the last call,
/// unless this call decoded them into a newly learned pin, in which case it
/// evaluated them too.
pub(crate) fn prefill(prompt_len: usize, kept: usize, learned_pin: bool) {
    let reused = if learned_pin { 0 } else { kept };
    observe(|| Observation::Prefill {
        evaluated: (prompt_len - reused) as u64,
        reused: reused as u64,
    });
}
