// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one translation from the provider's error enum into the API layer's.

use oicp_types::LocalInferenceError;

/// Carry the structured refusals across the trait boundary; flatten
/// everything else to prose exactly as before.
///
/// Both chat entry points route through it, so a refusal cannot reach the
/// wire as what it is on one path and as a crash on the other
/// (ARCH_PRINCIPLES §10.6 — one decider, one name). A queue shed stays
/// backpressure (503 + `Retry-After`); a context overflow stays the
/// caller's to fix (llama-server's 400), where as prose it rendered as a
/// retryable 503.
pub(super) fn map_provider_error(e: sovereign_contracts::Error) -> LocalInferenceError {
    match e {
        sovereign_contracts::Error::QueueShed {
            position,
            predicted_wait_ms,
            retry_after_secs,
        } => LocalInferenceError::Shed {
            position,
            predicted_wait_ms,
            retry_after_secs,
        },
        sovereign_contracts::Error::ContextExceeded {
            prompt_tokens,
            n_ctx,
        } => LocalInferenceError::ContextExceeded {
            prompt_tokens,
            n_ctx,
        },
        other => LocalInferenceError::Other(format!("{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 35B battery's failing input: a 131,076-token prompt against a
    /// 131,068 window. As `Other` it rendered as a 503 and the agent's
    /// client retried it 30 times.
    #[test]
    fn a_context_overflow_keeps_its_numbers_across_the_boundary() {
        let mapped = map_provider_error(sovereign_contracts::Error::ContextExceeded {
            prompt_tokens: 131_076,
            n_ctx: 131_068,
        });
        assert!(
            matches!(
                mapped,
                LocalInferenceError::ContextExceeded {
                    prompt_tokens: 131_076,
                    n_ctx: 131_068
                }
            ),
            "{mapped:?}"
        );
        // The prose the logs and string-matching callers read is unchanged.
        assert!(mapped
            .to_string()
            .contains("Prompt too long: 131076 tokens"));
    }

    #[test]
    fn other_failures_still_flatten_to_prose() {
        let mapped = map_provider_error(sovereign_contracts::Error::Inference("boom".into()));
        assert!(
            matches!(&mapped, LocalInferenceError::Other(m) if m == "Inference error: boom"),
            "{mapped:?}"
        );
    }
}
