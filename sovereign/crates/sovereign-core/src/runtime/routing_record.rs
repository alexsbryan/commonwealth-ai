// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the turn records about the router's decision after the router has
//! returned.
//!
//! The router writes one `routing_log` row per classification and then the
//! turn's [`IntentPolicy`] gets a second vote on the intent. Until this
//! module the second vote went unrecorded, so `routing_log.classified_as`
//! could disagree with the handler that actually ran and nothing in the
//! row said why.

use sovereign_contracts::intent_policy::IntentPolicy;

use crate::types::Intent;

impl super::Runtime {
    /// Resolve the turn's effective intent from its policy, and record the
    /// override on the `routing_log` row the router wrote for this message.
    ///
    /// One decider for both doors: `turn.rs` and `streaming.rs` each
    /// computed `effective_intent.unwrap_or(raw)` separately.
    ///
    /// The write happens ONLY when the policy changed the intent, so a
    /// NULL `policy_intent` reads as "the router's verdict stood", not
    /// "nobody looked". Best-effort — a routing-log failure has never
    /// blocked a turn.
    pub(crate) async fn resolve_policy_intent(
        &self,
        message: &str,
        raw_intent: &Intent,
        policy: &IntentPolicy,
    ) -> Intent {
        let effective = policy
            .effective_intent
            .clone()
            .unwrap_or_else(|| raw_intent.clone());
        let overridden = effective != *raw_intent;
        tracing::debug!(
            raw = ?raw_intent,
            effective = ?effective,
            overridden,
            source = ?policy.source,
            "runtime: intent policy verdict"
        );
        if overridden {
            let hash = crate::router::message_hash(message);
            // `Intent::name()`, not `{effective:?}` — `policy_intent` is a
            // RECORDED route, and `name()` is documented at
            // `types/routing.rs` as the one rendering for that. Debug would
            // write `Continuation { task_id: "…" }`: a different string every
            // turn for the same route, ungroupable and unjoinable against
            // `routed_intent` on the turn's own metadata.
            let label = effective.name();
            if let Err(e) = self.store.log_routing_policy_intent(&hash, label).await {
                tracing::warn!(error = %e, "routing:policy_intent write failed");
            }
        }
        effective
    }
}
