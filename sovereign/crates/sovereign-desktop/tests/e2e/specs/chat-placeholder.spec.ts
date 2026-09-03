// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Placeholder narration regression test. The 400ms threshold and the
// suppression rules (narration / docProgressText override) are
// load-bearing TTFI UX — if they regress, the dot-stare reappears
// silently. The TTFI silent-fast scenario catches the headline case;
// these tests pin the FSM-level invariants:
//
//   1. No placeholder before threshold (~350ms)
//   2. Placeholder appears AT threshold when no signal arrived
//   3. Narration arriving mid-wait replaces the placeholder
//   4. Narration arriving BEFORE threshold suppresses it entirely
//   5. Completion clears the placeholder

test.describe("preparing-state placeholder", () => {
  test("appears at ~400ms when no specific signal arrives", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("anything");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();

    // The contract: the placeholder eventually appears with the
    // expected text, and once it does the typing dots are gone. We
    // don't pin the typing-indicator's presence at a specific time —
    // under parallel-worker contention the placeholder timer can fire
    // anywhere in [400, 600] ms, and asserting on dots in that window
    // makes the test flaky without adding signal.
    const placeholder = page.locator(
      '.doc-progress-indicator[data-source="placeholder"]',
    );

    await expect(placeholder).toBeVisible({ timeout: 1_500 });
    await expect(placeholder).toContainText("Working on it");
    await expect(page.locator(".typing-indicator")).toHaveCount(0);
  });

  test("narration arriving before threshold suppresses placeholder entirely", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("with narration");
    await page.locator(".send-btn").click();
    const start = await expect
      .poll(() => chat.api.lastStreamStart())
      .not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    // Fire narration at 150ms — well under the 400ms threshold.
    await page.waitForTimeout(150);
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: "routing_committed",
          text: "Got it — looking now.",
          elapsed_ms: 150,
        },
      });
    }, ctx.conversationId);

    // Wait past the placeholder threshold. Narration is in the slot,
    // placeholder must NEVER fire.
    await page.waitForTimeout(500);
    await expect(
      page.locator('.doc-progress-indicator[data-source="narration"]'),
    ).toBeVisible();
    await expect(
      page.locator('.doc-progress-indicator[data-source="placeholder"]'),
    ).toHaveCount(0);
  });

  test("narration arriving AFTER placeholder replaces it (no flicker back to dots)", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("late narration");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    // Wait past threshold so placeholder fires.
    await page.waitForTimeout(550);
    await expect(
      page.locator('.doc-progress-indicator[data-source="placeholder"]'),
    ).toBeVisible();

    // Now fire narration. The slot should swap to data-source=narration
    // — never falling back to typing dots in between.
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: "retrieval_complete",
          text: "Found 8 passages.",
          elapsed_ms: 700,
        },
      });
    }, ctx.conversationId);

    await expect(
      page.locator('.doc-progress-indicator[data-source="narration"]'),
    ).toBeVisible();
    // Placeholder gone, dots not back.
    await expect(
      page.locator('.doc-progress-indicator[data-source="placeholder"]'),
    ).toHaveCount(0);
    await expect(page.locator(".typing-indicator")).toHaveCount(0);
  });

  test("placeholder clears on message-complete", { tag: ["@GR-43"] }, async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("complete after wait");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    await page.waitForTimeout(550);
    await expect(
      page.locator('.doc-progress-indicator[data-source="placeholder"]'),
    ).toBeVisible();

    await chat.api.completeMessage(start.messageId, "done.");

    // isLoading goes false; the entire indicator block unmounts.
    await expect(
      page.locator('.doc-progress-indicator[data-source="placeholder"]'),
    ).toHaveCount(0);
    await expect(page.locator(".typing-indicator")).toHaveCount(0);
  });
});

// ─── Sentence-stare guard (rotation) ────────────────────────────
// When the loading slot has had specific text for >1500ms with no
// update, append a "(still working)" suffix so the user sees that the
// system is acknowledging the longer wait. After 3000ms it escalates
// to "(taking longer than usual)". Always-on diamond pulse handles
// the visual "still alive" cue. Suspended during clarification because
// the system is then waiting on the user, not crunching.
test.describe("preparing-state rotation", () => {
  test("appends '(still working)' after 1500ms of static slot text", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("long synthesis");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    // Land a narration immediately. Slot now has stable text.
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: "primary_synthesis_start",
          text: "Drafting.",
          elapsed_ms: 50,
        },
      });
    }, ctx.conversationId);

    const slot = page.locator(
      '.doc-progress-indicator[data-source="narration"] .progress-text',
    );
    await expect(slot).toContainText("Drafting.");
    // Sub-1500ms: no rotation suffix yet.
    await page.waitForTimeout(800);
    await expect(slot).not.toContainText("still working");

    // Past the stale threshold (ChatView STALE_INTERVAL_MS = 3500ms)
    // + auto-wait slack for parallel-worker drift: suffix appended,
    // original text retained.
    await expect(slot).toContainText("still working", { timeout: 5_000 });
    await expect(slot).toContainText("Drafting.");
  });

  test("rotation resets when a new narration arrives", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("rotating narrations");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: { phase: "routing_committed", text: "First.", elapsed_ms: 50 },
      });
    }, ctx.conversationId);

    const slot = page.locator(
      '.doc-progress-indicator[data-source="narration"] .progress-text',
    );
    // Wait past rotation, see suffix. Generous timeout absorbs
    // parallel-worker drift on the 3500ms interval (ChatView
    // STALE_INTERVAL_MS).
    await expect(slot).toContainText("still working", { timeout: 5_000 });

    // New narration replaces text and resets the rotation timer —
    // suffix should disappear, fresh text shows alone.
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: "retrieval_complete",
          text: "Second.",
          elapsed_ms: 1700,
        },
      });
    }, ctx.conversationId);

    await expect(slot).toContainText("Second.");
    await expect(slot).not.toContainText("still working");
  });

  test("rotation is suspended while a clarification card is up", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("ambiguous");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: { phase: "routing_committed", text: "Asking.", elapsed_ms: 50 },
      });
      window.__sovereign_test__.emit("clarification-request", {
        session_id: "s1",
        conversation_id: cid,
        question: "Which one?",
        options: [
          { label: "A", follow_up: "tell me about A", intent_hint: "deep_query" },
          { label: "B", follow_up: "tell me about B", intent_hint: "deep_query" },
        ],
      });
    }, ctx.conversationId);

    await expect(page.locator(".clarification-card")).toBeVisible();

    // Wait well past the 1500ms threshold. Rotation must NOT fire
    // because the system is waiting on the user, not working.
    await page.waitForTimeout(1_800);
    const slot = page.locator(
      '.doc-progress-indicator[data-source="narration"] .progress-text',
    );
    await expect(slot).not.toContainText("still working");
    await expect(slot).toContainText("Asking.");
  });

  test("diamond accent has the breathing pulse class while loading", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("anything");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();

    // After the placeholder threshold, the indicator is up. Its
    // diamond mark must carry the .pulse class — the "still alive"
    // visual cue that handles long waits beyond the textual suffixes.
    await expect(
      page.locator(".doc-progress-indicator .progress-mark.pulse"),
    ).toBeVisible({ timeout: 1_000 });
  });
});
