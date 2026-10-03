// SPDX-License-Identifier: AGPL-3.0-or-later
//! How an inference call fails, in the protocol's own vocabulary.
//!
//! # Why this exists
//!
//! Every fallible method on the inference contract used to return
//! `sovereign_contracts::Error` — a ~20-variant enum whose other half is the
//! agent runtime (`Planning`, `Execution`, `Tool`, `ToolNotFound`,
//! `PermissionDenied`, `UpdateNotAuthorised`). An OICP client cannot name that
//! type without depending on the runtime it exists to decouple from.
//!
//! The split is not a guess. Measured across all 54 `impl InferenceProvider
//! for` blocks in the workspace (2026-08-20), the variants those bodies
//! actually construct are: `NotImplemented` 57, `Inference` 55, `Routing` 7,
//! `QueueShed` 1, `InvalidInput` 1 — and `Storage` 1, the lone runtime leak.
//! `ComputeUnavailable` and `ModelNotLoaded` are constructed by the compute
//! child's own client and travel the same wire. That is this enum.
//!
//! # One spelling for the wire tag
//!
//! [`InferenceError::kind`] and [`InferenceError::from_wire`] replace a pair
//! of hand-written maps in `sovereign-compute`'s `wire.rs` (`error_kind` and
//! `WireError::into_error`). They had DIVERGED: `error_kind` emitted
//! `"compute_unavailable"` and `into_error` had no arm to read it back, so a
//! child that fail-fasted with "slot warming, retry after respawn" arrived at
//! the parent as a generic `Inference` fault with the retry semantics gone.
//! `QueueShed` had no tag at all and flattened the same way. Neither
//! direction had a round-trip test — §18.1's "a check with no failing input
//! you can name". The two directions are now one closed set with one test
//! that walks every variant (noun-convergence rung 2b).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A failure that belongs to the inference protocol rather than to any one
/// runtime. Returned by every fallible method on the inference contract.
///
/// Coarse on purpose: the variant classifies what a caller should DO (retry
/// elsewhere, retry later, give up, fix the request); the payload carries the
/// human-readable detail. Display strings are user-facing — surfaces render
/// them verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InferenceError {
    /// The backend failed mid-request. The catch-all: any fault that is not
    /// one of the more specific cases below is this one.
    Inference {
        /// Human-readable detail, surfaced verbatim.
        message: String,
    },

    /// A compute-slot child process was unreachable — not yet warm,
    /// mid-restart after a crash, or gone — and the request had no in-process
    /// fallback. Fail-fast: the caller retries after the supervisor respawns
    /// rather than hanging on a dead socket.
    ComputeUnavailable {
        /// Name of the compute slot or pool that was unreachable.
        slot: String,
        /// Why it was unavailable (warming / restarting / exited).
        reason: String,
    },

    /// The named model or slot was requested but is not resident in memory.
    ModelNotLoaded {
        /// The model or slot id that was asked for.
        model: String,
    },

    /// No eligible slot or peer could be selected. Scheduler-level, raised
    /// before inference starts.
    Routing {
        /// Why nothing could be selected.
        message: String,
    },

    /// The host refused the request rather than making it wait an
    /// unreasonable time for a model permit.
    ///
    /// Structured rather than prose because callers must branch on it: an
    /// HTTP boundary renders `503` + `Retry-After`, and a peer load balancer
    /// reads it as "try another holder". Distinct from [`Self::Routing`] —
    /// the request was well-formed and the model IS present. This says "not
    /// now, and here is when".
    QueueShed {
        /// 1-based place this caller would have taken in line.
        position: u32,
        /// Predicted wait, from observed turn durations on this slot.
        predicted_wait_ms: u64,
        /// Hint for `Retry-After`; always at least 1.
        retry_after_secs: u64,
    },

    /// The request was rejected by validation before any work started.
    InvalidInput {
        /// What was wrong with the request.
        message: String,
    },

    /// The operation is defined by the contract but this provider does not
    /// implement it. The single most common variant across the contract's
    /// implementors, because most inherit defaulted methods they cannot serve.
    NotImplemented {
        /// Which operation is unsupported.
        message: String,
    },

    /// The call was interrupted: caller cancel, or a response channel
    /// dropped.
    Cancelled,
}

impl InferenceError {
    /// Build a [`InferenceError::QueueShed`] with the retry hint DERIVED from
    /// the predicted wait.
    ///
    /// Exists so `retry_after_secs` has one implementation and one name
    /// (ARCH §10.6). Two independent shed sites report it — the model slot's
    /// predicted-wait gate and the FastShort coalescer's queue bound — and a
    /// second copy of `div_ceil(1_000).max(1)` is exactly the drift this
    /// constructor prevents. Always at least 1: a `Retry-After: 0` is an
    /// invitation to hot-loop the host that just refused you.
    pub fn queue_shed(position: u32, predicted_wait_ms: u64) -> Self {
        Self::QueueShed {
            position,
            predicted_wait_ms,
            retry_after_secs: predicted_wait_ms.div_ceil(1_000).max(1),
        }
    }

    /// Convenience for the catch-all variant.
    pub fn inference(message: impl Into<String>) -> Self {
        Self::Inference {
            message: message.into(),
        }
    }

    /// Convenience for the unsupported-operation variant.
    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self::NotImplemented {
            message: message.into(),
        }
    }

    /// Convenience for the rejected-request variant.
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    /// Convenience for the scheduler-level variant.
    pub fn routing(message: impl Into<String>) -> Self {
        Self::Routing {
            message: message.into(),
        }
    }

    /// Stable snake_case tag for the wire. Total over the enum — adding a
    /// variant without a tag is a compile error, which is the point.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Inference { .. } => "inference",
            Self::ComputeUnavailable { .. } => "compute_unavailable",
            Self::ModelNotLoaded { .. } => "model_not_loaded",
            Self::Routing { .. } => "routing",
            Self::QueueShed { .. } => "queue_shed",
            Self::InvalidInput { .. } => "invalid_input",
            Self::NotImplemented { .. } => "not_implemented",
            Self::Cancelled => "cancelled",
        }
    }

    /// Rebuild from a wire tag plus its message — the exact inverse of
    /// [`Self::kind`] for every variant whose payload is a single string.
    ///
    /// The structured variants (`ComputeUnavailable`, `QueueShed`) cannot be
    /// reconstructed from a tag and a sentence, so they are carried whole as
    /// JSON by the serde representation and this function is not used for
    /// them. An unknown tag becomes [`Self::Inference`] — the fault came from
    /// the provider regardless — and that is a deliberate widening, not a
    /// silent substitution: the message survives intact.
    pub fn from_wire(kind: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        match kind {
            "compute_unavailable" => Self::ComputeUnavailable {
                slot: String::new(),
                reason: message,
            },
            "model_not_loaded" => Self::ModelNotLoaded { model: message },
            "routing" => Self::Routing { message },
            "invalid_input" => Self::InvalidInput { message },
            "not_implemented" => Self::NotImplemented { message },
            "cancelled" => Self::Cancelled,
            // "inference", "queue_shed" (needs its numbers, so it never
            // round-trips through this path), and anything unrecognised.
            _ => Self::Inference { message },
        }
    }
}

impl fmt::Display for InferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inference { message } => write!(f, "Inference error: {message}"),
            Self::ComputeUnavailable { slot, reason } => {
                write!(f, "Compute slot unavailable: {slot}: {reason}")
            }
            Self::ModelNotLoaded { model } => write!(f, "Model not loaded: {model}"),
            Self::Routing { message } => write!(f, "Routing error: {message}"),
            Self::QueueShed {
                position,
                predicted_wait_ms,
                retry_after_secs,
            } => write!(
                f,
                "host busy: ~{predicted_wait_ms} ms predicted wait at queue position \
                 {position}; retry after {retry_after_secs}s"
            ),
            Self::InvalidInput { message } => write!(f, "Invalid input: {message}"),
            Self::NotImplemented { message } => write!(f, "Not implemented: {message}"),
            Self::Cancelled => write!(f, "Task cancelled"),
        }
    }
}

impl std::error::Error for InferenceError {}

/// Result alias for the inference contract.
pub type InferenceResult<T> = std::result::Result<T, InferenceError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant, so a new one cannot be added without a tag.
    fn every_variant() -> Vec<InferenceError> {
        vec![
            InferenceError::inference("boom"),
            InferenceError::ComputeUnavailable {
                slot: "pool-0".into(),
                reason: "warming".into(),
            },
            InferenceError::ModelNotLoaded {
                model: "qwen".into(),
            },
            InferenceError::routing("no holder"),
            InferenceError::queue_shed(3, 2_400),
            InferenceError::invalid_input("empty prompt"),
            InferenceError::not_implemented("rerank"),
            InferenceError::Cancelled,
        ]
    }

    #[test]
    fn kinds_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for e in every_variant() {
            assert!(seen.insert(e.kind()), "duplicate wire tag: {}", e.kind());
        }
    }

    /// The defect this type was minted to close: `sovereign-compute`'s
    /// `error_kind` emitted `"compute_unavailable"` and its `into_error` had
    /// no arm to read it, so the variant round-tripped to `Inference` and the
    /// fail-fast retry semantics were lost in transit.
    #[test]
    fn compute_unavailable_survives_the_wire_tag() {
        let e = InferenceError::ComputeUnavailable {
            slot: "pool-0".into(),
            reason: "warming".into(),
        };
        let back = InferenceError::from_wire(e.kind(), "warming");
        assert!(
            matches!(back, InferenceError::ComputeUnavailable { .. }),
            "compute_unavailable must not degrade to Inference: got {back:?}"
        );
    }

    /// Every string-payload variant survives tag + message. The two
    /// structured variants are excluded by construction, not by oversight —
    /// they travel as JSON.
    #[test]
    fn string_payload_variants_round_trip_through_the_tag() {
        for e in every_variant() {
            if matches!(
                e,
                InferenceError::QueueShed { .. } | InferenceError::ComputeUnavailable { .. }
            ) {
                continue;
            }
            let back = InferenceError::from_wire(e.kind(), "detail");
            assert_eq!(
                back.kind(),
                e.kind(),
                "{} did not survive its own wire tag",
                e.kind()
            );
        }
    }

    /// The whole value survives JSON, including the structured variants the
    /// tag alone cannot carry.
    #[test]
    fn every_variant_round_trips_through_json() {
        for e in every_variant() {
            let json = serde_json::to_string(&e).expect("serialize");
            let back: InferenceError = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, e, "json round-trip lost {}", e.kind());
        }
    }

    #[test]
    fn queue_shed_derives_a_retry_hint_of_at_least_one_second() {
        let InferenceError::QueueShed {
            retry_after_secs, ..
        } = InferenceError::queue_shed(1, 10)
        else {
            panic!("expected QueueShed");
        };
        assert_eq!(retry_after_secs, 1, "Retry-After: 0 hot-loops the host");

        let InferenceError::QueueShed {
            retry_after_secs, ..
        } = InferenceError::queue_shed(1, 2_400)
        else {
            panic!("expected QueueShed");
        };
        assert_eq!(retry_after_secs, 3, "2.4s must round up, not down");
    }
}
