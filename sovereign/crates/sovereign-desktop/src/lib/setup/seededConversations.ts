// SPDX-License-Identifier: AGPL-3.0-or-later
// Seed conversations created on first launch. The user lands in
// chat with two real conversations in the sidebar — one about what
// the system is, one about its privacy posture — each with a
// pre-filled draft in the input box. Clicking a seeded conversation
// surfaces the draft as a one-shot pre-fill (see ChatView's
// loadConversation); the user presses Enter to send or edits/deletes
// it. They behave exactly like user-created conversations after
// that — rename, delete, scroll all work normally.

import {
  createConversation,
  listConversations,
  renameConversation,
} from "../api";

export const SEEDED_STARTERS: { title: string; prompt: string }[] = [
  {
    title: "What is this?",
    prompt: "What is Sovereign, and what's it for?",
  },
  {
    title: "How private is this?",
    prompt: "What stays on my machine, and what doesn't?",
  },
];

/**
 * Seed two starter conversations iff the user has none yet.
 * Idempotent: any pre-existing conversation suppresses seeding so
 * re-running setup never duplicates the seeds. Per-conversation
 * drafts go into localStorage; ChatView reads + removes them on
 * first open.
 */
export async function ensureSeededConversations(): Promise<void> {
  let existing: Awaited<ReturnType<typeof listConversations>> = [];
  try {
    existing = await listConversations(2, 0);
  } catch {
    // listConversations throws iff the backend isn't ready, which
    // shouldn't be possible on this code path (we only call after
    // setup completes). Treat as "skip seeding" — better to leave
    // the user with an empty conversation list than crash on first
    // launch.
    return;
  }
  if (existing.length > 0) return;

  // Create newest-last so the conversation list (sorted by
  // updated_at desc) shows "What is this?" on top.
  for (let i = SEEDED_STARTERS.length - 1; i >= 0; i--) {
    const { title, prompt } = SEEDED_STARTERS[i];
    try {
      const { id } = await createConversation();
      await renameConversation(id, title);
      try {
        localStorage.setItem(`chat-draft:${id}`, prompt);
      } catch {
        // Private-mode / quota failures are tolerable — the
        // conversation still exists, just without the pre-fill.
      }
    } catch (e) {
      // If creation/rename fails for any reason, log but keep
      // going — partial seeding is fine.
      console.warn("ensureSeededConversations: skip seed", title, e);
    }
  }
}
