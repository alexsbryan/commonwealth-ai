// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `routing_log` contract: what a router (and the turn that follows it)
//! records about how one message was classified.
//!
//! Its own file rather than a block in `traits.rs` because that file is at
//! its arch-gate ceiling (ARCH §3.1), and this trait is the one the join
//! keys below belong to.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::RoutingCorrection;

/// Persistence for the routing log: per-message classifications, correctness
/// feedback, and redirect signals that feed threshold calibration.
#[async_trait]
pub trait RoutingStore: Send + Sync {
    /// Record one classification: the message's hash, the chosen intent
    /// label, classification latency, and the conversation the message
    /// belongs to.
    ///
    /// `conversation_id` is the join key, and it goes in at INSERT rather
    /// than through a follow-up UPDATE because `message_hash` is not unique
    /// — a write keyed on the hash alone can land on another conversation's
    /// row. `None` means the caller genuinely has no conversation, never
    /// "not filled in yet".
    ///
    /// There is no `message_id`: the assistant message this turn produces
    /// does not exist yet when the router runs.
    async fn log_routing(
        &self,
        message_hash: &str,
        classified_as: &str,
        latency_ms: i64,
        conversation_id: Option<&str>,
    ) -> Result<()>;
    /// Record the post-router intent override on the `routing_log` row
    /// `log_routing` wrote for `message_hash`.
    ///
    /// Called only when the turn's [`crate::intent_policy::IntentPolicy`]
    /// produced an effective intent that DIFFERS from the router's verdict,
    /// so a NULL `policy_intent` reads as "the policy left the router's
    /// intent standing" — not "unknown". Default no-op so existing
    /// implementations compile without changes.
    ///
    /// `&'static str` is the invariant, not a lifetime convenience: a
    /// recorded route is a closed set of labels, so the only values that
    /// belong here are [`crate::types::Intent::name`]'s. `&format!("{i:?}")`
    /// — the rendering that leaks `Continuation { task_id: … }` into the
    /// column and makes routes ungroupable — does not compile against it.
    async fn log_routing_policy_intent(
        &self,
        message_hash: &str,
        policy_intent: &'static str,
    ) -> Result<()> {
        let _ = (message_hash, policy_intent);
        Ok(())
    }
    /// Attach metacognition fields to a routing_log row written by `log_routing`.
    /// Default no-op so existing implementations compile without changes.
    async fn log_routing_meta(
        &self,
        message_hash: &str,
        coarse_intent: &str,
        self_assessment: Option<&str>,
    ) -> Result<()> {
        let _ = (message_hash, coarse_intent, self_assessment);
        Ok(())
    }
    /// Most recent user-flagged misclassifications (rows with `was_correct = false`) — the router's avoid-list.
    async fn get_routing_corrections(&self, limit: usize) -> Result<Vec<RoutingCorrection>>;
    /// Record the user's verdict on the classification previously logged for `message_hash`.
    async fn mark_routing_correct(&self, message_hash: &str, was_correct: bool) -> Result<()>;
    /// PR4 — record an explicit user redirect away from a
    /// Propose-tier commit. Sets `routing_log.was_redirected = 1`
    /// and `routing_log.redirect_to = <intent_hint>` for the row
    /// previously written by `log_routing`. A future calibration
    /// job tunes confidence thresholds from the aggregate of these
    /// signals. Default no-op so legacy implementations compile.
    async fn mark_routing_redirected(&self, message_hash: &str, redirect_to: &str) -> Result<()> {
        let _ = (message_hash, redirect_to);
        Ok(())
    }
}
