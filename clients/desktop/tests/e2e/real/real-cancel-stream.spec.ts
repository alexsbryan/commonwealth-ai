// SPDX-License-Identifier: AGPL-3.0-or-later
// Cancel mid-stream against the real sampler: the stop button must
// terminate a live generation promptly, the partial stream must still
// satisfy integrity (concat == full_text), and the next turn must
// succeed cleanly — no orphaned session state.
import { assertTurnInvariants, sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

test("stop button cancels a live stream; session recovers for the next turn", async ({
  sovereignPage: page,
  bridge,
}) => {
  await realBootToChat(page);
  await page.locator(".new-btn").click();

  const before = await page.evaluate(
    () =>
      window.__sovereign_real__.captured.filter((r) => r.event === "message-complete")
        .length,
  );
  await page
    .locator(".input-area textarea")
    .fill("Write a detailed thousand-word story about a voyage across the sea.");
  await page.locator(".send-btn").click();

  // Wait until tokens are actually flowing, then stop.
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            window.__sovereign_real__.captured.filter((r) => r.event === "message-chunk")
              .length,
        ),
      { timeout: 120_000, intervals: [250, 500, 1000] },
    )
    .toBeGreaterThan(3);
  await page.locator(".stop-btn").click();

  // The stream must terminate promptly after cancel.
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            window.__sovereign_real__.captured.filter(
              (r) => r.event === "message-complete",
            ).length,
        ),
      { timeout: 30_000, intervals: [250, 500, 1000] },
    )
    .toBeGreaterThan(before);

  const cancelledId = await page.evaluate(() => {
    const completes = window.__sovereign_real__.captured.filter(
      (r) => r.event === "message-complete",
    );
    return (completes[completes.length - 1].payload as { message_id: string }).message_id;
  });
  // Integrity holds for the partial text, and the glassbox reports the
  // truth: the turn was cancelled, not naturally stopped. (Fixed
  // 2026-06-10 — the session cancel token was previously discarded by
  // the runtime, so cancel was a no-op and finish_reason came from the
  // provider's natural stop; note df66cb8d.)
  await assertTurnInvariants(page, bridge, cancelledId, { expectFinish: "cancelled" });

  // Recovery: a fresh turn in the same conversation completes.
  const nextId = await sendAndAwaitTurn(page, "Reply with the single word: recovered");
  const facts = await assertTurnInvariants(page, bridge, nextId);
  expect(facts.complete.full_text.length).toBeGreaterThan(0);
});
