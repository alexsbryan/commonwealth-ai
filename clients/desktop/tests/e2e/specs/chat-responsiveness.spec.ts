// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Responsiveness assertions. The chat surface has historically felt
// laggy; these tests pin down the behaviors that distinguish "smooth"
// from "janky" so regressions surface as test failures rather than vibes.
//
// Each test bounds a specific perceived-performance budget. The numbers
// are generous on purpose — we want to catch bad regressions, not
// flake-out on noisy CI. Tighten them as the surface gets smoother.

test.describe("chat responsiveness", () => {
  // First-token latency: how quickly does the placeholder bubble's prose
  // populate after the very first chunk lands? This is the most-felt
  // metric on a slow chat surface — every ms shows up as visible delay
  // between "I see typing dots" and "I see words".
  test("first chunk renders within 500ms of arrival", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await page.locator(".input-area textarea").fill("ping");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    const t0 = Date.now();
    // First chunk has a trailing space so the word buffer flushes.
    await chat.api.streamTokens(start.messageId, ["hello "], 0);
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText("hello");
    const elapsed = Date.now() - t0;

    expect(elapsed).toBeLessThan(500);
  });

  // Stop button is the user's escape hatch for a turn that's taking too
  // long. It MUST appear instantly when streaming starts and dispatch
  // cancel_stream the moment it's clicked. Anything sluggish here is
  // exactly the "I have to wait to bail" friction the user is complaining
  // about.
  test("Stop button appears mid-stream and dispatches cancel synchronously", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await page.locator(".input-area textarea").fill("long task");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();

    // Stop appears as soon as we're loading.
    await expect(page.locator(".stop-btn")).toBeVisible();
    await expect(page.locator(".send-btn")).toHaveCount(0);

    const t0 = Date.now();
    await page.locator(".stop-btn").click();
    await expect.poll(() => chat.api.lastCancel()).not.toBeNull();
    // 500ms budget absorbs Playwright click-dispatch + IPC overhead
    // under parallel-worker contention. A genuine regression (Stop
    // wired wrong, or the cancel_stream call never fires) shows up
    // as a multi-second hang; 500ms catches that with margin.
    expect(Date.now() - t0).toBeLessThan(500);
  });

  // Scroll-stick-to-bottom: when the user is already at the bottom of a
  // scrollback and tokens start streaming, the view should track. If
  // scrollTop diverges from scrollHeight we're failing this guarantee.
  test("scroll tracks bottom while streaming", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await page.locator(".input-area textarea").fill("write a lot");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    // Stream enough lines to overflow the messages container.
    const tokens = Array.from({ length: 80 }, (_, i) => `line ${i}\n`);
    await chat.api.streamTokens(start.messageId, tokens, 4);
    await chat.api.completeMessage(start.messageId, tokens.join(""));

    const distanceFromBottom = await page.evaluate(() => {
      const el = document.querySelector(".messages") as HTMLDivElement;
      if (!el) return -1;
      return el.scrollHeight - el.scrollTop - el.clientHeight;
    });

    // Allow a small slack: scrollToBottom is rAF-deferred, so the last
    // chunk may not have landed yet at the moment of measurement.
    expect(distanceFromBottom).toBeLessThan(50);
  });

  // Burst delivery: many tokens arriving back-to-back must not lock up
  // the UI thread. We measure by seeing how quickly the page can respond
  // to a synthetic click after the burst.
  test("UI stays interactive after a 500-token burst", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("burst");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    const tokens = Array.from({ length: 500 }, () => "tok ");
    await chat.api.streamTokens(start.messageId, tokens, 0);
    await chat.api.completeMessage(start.messageId, tokens.join(""));

    // After the burst, Stop button should already be gone (we completed)
    // and Send should be back, clickable, within budget.
    const send = page.locator(".send-btn");
    await expect(send).toBeVisible();
    const t0 = Date.now();
    await page.locator(".input-area textarea").fill("follow up");
    await send.click();
    expect(Date.now() - t0).toBeLessThan(500);
    await expect.poll(() => chat.api.lastStreamStart()).toMatchObject({
      // The second send should have produced a fresh stream-start.
      messageId: expect.not.stringMatching(start.messageId),
    });
  });

  // ── Latency between click and visible feedback ──────────────
  // The user clicks Send, then ChatView.handleSend awaits two Tauri
  // calls before any UI changes:
  //   1. ensureConversation()  → create_conversation (can be slow)
  //   2. sendMessageStream()   → starts the backend stream
  //
  // In the wild a cold daemon can take many seconds for those. During
  // that window the user MUST see feedback — at minimum their own
  // message bubble + a loading indicator — or the screen looks frozen.
  // A 60-second blank window after clicking a starter chip is the
  // regression we're pinning here.
  test("user sees feedback within 150ms of click even when bridge is slow", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Simulate a slow daemon: 500ms each on the two awaits inside
    // handleSend. With a fix in place, the user message bubble + a
    // loading indicator should render long before the 1s round-trip.
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler(
        "create_conversation",
        async () => {
          await new Promise((r) => setTimeout(r, 500));
          return {
            id: "conv-slow",
            title: "Slow",
            created_at: 0,
          };
        },
      );
      window.__sovereign_test__.setHandler(
        "send_message_stream",
        async () => {
          await new Promise((r) => setTimeout(r, 500));
          return { message_id: "asst-slow" };
        },
      );
    });

    const input = page.locator(".input-area textarea");
    await input.fill("ping");

    // Measure click → bubble latency INSIDE the page so Playwright's
    // IPC overhead (which can spike to 200ms+ under parallel-worker
    // load) doesn't pollute the budget. The pure UI work — handleSend
    // synchronous prelude + SEND_INITIATED dispatch + Svelte reactive
    // flush — is what matters for perceived responsiveness.
    const elapsedMs = await page.evaluate(
      () =>
        new Promise<number>((resolve) => {
          const send = document.querySelector(
            ".send-btn",
          ) as HTMLButtonElement;
          const t0 = performance.now();
          send.click();
          const tick = () => {
            const bubble = document.querySelector(".bubble.user .content");
            if (bubble && bubble.textContent === "ping") {
              resolve(performance.now() - t0);
            } else {
              requestAnimationFrame(tick);
            }
          };
          requestAnimationFrame(tick);
        }),
    );

    // 100ms is generous for in-page work (typical: <20ms). The original
    // bug measured >1000ms because the user message wasn't pushed
    // until both Tauri awaits resolved.
    expect(elapsedMs).toBeLessThan(100);

    // A loading affordance (typing dots OR Stop button) is visible
    // immediately so the user knows something is happening.
    await expect(
      page.locator(".typing-indicator, .stop-btn").first(),
    ).toBeVisible({ timeout: 300 });
  });

  // Word boundary buffering: tokens without a trailing space should NOT
  // appear mid-word. The word-buffer is what guarantees this. If a
  // future change rips it out, this test catches it.
  test("partial tokens are buffered until a word boundary", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("buffer test");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    // Three partial tokens, no trailing space — should NOT render yet.
    // The .sv-prose element is gated on `proseText` being non-empty, so
    // "not rendered" means count === 0 (the div doesn't exist).
    await chat.api.streamTokens(start.messageId, ["par", "tial", "word"], 0);
    await page.waitForTimeout(50);
    await expect(page.locator(".sv-ai-msg .sv-prose")).toHaveCount(0);

    // Trailing space flushes "partialword " in one shot.
    await chat.api.streamTokens(start.messageId, [" "], 0);
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "partialword",
    );
  });
});
