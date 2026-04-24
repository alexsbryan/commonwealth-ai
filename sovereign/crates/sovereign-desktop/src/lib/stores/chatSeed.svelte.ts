// Chat-seed store.
//
// One question at a time, set from anywhere in the app, consumed by
// ChatView on its next render when `messages.length === 0`.
//
// Scope: the cross-component handoff for "here, open chat with this
// question pre-filled and auto-submitted". Replaces the prop-drilled
// `onOpenChatWithSeed` pattern for the background-toast case where
// the originating component (FolderDropFlow) may have unmounted
// before the user clicks "Ask a question".
//
// Consumers:
//   - ChatView reads `chatSeedStore.pending` in an $effect; on non-
//     null and empty messages, fills inputText + sends, then calls
//     `consume()` to clear.
//   - FolderDropFlow's atlas-complete toast sets a seed via
//     `chatSeedStore.set(q)` when the user clicks "Ask a question".
//   - FirstCorpusFlow's starter-chip click routes through here too
//     so there's one handoff path.

import type { StarterQuestion } from "../types";

let _pending: StarterQuestion | null = $state(null);

export const chatSeedStore = {
  get pending(): StarterQuestion | null {
    return _pending;
  },

  /// Push a seed. Replaces any existing pending seed (last-writer-
  /// wins — two back-to-back toast clicks would collapse to the
  /// newer one, which is the right default).
  set(question: StarterQuestion): void {
    _pending = question;
  },

  /// Consume and clear. ChatView calls this after pre-filling +
  /// sending so the next state change doesn't re-fire.
  consume(): StarterQuestion | null {
    const q = _pending;
    _pending = null;
    return q;
  },

  /// Drop the seed without using it. Useful when navigation cancels
  /// the handoff (e.g., a modal closes before chat mounts).
  clear(): void {
    _pending = null;
  },
};
