// SPDX-License-Identifier: AGPL-3.0-or-later
// Input-contract regression guards — deterministic pins for the breaker's
// 2026-06-23 findings (the ratchet: every confirmed finding becomes a
// standing spec so it can't silently come back).
//
// The breaker found adversarial inputs that produced turns violating the
// streaming + provenance contract: a 100KB message was surfaced as a
// malformed message-complete (no chunk, no intent), and low-confidence /
// clarification turns carried no intent. Fixed in:
//   • commands/chat.rs — the non-streaming fallback now emits the body as
//     a chunk (so concat == full_text) and the error branch carries an
//     `intent` marker;
//   • runtime/handlers/ask_move.rs + knowledge_query.rs — clarification /
//     retrieval-miss metadata now carries an `intent`.
//
// These send the exact adversarial inputs THROUGH THE COMMAND BRIDGE — the
// UI disables Send for empty/oversize input (ChatView.svelte:1559), so the
// bridge is the only way to exercise the runtime path they hit — and
// assert the invariant pack holds for every one: exactly one terminal,
// concat(message-chunk) == full_text, and an intent present.
import type { Page } from "@playwright/test";
import { assertTurnInvariants } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

/** Send a message through the command bridge (not the UI) and await its
 *  terminal, returning the message id. Events are still captured by the
 *  page shim — ChatView subscribed to message-chunk/-complete on boot, so
 *  they ring regardless of who initiated the turn. */
async function bridgeTurn(
  page: Page,
  bridge: { invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> },
  message: string,
  timeoutMs = 120_000,
): Promise<string> {
  const convo = await bridge.invoke<{ id: string }>("create_conversation", {});
  const started = await bridge.invoke<{ message_id: string }>("send_message_stream", {
    message,
    conversationId: convo.id,
  });
  const mid = started.message_id;
  await expect
    .poll(
      () =>
        page.evaluate(
          (m) =>
            window.__sovereign_real__.captured.some(
              (r) =>
                (r.event === "message-complete" || r.event === "message-error") &&
                (r.payload as { message_id?: string })?.message_id === m,
            ),
          mid,
        ),
      { timeout: timeoutMs, intervals: [500, 1000, 2000] },
    )
    .toBe(true);
  return mid;
}

// (label, payload) — the inputs that originally broke the contract.
const CASES: Array<[string, string]> = [
  ["empty", ""],
  ["whitespace-only", "   \n\t   "],
  ["oversize-100k", "A".repeat(100_000)],
];

test.describe("input-contract regressions (breaker findings 2026-06-23)", () => {
  for (const [name, message] of CASES) {
    test(`${name} input → contract-compliant turn (chunk integrity + intent)`, async ({
      sovereignPage: page,
      bridge,
    }) => {
      await realBootToChat(page);
      const mid = await bridgeTurn(page, bridge, message);
      // The contract these inputs used to violate: exactly one terminal,
      // concat(message-chunk) == full_text, and an intent present. A
      // degenerate input must still produce a well-formed turn (a
      // clarification or a clean error), never an intent-less, chunk-less
      // blank.
      await assertTurnInvariants(page, bridge, mid);
    });
  }
});
