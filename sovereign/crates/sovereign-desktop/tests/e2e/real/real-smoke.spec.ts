// SPDX-License-Identifier: AGPL-3.0-or-later
// The proving spec for the real-mode harness: one user-shaped turn
// through the REAL stack — real Tauri command dispatch, real routing,
// real inference, real streamed tokens — asserted at the UI surface
// plus the stream-integrity invariant.
//
// What passing means:
//   • the sticky backend-ready replay boots the page to the chat view
//     without any synthetic handshake,
//   • send_message_stream dispatched through the production invoke
//     path starts a real stream,
//   • message-chunk events render incrementally in the bubble,
//   • concat(message-chunk payloads) === message-complete.full_text
//     (the wire-integrity contract everything in Phase 2+ leans on).
import { expect, realBootToChat, test } from "./test-base-real";

test("real stack: send a message, stream real tokens, verify integrity", async ({
  sovereignPage: page,
  bridge,
}) => {
  await realBootToChat(page);

  const input = page.locator(".input-area textarea");
  await input.fill("Reply with one short sentence: what is 2+2?");
  await page.locator(".send-btn").click();

  // The user bubble renders immediately.
  await expect(page.locator(".bubble.user .content")).toContainText("2+2");

  // A real assistant message streams in and terminates. Budget covers
  // a cold slot load on the fast profile.
  const aiProse = page.locator(".sv-ai-msg .sv-prose").last();
  await expect(aiProse).toBeVisible({ timeout: 150_000 });
  await expect
    .poll(
      async () =>
        bridge.real((api) =>
          api.captured.some((r) => r.event === "message-complete"),
        ),
      { timeout: 150_000, intervals: [500, 1000, 2000] },
    )
    .toBe(true);

  // Stream integrity: the bytes the UI assembled are exactly the
  // terminal full_text, with no SSE lag holes.
  const verdict = await bridge.real((api) => {
    const complete = api.captured.find((r) => r.event === "message-complete");
    if (!complete) return { ok: false, why: "no message-complete captured" };
    const payload = complete.payload as { message_id: string; full_text: string };
    const concat = api.chunksFor(payload.message_id).join("");
    return {
      ok: concat === payload.full_text && !api.lagged(),
      why:
        concat === payload.full_text
          ? api.lagged()
            ? "SSE lagged"
            : ""
          : `concat(${concat.length} ch) != full_text(${payload.full_text.length} ch)`,
      fullText: payload.full_text,
    };
  });
  expect(verdict.why).toBe("");
  expect(verdict.ok).toBe(true);

  // The terminal text actually rendered in the bubble.
  const rendered = (await aiProse.textContent()) ?? "";
  expect(rendered.trim().length).toBeGreaterThan(0);

  // Glassbox: the turn left provenance behind (real metadata, not a
  // synthetic scenario's). Full invariant pack lands in Phase 2.
  const complete = await bridge.real((api) => {
    const row = api.captured.find((r) => r.event === "message-complete");
    return row?.payload as { metadata?: Record<string, unknown> | null };
  });
  expect(complete.metadata).toBeTruthy();
  expect(complete.metadata).toHaveProperty("intent");
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
