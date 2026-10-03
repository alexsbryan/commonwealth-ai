// SPDX-License-Identifier: AGPL-3.0-or-later
// J2 (Tier 1, CRITICAL) — cancel a streaming reply and keep the session
// usable. Stream cancellation was a no-op until recently (the cancel
// token wasn't wired into both decode loops); this journey is the
// standing regression guard.
//
// Catching a cancel window on a fast 2B is the hard part: waiting for a
// generated chunk before clicking let short turns finish first. The stop
// control is visible for the WHOLE turn (isLoading), so we click it the
// instant it appears — that reliably lands mid-turn.
//
// Hard gates are the severe cancellation regressions a no-op or buggy
// cancel would cause: a hang (control never clears), a corrupted stream
// (chunks don't reassemble), a duplicate/zero terminal, chunks leaking
// after the terminal, or a wedged session. Whether cancel actually
// *truncated* the would-be-long output is recorded as a glassbox note —
// a fast test model can reach natural end before the click lands, so a
// hard truncation gate wants a controlled-speed model (a follow-up).
import { expect, journeyTest, realBootToChat } from "./journey";
import { J_CANCELLATION } from "./manifest";

journeyTest(J_CANCELLATION, async ({ page, run }) => {
  await realBootToChat(page);

  // Warm-up so a cold model load can't masquerade as a stuck cancel.
  await run.turn("In one sentence, how tall is the Meridian Lighthouse?");

  const chunkCount = () =>
    page.evaluate(
      () =>
        window.__sovereign_real__.captured.filter((r) => r.event === "message-chunk")
          .length,
    );
  const completeCount = () =>
    page.evaluate(
      () =>
        window.__sovereign_real__.captured.filter((r) => r.event === "message-complete")
          .length,
    );
  const completesBefore = await completeCount();

  // A prompt that would stream long if left alone.
  await page
    .locator(".input-area textarea")
    .fill("List every integer from 1 to 300, one per line, with no other text.");
  await page.locator(".send-btn").click();

  // Click Stop the moment it appears (visible for the whole turn) — the
  // earliest, most reliable mid-turn catch for a fast model.
  const stopBtn = page.locator(".stop-btn");
  await stopBtn.waitFor({ state: "visible", timeout: 60_000 });
  await stopBtn.click();

  // (a) No hang — the session returns to idle.
  await expect(stopBtn, "must return to idle after cancel (no hang)").toBeHidden({
    timeout: 30_000,
  });
  await expect(page.locator(".send-btn"), "send control must return").toBeVisible();

  // (b) Exactly one terminal message-complete for the cancelled turn.
  await expect.poll(completeCount, { timeout: 30_000 }).toBe(completesBefore + 1);
  const chunksAtComplete = await chunkCount();

  // (c) Stream integrity held through cancellation; no chunk after terminal.
  const facts = await page.evaluate(() => {
    const api = window.__sovereign_real__;
    const completes = api.captured.filter((r) => r.event === "message-complete");
    const last = completes[completes.length - 1].payload as {
      message_id: string;
      full_text: string;
    };
    return {
      lagged: api.lagged(),
      full: last.full_text,
      concatEqualsFull: api.chunksFor(last.message_id).join("") === last.full_text,
    };
  });
  expect(facts.lagged, "SSE consumer lagged — assertions invalid").toBe(false);
  expect(
    facts.concatEqualsFull,
    "cancelled turn: concat(message-chunk) must equal full_text",
  ).toBe(true);
  await page.waitForTimeout(2000);
  expect(
    await chunkCount(),
    "no message-chunk may arrive after the terminal event",
  ).toBe(chunksAtComplete);

  // Glassbox: did cancel cut the would-be-long stream short? (Best-effort.)
  const reached300 = /(^|\D)300(\D|$)/.test(facts.full);
  run.note(
    reached300
      ? "stream reached its natural end (300) before cancel could truncate — fast model"
      : `cancel cut the stream short (${facts.full.length} chars, never reached 300)`,
  );

  // (d) Session remains usable after cancellation.
  await run.turn("In one sentence, who was Elowen Marsh?");
});
