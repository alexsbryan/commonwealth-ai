// SPDX-License-Identifier: AGPL-3.0-or-later
// Cancel storm: rapid send → cancel cycles must never orphan session
// state. After the storm, a clean turn completes and stream integrity
// holds. (The single-cancel case lives in the main real suite; this is
// the abuse-pattern variant.)
import { assertTurnInvariants, sendAndAwaitTurn } from "../invariants";
import { expect, realBootToChat, test } from "../test-base-real";

test("five rapid send/cancel cycles, then a clean turn", async ({
  sovereignPage: page,
  bridge,
}) => {
  // KNOWN BUG (sovereign todo note, 2026-06-10): cancelled partial
  // outputs accumulate as full history turns and prompt assembly
  // hard-fails at the context window instead of compacting — after
  // the storm, EVERY turn in this conversation errors "Prompt too
  // long … Shorten the conversation." This spec asserts the correct
  // contract (the conversation stays usable) and is marked
  // test.fail() until compaction/trimming lands; Playwright alerts
  // the moment it starts passing.
  test.fail();
  test.setTimeout(480_000);
  await realBootToChat(page);
  await page.locator(".new-btn").click();

  for (let i = 0; i < 5; i++) {
    const seqBefore = await page.evaluate(() => {
      const rows = window.__sovereign_real__.captured;
      return rows.length ? rows[rows.length - 1].seq : 0;
    });
    await page
      .locator(".input-area textarea")
      .fill(`Storm cycle ${i}: write a very long essay about the tides.`);
    await page.locator(".send-btn").click();
    // Cancel as soon as the stop affordance exists — sometimes before
    // the first token, sometimes mid-stream. Both must be safe.
    const stop = page.locator(".stop-btn");
    try {
      await stop.waitFor({ state: "visible", timeout: 20_000 });
      await stop.click();
    } catch {
      // Turn may have already completed (fast refusal) — also fine.
    }
    // Determinism: each cycle's turn must reach a TERMINAL event
    // before the next send. (First run of this spec raced here: the
    // send button reappeared while the cancelled cycle was still
    // streaming, producing two concurrent streams in one conversation
    // and a 150s no-complete stall — see note in the suite docs.)
    await expect
      .poll(
        () =>
          page.evaluate(
            (since) =>
              window.__sovereign_real__.captured.some(
                (r) =>
                  r.seq > since &&
                  (r.event === "message-complete" || r.event === "message-error"),
              ),
            seqBefore,
          ),
        { timeout: 90_000, intervals: [500, 1000] },
      )
      .toBe(true);
    await page.locator(".send-btn").waitFor({ state: "visible", timeout: 30_000 });
  }

  // The session must come out clean: one normal turn, full invariants.
  // (Budget: post-storm turns inherit the conversation's knowledge
  // thread → DeepQuery retrieval+synthesis on the small model.)
  const messageId = await sendAndAwaitTurn(page, "Reply with the single word: calm", {
    timeoutMs: 240_000,
  });
  const facts = await assertTurnInvariants(page, bridge, messageId, {
    expectFinish: null,
  });
  expect(facts.complete.full_text.length).toBeGreaterThan(0);
});
