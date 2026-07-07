// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-turn ephemeral scratch object powering the antifragile
//! routing paradigm.
//!
//! A `QuerySession` records everything the runtime did for one user
//! turn: the classification, the policy decision, any cached
//! retrieval, partial response, narration log, and a cancellation
//! token the UI can trigger from the desktop's `redirect_turn`
//! command. It is *ephemeral* — not `StateStore`-worthy per ARCH
//! §4.5, because the data is only meaningful while the user's
//! attention is on this turn. Preservation-across-turns beyond the
//! 30-second garbage-collect horizon is future work (roadmap PR4+).
//!
//! The store is an `Arc<DashMap>` (ARCH §8 — workspace dep) so
//! every caller reads the same session without fighting a lock.
//! PR1 only writes to the store; PR2 will read from it on redirect.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use dashmap::DashMap;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::types::{NarrationEvent, NarrationPhase, RouterClassification, RoutingPolicy};

/// Identifier for a live turn's scratch object. Scoped to a single
/// runtime process (in-memory, not persisted).
pub type SessionId = String;

/// Conversation identifier — mirrors the string type used across
/// `ConversationStore`. Kept loosely typed to avoid an import cycle
/// with `types.rs`.
pub type ConversationRef = String;

/// Ephemeral turn-scratch. Created after `Router::classify` returns,
/// dropped 30s after the response completes (or on the next user
/// turn that isn't a redirect).
#[derive(Debug, Clone)]
pub struct QuerySession {
    pub id: SessionId,
    pub conversation_id: ConversationRef,
    /// Mirror of the `Conversation.skill_id` for structural privacy
    /// symmetry (ARCH §7). When the active skill is `local_only`,
    /// the session must not carry alternatives that would leak to a
    /// mesh peer. PR1 only populates this; PR2 honours it.
    pub skill_id: Option<String>,
    pub input: String,
    pub classification: RouterClassification,
    pub policy: RoutingPolicy,
    /// Cancellation token threaded to the inference sampler. On
    /// redirect, the desktop calls `cancel.cancel()`; the sampler
    /// drops the current decode on its next check, and the runtime
    /// re-enters the dispatcher with the alternative intent. PR1
    /// threads the token through; PR2 wires the desktop `redirect_turn`
    /// command that actually fires it.
    pub cancel: CancellationToken,
    pub created_at: SystemTime,
    /// Monotonic reference for computing `elapsed_ms` on narration
    /// entries. `SystemTime` is used for retention math (not monotonic,
    /// but good enough for 30s GC); `Instant` is used for per-turn
    /// narration timing so clock skew doesn't surface as nonsense.
    pub started_at: Instant,
    /// Narration entries emitted so far. Bounded by
    /// `MAX_NARRATION_EVENTS_PER_TURN` in the runtime emitter. PR2
    /// appends; the desktop renders.
    pub narration: Vec<NarrationEvent>,
}

/// Cap on narration entries per turn. Prevents pollution even if
/// new emission points are added carelessly. Sized for the four
/// `NarrationPhase` variants currently defined (RoutingCommitted,
/// RetrievalComplete, PrimarySynthesisStart, GapCheckFired).
pub const MAX_NARRATION_EVENTS_PER_TURN: usize = 4;

/// Don't narrate below this elapsed threshold — short responses
/// don't need chrome. The runtime checks elapsed at the phase
/// boundary, so a sub-threshold turn emits nothing and a
/// just-over turn emits at most one backward-looking entry.
///
/// Was 5s when only one phase was wired (RetrievalComplete on
/// shape divergence). Lowered so a long DeepQuery turn — which
/// today can spend 90s loading a CPU primary slot before any
/// token streams — gets the RetrievalComplete + PrimarySynthesisStart
/// chips early enough to reassure the user, instead of staring
/// at a static "Working on it…" placeholder for the whole wait.
pub const NARRATION_MIN_ELAPSED: Duration = Duration::from_millis(1_500);

/// How long a completed session lingers before GC sweeps it.
/// Picked to cover "user reads the banner, decides to redirect"
/// plus a little slack.
pub const SESSION_RETENTION: Duration = Duration::from_secs(30);

/// Thin wrapper over the shared DashMap. Exists so the Runtime can
/// hand callers an `Arc<SessionStore>` without leaking the map type.
#[derive(Debug)]
pub struct SessionStore {
    sessions: DashMap<SessionId, QuerySession>,
    /// Cancel tokens reserved for turns that have STARTED but not yet
    /// reached `begin` — the "preparing" window (build-context +
    /// classification + retrieval), which on a slow model is several
    /// seconds long. Keyed by conversation so a `cancel_stream` that
    /// arrives during preparing (before the turn's session exists)
    /// still cancels the right token: `reserve_cancel` mints it at the
    /// top of the turn, `begin` ADOPTS it into the session, and
    /// `cancel_preparing` trips it in place. Without this, a
    /// preparing-phase cancel hit only the *previous* (stale) session
    /// via `latest_for_conversation`, and the real turn began later
    /// with a fresh, uncancelled token — the turn ran to completion.
    /// (Confirmed 2026-07-07: on a 4B, classification delayed `begin`
    /// ~5s past the Stop click; the synth token read `cancelled=false`
    /// throughout.)
    preparing_cancels: DashMap<ConversationRef, CancellationToken>,
    /// Minimum elapsed time before a `try_emit_narration` call will
    /// be allowed through. Defaults to [`NARRATION_MIN_ELAPSED`].
    /// Tests override via [`SessionStore::with_narration_min_elapsed`]
    /// so they can drive the runtime end-to-end without sleeping
    /// past the production threshold on every assertion.
    narration_min_elapsed: Duration,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            preparing_cancels: DashMap::new(),
            narration_min_elapsed: NARRATION_MIN_ELAPSED,
        }
    }

    /// Override the narration suppression threshold. Test-only knob:
    /// production code keeps the const default. Pass `Duration::ZERO`
    /// to disable the gate so a runtime test can assert that the
    /// expected `NarrationPhase` events fire on a near-instant
    /// stubbed turn.
    pub fn with_narration_min_elapsed(mut self, threshold: Duration) -> Self {
        self.narration_min_elapsed = threshold;
        self
    }

    /// Reserve a cancel token for a turn that is STARTING but hasn't
    /// classified yet, so a cancel during the preparing window lands on
    /// the right turn. Call once at the top of the turn; `begin` adopts
    /// the token. Overwrites any prior reservation for the conversation
    /// (turns are serialized per conversation, so a new turn supersedes
    /// an abandoned one). The returned token is the same handle `begin`
    /// will thread into synthesis.
    pub fn reserve_cancel(&self, conversation_id: &str) -> CancellationToken {
        let cancel = CancellationToken::new();
        self.preparing_cancels
            .insert(conversation_id.to_string(), cancel.clone());
        cancel
    }

    /// Trip the reserved preparing-token for a conversation, if any.
    /// Leaves the entry in place so `begin` still adopts it (already
    /// cancelled → the turn starts cancelled and its synthesis loop
    /// breaks on the first poll). Returns whether a reservation existed.
    pub fn cancel_preparing(&self, conversation_id: &str) -> bool {
        if let Some(entry) = self.preparing_cancels.get(conversation_id) {
            entry.value().cancel();
            true
        } else {
            false
        }
    }

    /// Create and register a fresh session. Returns the id + the
    /// cancel-token handle so the caller can pass both into the
    /// dispatcher without re-fetching. If the turn reserved a
    /// preparing-token (`reserve_cancel`), ADOPT it so a cancel that
    /// arrived during preparing carries through to this session's
    /// synthesis; otherwise mint a fresh one.
    pub fn begin(
        &self,
        conversation_id: ConversationRef,
        skill_id: Option<String>,
        input: String,
        classification: RouterClassification,
        policy: RoutingPolicy,
    ) -> (SessionId, CancellationToken) {
        let id = Uuid::new_v4().to_string();
        let cancel = self
            .preparing_cancels
            .remove(&conversation_id)
            .map(|(_, token)| token)
            .unwrap_or_default();
        let session = QuerySession {
            id: id.clone(),
            conversation_id,
            skill_id,
            input,
            classification,
            policy,
            cancel: cancel.clone(),
            created_at: SystemTime::now(),
            started_at: Instant::now(),
            narration: Vec::new(),
        };
        self.sessions.insert(id.clone(), session);
        (id, cancel)
    }

    /// Append a narration entry to an in-flight session, if the
    /// turn has been running long enough and the cap hasn't been
    /// hit. Returns the event that was recorded (for the runtime to
    /// forward to Tauri) or `None` when suppression rules fired.
    ///
    /// Suppression rules (per ARCH §0.1 — glassbox doesn't mean
    /// noisy): (a) under 5s total elapsed → suppress; (b) already at
    /// the 3-event cap → suppress. Both are observable via a
    /// `narration:suppressed` tracing event at the call site.
    pub fn try_emit_narration(
        &self,
        session_id: &str,
        phase: NarrationPhase,
        text: String,
    ) -> Option<NarrationEvent> {
        let mut entry = self.sessions.get_mut(session_id)?;
        let elapsed = entry.started_at.elapsed();
        if elapsed < self.narration_min_elapsed {
            return None;
        }
        if entry.narration.len() >= MAX_NARRATION_EVENTS_PER_TURN {
            return None;
        }
        let event = NarrationEvent {
            phase,
            text,
            elapsed_ms: elapsed.as_millis() as u64,
        };
        entry.narration.push(event.clone());
        Some(event)
    }

    /// Push a narration entry directly, bypassing the elapsed-time
    /// suppression and the per-turn cap. Required for `ToolInvocation*`
    /// frames per the contract in `NarrationPhase` (see types.rs):
    /// tool activity has to surface immediately even on fast turns,
    /// and a multi-tool ReasonWithTools loop can fire more frames
    /// than the 3-event cap would otherwise allow.
    ///
    /// Returns `None` when no session by that id exists.
    pub fn force_push_narration(
        &self,
        session_id: &str,
        phase: NarrationPhase,
        text: String,
    ) -> Option<NarrationEvent> {
        let mut entry = self.sessions.get_mut(session_id)?;
        let elapsed = entry.started_at.elapsed();
        let event = NarrationEvent {
            phase,
            text,
            elapsed_ms: elapsed.as_millis() as u64,
        };
        entry.narration.push(event.clone());
        Some(event)
    }

    pub fn get(&self, id: &str) -> Option<QuerySession> {
        self.sessions.get(id).map(|s| s.clone())
    }

    pub fn remove(&self, id: &str) -> Option<QuerySession> {
        self.sessions.remove(id).map(|(_, s)| s)
    }

    /// Garbage-collect sessions older than `SESSION_RETENTION`. The
    /// runtime calls this opportunistically (per turn) — lazy GC is
    /// sufficient for the turn-scoped lifetime.
    pub fn sweep_expired(&self) -> usize {
        let now = SystemTime::now();
        let mut removed = 0;
        self.sessions.retain(|_, s| {
            let age = now.duration_since(s.created_at).unwrap_or_default();
            let keep = age < SESSION_RETENTION;
            if !keep {
                removed += 1;
            }
            keep
        });
        removed
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Return the most recent live session for a given conversation.
    /// Useful for test assertions + potential future features that
    /// want to inspect "the turn we're currently serving on this
    /// conversation." Returns `None` if no live session matches —
    /// the caller should treat that as "no in-flight turn."
    pub fn latest_for_conversation(&self, conversation_id: &str) -> Option<QuerySession> {
        let mut best: Option<QuerySession> = None;
        for entry in self.sessions.iter() {
            if entry.conversation_id == conversation_id {
                let candidate = entry.clone();
                best = match best {
                    None => Some(candidate),
                    Some(existing) if candidate.created_at > existing.created_at => Some(candidate),
                    other => other,
                };
            }
        }
        best
    }

    /// The conversation ids of all live sessions. Used by the desktop's
    /// `cancel_stream` to make a lookup miss legible (a cancel that finds no
    /// session is a no-op; logging the live inventory shows whether it's an
    /// id mismatch vs. an already-finished turn).
    pub fn conversation_ids(&self) -> Vec<String> {
        self.sessions
            .iter()
            .map(|e| e.conversation_id.clone())
            .collect()
    }
}

/// Handy alias — most callers hold `Arc<SessionStore>`.
pub type SharedSessionStore = Arc<SessionStore>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ConfidenceThresholds, ConfidenceTier, Intent, IntentCandidate, MoveKind,
        RouterClassification, RoutingPolicy,
    };

    fn sample_classification(confidence: f32) -> RouterClassification {
        RouterClassification {
            primary: IntentCandidate {
                intent: Intent::SimpleQuery,
                confidence,
            },
            alternatives: Vec::new(),
            rationale: None,
            coarse_intent: Some("SIMPLE".to_string()),
            self_assessment: None,
            timing: None,
            scope: None,
        }
    }

    fn sample_policy() -> RoutingPolicy {
        RoutingPolicy {
            move_kind: MoveKind::Commit,
            tier: ConfidenceTier::High,
            thresholds_used: ConfidenceThresholds::default(),
        }
    }

    #[test]
    fn begin_registers_session_and_returns_token() {
        let store = SessionStore::new();
        let (id, token) = store.begin(
            "conv-1".into(),
            None,
            "hello".into(),
            sample_classification(0.9),
            sample_policy(),
        );
        assert_eq!(store.len(), 1);
        let s = store.get(&id).expect("session visible");
        assert_eq!(s.conversation_id, "conv-1");
        assert_eq!(s.input, "hello");
        assert!(!token.is_cancelled());
        // Cancelling via the returned handle must propagate to the
        // stored session — they share the underlying token.
        token.cancel();
        let s = store.get(&id).unwrap();
        assert!(s.cancel.is_cancelled());
    }

    #[test]
    fn sweep_expired_drops_old_sessions() {
        let store = SessionStore::new();
        let (id, _) = store.begin(
            "conv-1".into(),
            None,
            "hello".into(),
            sample_classification(0.9),
            sample_policy(),
        );
        // Backdate the session so sweep removes it.
        {
            let mut s = store.sessions.get_mut(&id).unwrap();
            s.created_at = SystemTime::now() - SESSION_RETENTION - Duration::from_secs(1);
        }
        let removed = store.sweep_expired();
        assert_eq!(removed, 1);
        assert!(store.is_empty());
    }

    #[test]
    fn try_emit_narration_suppresses_under_min_elapsed() {
        let store = SessionStore::new();
        let (id, _) = store.begin(
            "conv-1".into(),
            None,
            "hello".into(),
            sample_classification(0.9),
            sample_policy(),
        );
        // Brand-new session: elapsed ≈ 0ms, well below
        // `NARRATION_MIN_ELAPSED`. Emit must be dropped so a
        // sub-threshold turn doesn't flash a chip and disappear.
        let out = store.try_emit_narration(
            &id,
            NarrationPhase::RoutingCommitted,
            "looking this up…".into(),
        );
        assert!(out.is_none(), "short turn must not narrate");
        assert_eq!(store.get(&id).unwrap().narration.len(), 0);
    }

    #[test]
    fn try_emit_narration_enforces_cap() {
        let store = SessionStore::new();
        let (id, _) = store.begin(
            "conv-1".into(),
            None,
            "hello".into(),
            sample_classification(0.9),
            sample_policy(),
        );
        // Force the session past the 5s suppression window without
        // actually sleeping — back-date `started_at`.
        {
            let mut s = store.sessions.get_mut(&id).unwrap();
            s.started_at = Instant::now() - Duration::from_secs(10);
        }
        // Fill to the cap.
        for _ in 0..MAX_NARRATION_EVENTS_PER_TURN {
            assert!(store
                .try_emit_narration(&id, NarrationPhase::RoutingCommitted, "x".into())
                .is_some());
        }
        // One more must be dropped.
        let overflow =
            store.try_emit_narration(&id, NarrationPhase::GapCheckFired, "too many".into());
        assert!(overflow.is_none());
        assert_eq!(
            store.get(&id).unwrap().narration.len(),
            MAX_NARRATION_EVENTS_PER_TURN
        );
    }

    #[test]
    fn try_emit_narration_returns_none_for_unknown_session() {
        let store = SessionStore::new();
        let out = store.try_emit_narration(
            "does-not-exist",
            NarrationPhase::RoutingCommitted,
            "x".into(),
        );
        assert!(out.is_none());
    }

    #[test]
    fn remove_returns_session() {
        let store = SessionStore::new();
        let (id, _) = store.begin(
            "conv-1".into(),
            None,
            "hello".into(),
            sample_classification(0.9),
            sample_policy(),
        );
        let s = store.remove(&id).expect("removed");
        assert_eq!(s.id, id);
        assert!(store.is_empty());
    }
}
