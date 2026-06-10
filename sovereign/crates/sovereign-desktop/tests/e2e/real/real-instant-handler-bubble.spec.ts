// SPDX-License-Identifier: AGPL-3.0-or-later
// Regression for the early-arrival race (harness note 410db385): a
// handler that answers INSTANTLY (ConationQuery's canned empty-state
// reply on a first turn — no model in the loop) emits its chunks and
// complete while the send_message_stream invoke response is still in
// flight. Before the ChatView early-capture buffer, those events were
// destroyed/dropped and the user saw a spinner with no assistant
// bubble, ever. Real models mask the race behind first-token latency,
// which is exactly why only the harness caught it.
import { sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

test("instant-handler reply renders a bubble (early-arrival race)", async ({
  sovereignPage: page,
}) => {
  await realBootToChat(page);
  await page.locator(".new-btn").click();

  // "shorter please" on a FIRST turn: the embed router classifies it
  // ConationQuery (transform shape), and with no prior assistant
  // message the handler returns its canned empty-state reply
  // immediately — the fastest turn the backend can produce.
  const messageId = await sendAndAwaitTurn(page, "shorter please", {
    timeoutMs: 60_000,
  });
  expect(messageId.length).toBeGreaterThan(0);

  // The whole point: the reply must RENDER, not just complete on the
  // wire.
  const prose = page.locator(".sv-ai-msg .sv-prose").last();
  await expect(prose).toBeVisible({ timeout: 15_000 });
  await expect(prose).toContainText(/previous reply|rephrase/i);
});
