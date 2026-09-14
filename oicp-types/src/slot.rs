// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a host reports about its own slots: which weights are resident right
//! now, and where they physically sit.
//!
//! Layer 0 for the same reason as [`crate::completion`] — an OICP client asks
//! a host what it is currently serving, so the answer's shape is protocol
//! vocabulary, not sovereign's. These four moved down out of
//! `sovereign_contracts::traits` with `InferenceProvider`'s residency methods
//! (noun-convergence rung 2b, 2026-08-20); `sovereign_contracts::traits`
//! re-exports them at their historical paths.

use serde::{Deserialize, Serialize};

/// Where a slot's weights physically live — the glassbox answer to
/// "is this model distributed across the mesh, and how is it split?".
/// Populated for the primary (the only distributable slot); `None` for
/// slots loaded purely locally, or before the placement is known. This
/// exists so an operator NEVER has to infer distribution from `free`
/// deltas or decode-latency signatures — the daemon states it outright.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotPlacement {
    /// `local` | `distributed` | `stream-split` | `forming`.
    pub mode: String,
    /// Total transformer blocks (layers) the plan apportions. `0` when
    /// unknown (a local load computes no block plan).
    pub total_blocks: u32,
    /// Blocks resident on THIS node's local GPU.
    pub local_blocks: u32,
    /// Per remote RPC worker (anchor) lending memory: endpoint + the
    /// block count pinned onto it. Empty for a local load.
    pub workers: Vec<WorkerPlacement>,
}

/// One remote worker's share of a distributed slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerPlacement {
    /// Raw-TCP rpc-server endpoint, `host:port`.
    pub endpoint: String,
    /// Transformer blocks pinned onto this worker.
    pub blocks: u32,
    /// Whether this worker holds the output head (`output.weight`).
    pub holds_output: bool,
}

/// A single inference slot's *actual* in-memory residency, as reported
/// by the engine that owns the weights — the `ollama ps` analog.
///
/// This is the ground truth for "what is loaded right now", distinct
/// from what is *configured* (`SetupConfig.models.*`) and from what is
/// *advertised* on `/v1/models` (config-derived). Only the embedded
/// engine can answer it; every other provider inherits the empty
/// default of [`InferenceProvider::resident_slots`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidentSlot {
    /// Role stem: `fast` | `primary` | `embed` | `code` | `rerank` |
    /// `extra:<name>` | `pool:<i>`. Stable across the config workstream
    /// so a UI can join residency to the configured slot by role.
    pub role: String,
    /// The model id (gguf file stem) currently occupying the slot.
    pub model_id: String,
    /// `true` when the weights are resident in memory this instant.
    /// A lazy slot (primary/code) reports `false` while idle-unloaded.
    pub resident: bool,
    /// Resident byte footprint when the engine knows it, else `None`.
    pub size_bytes: Option<u64>,
    /// `true` when the slot is mid load/unload (its lock was contended
    /// at read time) — residency is momentarily indeterminate. Never
    /// forces a load to resolve it.
    pub transitioning: bool,
    /// Physical placement of the weights (distributed vs local + the
    /// per-device split). `None` for non-distributable slots. The
    /// glassbox answer that must never require guessing.
    #[serde(default)]
    pub placement: Option<SlotPlacement>,
}

/// What a slot has DEMONSTRATED about decoding since this process started —
/// the evidence behind `/healthz`'s per-slot verdict.
///
/// Recorded at the engine's completion boundary, never produced by a probe, so
/// reading it cannot block and cannot force a load (a status field fed by a
/// call that can block is how a health check hangs on the thing it reports).
/// Three harnesses reported false results on 2026-09-12 because nothing could
/// tell "the subject died" from "the subject failed"; residency alone cannot
/// either — weights can be resident while every decode errors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotDecodeEvidence {
    /// Role stem, the same join key as [`ResidentSlot::role`].
    pub role: String,
    /// Model id of the most recent attempt that named one.
    pub model_id: String,
    /// Completions that returned a response.
    pub ok: u64,
    /// Completions that returned an error.
    pub failed: u64,
    /// Unix seconds of the most recent success, if any.
    #[serde(default)]
    pub last_ok_unix: Option<u64>,
    /// Unix seconds of the most recent failure, if any.
    #[serde(default)]
    pub last_failure_unix: Option<u64>,
    /// Error text of the most recent failure, truncated.
    #[serde(default)]
    pub last_failure: Option<String>,
    /// Whether the most recent outcome was a success. Stored rather than
    /// derived from the timestamps, which collide within one second.
    #[serde(default)]
    pub last_outcome_ok: Option<bool>,
}

impl SlotDecodeEvidence {
    /// Longest failure text kept; an error string is evidence, not a log.
    pub const MAX_FAILURE_CHARS: usize = 240;

    /// `None` when the slot has never been asked to decode — "not yet
    /// demonstrated" is absence, never a default of healthy. Otherwise, whether
    /// the latest attempt succeeded.
    pub fn decode_capable(&self) -> Option<bool> {
        self.last_outcome_ok
    }

    /// Record a completion that returned a response.
    pub fn record_ok(&mut self, model_id: &str, now_unix: u64) {
        self.ok += 1;
        self.model_id = model_id.to_string();
        self.last_ok_unix = Some(now_unix);
        self.last_outcome_ok = Some(true);
    }

    /// Record a completion that returned an error; the text is truncated.
    pub fn record_failure(&mut self, error: &str, now_unix: u64) {
        self.failed += 1;
        self.last_failure_unix = Some(now_unix);
        self.last_failure = Some(error.chars().take(Self::MAX_FAILURE_CHARS).collect());
        self.last_outcome_ok = Some(false);
    }
}

#[cfg(test)]
mod decode_evidence_tests {
    use super::SlotDecodeEvidence;

    #[test]
    fn a_slot_that_never_decoded_is_not_reported_capable() {
        assert_eq!(SlotDecodeEvidence::default().decode_capable(), None);
    }

    #[test]
    fn the_latest_outcome_decides_even_within_one_second() {
        let mut e = SlotDecodeEvidence::default();
        e.record_ok("m", 100);
        assert_eq!(e.decode_capable(), Some(true));
        e.record_failure("decode -1", 100);
        assert_eq!(
            e.decode_capable(),
            Some(false),
            "same-second failure must win"
        );
        e.record_ok("m", 100);
        assert_eq!(e.decode_capable(), Some(true));
        assert_eq!((e.ok, e.failed), (2, 1));
    }

    #[test]
    fn failure_text_is_bounded() {
        let mut e = SlotDecodeEvidence::default();
        e.record_failure(&"x".repeat(10_000), 1);
        assert_eq!(
            e.last_failure.map(|s| s.chars().count()),
            Some(SlotDecodeEvidence::MAX_FAILURE_CHARS)
        );
    }
}

/// One supervised compute-child's live status (DISTRIBUTED_PILOT_READINESS.md
/// P1). Surfaced by [`InferenceProvider::compute_children`] and rendered on
/// `/status` so an operator watching a silent local-only fallback sees the
/// child lifecycle, not just a slower model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeChildStatus {
    /// Replica name (`<pool>-<i>`).
    pub name: String,
    /// `"generate"` | `"embed"`.
    pub role: String,
    /// The addressable pool id this replica belongs to.
    pub model_id: String,
    /// Lifecycle: `starting` | `warming` | `serving` | `degraded` |
    /// `restarting` | `failed`.
    pub lifecycle: String,
    /// Current ephemeral port, when serving/warming.
    #[serde(default)]
    pub port: Option<u16>,
    /// Restart count.
    pub restarts: u32,
    /// Reason for the most recent lifecycle transition.
    pub last_transition_reason: String,
    /// Reason for the most recent exit/crash, if any.
    #[serde(default)]
    pub last_exit: Option<String>,
}
