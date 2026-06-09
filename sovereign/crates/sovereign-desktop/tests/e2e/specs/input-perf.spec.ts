// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Chat input typing latency.
//
// The user reported that the input row "isn't 60fps" while typing.
// To investigate before guessing at fixes, we measure the actual
// time per simulated keystroke. The harness drives the textarea
// via dispatched events (avoids Playwright's keyboard IPC overhead
// which would dominate the budget at ~5-10ms per stroke) and
// reads the elapsed wall-clock and the per-frame time of input
// events.
//
// We sample TWO contexts:
//   1. Empty chat — no messages, no streaming state.
//   2. Loaded chat — 20 prior messages + ongoing-stream placeholder.
//
// A 60fps budget allows ~16.6ms per frame. The numbers below are
// generous because Playwright's evaluate() bridge itself adds
// 3-5ms of overhead per page.evaluate roundtrip — the goal is to
// detect a multi-x regression, not to assert millisecond precision.

test.describe("input typing perf", () => {
  test("typing latency on empty chat", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    const result = await page.evaluate(async () => {
      const ta = document.querySelector(
        ".input-area textarea",
      ) as HTMLTextAreaElement;
      ta.focus();
      ta.value = "";
      const N = 60;
      const perKeystroke: number[] = [];
      // Dispatch synthetic input events. Svelte's bind:value
      // listens to the `input` event, so we use the same channel
      // the real DOM uses. value mutated immediately before the
      // dispatch mirrors what the browser does on a key press.
      for (let i = 0; i < N; i++) {
        const t0 = performance.now();
        ta.value = ta.value + "x";
        ta.dispatchEvent(new Event("input", { bubbles: true }));
        // Wait for Svelte's reactive flush to complete. A double-
        // microtask is sufficient since Svelte schedules updates
        // on the microtask queue. We do not wait for a paint
        // because the user's "typing feels janky" perception is
        // tied to input → bind reflection, not paint commit.
        await new Promise<void>((r) => queueMicrotask(() => r()));
        await new Promise<void>((r) => queueMicrotask(() => r()));
        perKeystroke.push(performance.now() - t0);
      }
      const sorted = [...perKeystroke].sort((a, b) => a - b);
      const median = sorted[Math.floor(sorted.length / 2)];
      const p95 = sorted[Math.floor(sorted.length * 0.95)];
      const max = sorted[sorted.length - 1];
      const avg = perKeystroke.reduce((a, b) => a + b, 0) / perKeystroke.length;
      return { median, p95, max, avg, samples: perKeystroke.length };
    });

    console.log(
      `[input-perf empty] median=${result.median.toFixed(2)}ms p95=${result.p95.toFixed(2)}ms max=${result.max.toFixed(2)}ms avg=${result.avg.toFixed(2)}ms (n=${result.samples})`,
    );

    // 60fps budget per keystroke is 16.6ms. We give 33ms for
    // median (allow one missed frame), 50ms for p95. Failing
    // these is a real perf regression.
    expect(result.median).toBeLessThan(33);
    expect(result.p95).toBeLessThan(50);
  });

  // Measure paint-to-paint cost: how long from a keystroke until the
  // browser actually commits a frame. This catches paint/composite
  // costs the previous test misses (it only measures the JS-side
  // reactive flush). A 60fps target is one frame ≈ 16.6ms; we allow
  // 32ms to account for harness overhead.
  test("paint pipeline keeps up at 60fps under sustained typing", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    const result = await page.evaluate(async () => {
      const ta = document.querySelector(
        ".input-area textarea",
      ) as HTMLTextAreaElement;
      ta.focus();
      ta.value = "";

      // Sample 60 keystrokes, waiting for one full rAF between each.
      // Each rAF-bracketed window is the actual time from "keystroke
      // happened" to "browser committed the next frame."
      const N = 60;
      const frameTimes: number[] = [];
      for (let i = 0; i < N; i++) {
        await new Promise<void>((r) => requestAnimationFrame(() => r()));
        const t0 = performance.now();
        ta.value = ta.value + "x";
        ta.dispatchEvent(new Event("input", { bubbles: true }));
        await new Promise<void>((r) => requestAnimationFrame(() => r()));
        frameTimes.push(performance.now() - t0);
      }
      const sorted = [...frameTimes].sort((a, b) => a - b);
      return {
        median: sorted[Math.floor(sorted.length / 2)],
        p95: sorted[Math.floor(sorted.length * 0.95)],
        max: sorted[sorted.length - 1],
        avg: frameTimes.reduce((a, b) => a + b, 0) / frameTimes.length,
        samples: frameTimes.length,
        // Also expose: how many keystrokes took longer than 1 frame
        // (16.6ms). On a healthy 60fps surface this is 0.
        droppedFrames: frameTimes.filter((t) => t > 16.6).length,
      };
    });

    console.log(
      `[input-paint] median=${result.median.toFixed(2)}ms p95=${result.p95.toFixed(2)}ms max=${result.max.toFixed(2)}ms avg=${result.avg.toFixed(2)}ms dropped=${result.droppedFrames}/${result.samples}`,
    );

    // Calibration: each sample brackets a full rAF, so even a perfect
    // pipeline measures ~16.7ms (one vsync quantum) — isolated, median
    // lands at 16.7ms. The suite runs `fullyParallel` (6 Chromium
    // workers contend for the CPU), which adds ~5ms of scheduling
    // jitter. Thresholds sit at ~2/~3 frames so the gate survives that
    // contention yet still fails a genuine regression (a synchronous
    // reflow per keystroke would push median past one extra frame of
    // real work). `droppedFrames` stays logged-only — it's ~always high
    // here because every sample includes the rAF quantum by design.
    expect(result.median).toBeLessThan(33);
    expect(result.p95).toBeLessThan(50);
  });

  test("typing latency with 20 prior messages + streaming placeholder", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Seed 20 messages by directly driving the chat machine through
    // the chunk/complete event pair. This avoids the per-message
    // 500ms+ round trips of an actual conversation.
    for (let i = 0; i < 10; i++) {
      await page.locator(".input-area textarea").fill(`question ${i}`);
      await page.locator(".send-btn").click();
      await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
      const start = (await chat.api.lastStreamStart())!;
      await chat.api.streamTokens(
        start.messageId,
        Array.from({ length: 12 }, (_, j) => `word${j} `),
        0,
      );
      await chat.api.completeMessage(
        start.messageId,
        Array.from({ length: 12 }, (_, j) => `word${j} `).join(""),
      );
      // Reset lastStreamStart so the next poll observes the NEXT send.
      await page.evaluate(() => {
        const api = window.__sovereign_test__ as unknown as {
          _lastStreamStart: unknown;
        };
        api._lastStreamStart = null;
      });
    }

    // Sanity: bubble count present.
    const bubbleCount = await page.locator(".bubble.user").count();
    expect(bubbleCount).toBe(10);

    // Now start a NEW turn but never complete — the placeholder
    // assistant bubble + isStreaming state stay live while we
    // type the next question.
    await page.locator(".input-area textarea").fill("ongoing");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ongoing = (await chat.api.lastStreamStart())!;
    await chat.api.streamTokens(
      ongoing.messageId,
      Array.from({ length: 80 }, (_, j) => `t${j} `),
      0,
    );
    // Don't complete — leave the streaming bubble alive.

    // Now measure typing latency. The user reports jank in this
    // scenario specifically (typing while previous answer is
    // visible / streaming).
    const result = await page.evaluate(async () => {
      const ta = document.querySelector(
        ".input-area textarea",
      ) as HTMLTextAreaElement;
      ta.focus();
      ta.value = "";
      const N = 60;
      const perKeystroke: number[] = [];
      for (let i = 0; i < N; i++) {
        const t0 = performance.now();
        ta.value = ta.value + "x";
        ta.dispatchEvent(new Event("input", { bubbles: true }));
        await new Promise<void>((r) => queueMicrotask(() => r()));
        await new Promise<void>((r) => queueMicrotask(() => r()));
        perKeystroke.push(performance.now() - t0);
      }
      const sorted = [...perKeystroke].sort((a, b) => a - b);
      const median = sorted[Math.floor(sorted.length / 2)];
      const p95 = sorted[Math.floor(sorted.length * 0.95)];
      const max = sorted[sorted.length - 1];
      const avg = perKeystroke.reduce((a, b) => a + b, 0) / perKeystroke.length;
      return { median, p95, max, avg, samples: perKeystroke.length };
    });

    console.log(
      `[input-perf loaded] median=${result.median.toFixed(2)}ms p95=${result.p95.toFixed(2)}ms max=${result.max.toFixed(2)}ms avg=${result.avg.toFixed(2)}ms (n=${result.samples})`,
    );

    // Loaded budget — same as empty. If the loaded budget is
    // dramatically higher than empty, something in the message
    // list is reacting to inputText changes (which would be a
    // reactivity bug, not a workload bug).
    expect(result.median).toBeLessThan(33);
    expect(result.p95).toBeLessThan(50);
  });
});
