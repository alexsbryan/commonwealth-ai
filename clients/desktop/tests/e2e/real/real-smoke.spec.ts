// SPDX-License-Identifier: AGPL-3.0-or-later
// The proving spec for the real-mode harness: one user-shaped turn
// through the REAL stack — real Tauri command dispatch, real routing,
// real inference, real streamed tokens — asserted at the UI surface
// plus the full invariant pack (stream integrity, provenance,
// citation resolution, numeric honesty; see invariants.ts).
//
// History: this spec's first-ever run caught a live inference bug —
// complete_stream_with_finish was missing the FastShort→Fast streaming
// coercion and panicked unreachable! mid-turn (embedded/engine.rs:2479,
// fixed 2026-06-09).
import { assertTurnInvariants, sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

test("real stack: send a message, stream real tokens, verify invariants", async ({
  sovereignPage: page,
  bridge,
}) => {
  await realBootToChat(page);

  const messageId = await sendAndAwaitTurn(
    page,
    "What is the chemical symbol for gold?",
  );

  const facts = await assertTurnInvariants(page, bridge, messageId);
  expect(facts.chunkCount).toBeGreaterThan(0);

  // The user bubble and the terminal assistant text both rendered.
  await expect(page.locator(".bubble.user .content").last()).toContainText("chemical symbol");
  const rendered = (await page.locator(".sv-ai-msg .sv-prose").last().textContent()) ?? "";
  expect(rendered.trim().length).toBeGreaterThan(0);
});

test("real stack: conversation persists across the bridge", async ({
  sovereignPage: page,
  bridge,
}) => {
  await realBootToChat(page);

  // The previous spec's turn landed in the scratch store — the real
  // SQLite-backed conversation list must reflect it.
  const conversations = await bridge.invoke<Array<{ id: string }>>(
    "list_conversations",
  );
  expect(Array.isArray(conversations)).toBe(true);
  expect(conversations.length).toBeGreaterThan(0);

  const full = await bridge.invoke<{ messages: Array<{ role: string }> }>(
    "get_conversation",
    { conversationId: conversations[0].id },
  );
  const roles = full.messages.map((m) => m.role);
  expect(roles).toContain("user");
  expect(roles).toContain("assistant");
});
