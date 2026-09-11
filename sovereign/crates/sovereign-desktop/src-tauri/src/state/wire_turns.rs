// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live wire turns this surface is driving, and the prompts they have
//! parked on it (sv-surface RB5).
//!
//! A wire turn is a PER-CONVERSATION resource: the socket's URL names one
//! conversation, its prompts belong to that conversation, and
//! `cancel_stream(conversation_id)` is asking about exactly one of them.
//! `AppState` held ONE `Option<TurnSender>` for all of them, which made
//! three states representable that must not be: a cancel aimed at
//! conversation A tripping B's turn, a redirect overwriting the slot the
//! live turn parked, and the first pump to end clearing a sender still
//! being answered on.
//!
//! Both registries live here rather than as bare `RwLock<HashMap<..>>`
//! fields so the "is this entry still MINE" rule has ONE implementation
//! (ARCH §10.6) and can be exercised without a socket.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

/// A prompt the daemon parked on this surface: the card is up, the
/// executor is waiting, and an answer may arrive from any task.
#[derive(Debug, Clone)]
pub struct ParkedPrompt {
    /// The conversation whose wire turn raised it — the key into
    /// [`TurnWires`], and the only way `submit_*` (which the frontend
    /// calls with the card's `key` alone) can find the socket to answer
    /// on.
    pub conversation_id: String,
    /// The question itself, kept so a `ResolveAck { WrongKind }` can
    /// re-raise the SAME card byte-identically instead of leaving the
    /// user staring at a card the host has already refused an answer for.
    pub prompt: sovereign_contracts::types::TurnPrompt,
}

/// The write halves of the live wire turns, keyed by conversation id.
///
/// Generic over the sender so the ownership rules below are testable
/// without standing up a socket — `TurnSender` is opaque (a private
/// channel handle with no public constructor and no equality), which is
/// also why [`TurnWires::release`] identifies an entry by `Arc::ptr_eq`
/// rather than by value or by a generation counter.
pub struct TurnWires<S = sovereign_turn_client::TurnSender> {
    live: RwLock<HashMap<String, Arc<S>>>,
}

impl<S> Default for TurnWires<S> {
    fn default() -> Self {
        Self {
            live: RwLock::new(HashMap::new()),
        }
    }
}

impl<S> TurnWires<S> {
    /// Park a turn's write half. Returns the handle the pump keeps so it
    /// can prove ownership at [`TurnWires::release`].
    pub async fn park(&self, conversation_id: &str, sender: S) -> Arc<S> {
        let sender = Arc::new(sender);
        self.live
            .write()
            .await
            .insert(conversation_id.to_string(), Arc::clone(&sender));
        sender
    }

    /// The write half for one conversation, or `None` when no wire turn is
    /// parked for it.
    pub async fn sender_for(&self, conversation_id: &str) -> Option<Arc<S>> {
        self.live.read().await.get(conversation_id).cloned()
    }

    /// Whether a wire turn is parked for this conversation.
    pub async fn has(&self, conversation_id: &str) -> bool {
        self.live.read().await.contains_key(conversation_id)
    }

    /// Drop a turn's entry — but ONLY if it is still the one `mine`
    /// parked. A redirect opens a second socket on the same conversation
    /// while the first pump is still draining; the loser's end must not
    /// clear the winner's sender. Returns whether anything was removed.
    pub async fn release(&self, conversation_id: &str, mine: &Arc<S>) -> bool {
        let mut live = self.live.write().await;
        match live.get(conversation_id) {
            Some(parked) if Arc::ptr_eq(parked, mine) => {
                live.remove(conversation_id);
                true
            }
            _ => false,
        }
    }

    /// How many turns are parked — the fact the single-slot registry
    /// could not represent, and the one the tests below pin.
    #[cfg(test)]
    pub async fn len(&self) -> usize {
        self.live.read().await.len()
    }
}

/// Prompt ids the live wire turns have put to this surface and that no
/// answer has resolved yet — the wire-side form of the local desk's
/// `has_pending_information` guard.
#[derive(Default)]
pub struct PendingPrompts {
    parked: RwLock<HashMap<String, ParkedPrompt>>,
}

impl PendingPrompts {
    /// Record a prompt the daemon just put up.
    pub async fn park(
        &self,
        id: &str,
        conversation_id: &str,
        prompt: sovereign_contracts::types::TurnPrompt,
    ) {
        self.parked.write().await.insert(
            id.to_string(),
            ParkedPrompt {
                conversation_id: conversation_id.to_string(),
                prompt,
            },
        );
    }

    /// The prompt parked under `id`, or `None` — which is the honest
    /// answer to "is this card still live", and the refusal `submit_*`
    /// owes an answer aimed at a key nothing is parked under.
    pub async fn get(&self, id: &str) -> Option<ParkedPrompt> {
        self.parked.read().await.get(id).cloned()
    }

    /// Forget a prompt — the host said it resolved, or said nothing is
    /// parked there any more.
    pub async fn resolve(&self, id: &str) {
        self.parked.write().await.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RB5: cancel aims at ONE conversation. Watched red against the
    /// single-slot registry this replaced — `sender_for("a")` returned
    /// b's sender and `release` cleared whatever was there.
    #[tokio::test]
    async fn two_conversations_keep_their_own_senders() {
        let wires: TurnWires<&'static str> = TurnWires::default();
        let a = wires.park("conv-a", "sender-a").await;
        let b = wires.park("conv-b", "sender-b").await;

        assert_eq!(
            wires.sender_for("conv-a").await.as_deref().copied(),
            Some("sender-a"),
            "conv-a's own sender, not the most recently parked one"
        );
        assert_eq!(
            wires.sender_for("conv-b").await.as_deref().copied(),
            Some("sender-b")
        );
        assert_eq!(
            wires.len().await,
            2,
            "a second turn does not evict the first"
        );

        // conv-a's turn ends. conv-b is mid-turn and must be untouched.
        assert!(wires.release("conv-a", &a).await);
        assert!(wires.sender_for("conv-a").await.is_none());
        assert_eq!(
            wires.sender_for("conv-b").await.as_deref().copied(),
            Some("sender-b"),
            "the other conversation's parked sender survives"
        );
        drop(b);
    }

    /// A redirect opens a second socket on the SAME conversation while
    /// the first pump is still draining. The loser's end must not clear
    /// the winner's sender.
    #[tokio::test]
    async fn a_superseded_pump_does_not_clear_the_live_sender() {
        let wires: TurnWires<&'static str> = TurnWires::default();
        let first = wires.park("conv", "first").await;
        let second = wires.park("conv", "second").await;

        assert!(
            !wires.release("conv", &first).await,
            "the superseded pump owns nothing to release"
        );
        assert_eq!(
            wires.sender_for("conv").await.as_deref().copied(),
            Some("second"),
            "the live turn keeps its sender"
        );
        assert!(wires.release("conv", &second).await);
        assert_eq!(wires.len().await, 0);
    }

    /// Nothing parked is `None`, not a stale hit — the refusal
    /// `cancel_stream`/`submit_*` need in order to say so by name
    /// instead of claiming an answer reached a socket (§18.3).
    #[tokio::test]
    async fn an_unparked_conversation_has_no_sender() {
        let wires: TurnWires<&'static str> = TurnWires::default();
        let _held = wires.park("conv-a", "sender-a").await;
        assert!(wires.sender_for("conv-b").await.is_none());
        assert!(!wires.has("conv-b").await);
    }

    #[tokio::test]
    async fn a_prompt_remembers_its_conversation_and_its_card() {
        let pending = PendingPrompts::default();
        pending
            .park(
                "step:1",
                "conv-a",
                sovereign_contracts::types::TurnPrompt::UserInput {
                    question: "which one?".to_string(),
                },
            )
            .await;

        let parked = pending.get("step:1").await.expect("parked");
        assert_eq!(parked.conversation_id, "conv-a");
        assert!(matches!(
            parked.prompt,
            sovereign_contracts::types::TurnPrompt::UserInput { .. }
        ));
        assert!(pending.get("step:2").await.is_none());

        pending.resolve("step:1").await;
        assert!(pending.get("step:1").await.is_none());
    }
}
