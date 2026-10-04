// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Streaming performance regression budget.
//
// Background: until 2026-05-19 the streaming render path in
// AssistantMessage.svelte gated the prose subtree on `{#key content}`,
// which tore the entire prose div down and re-mounted it on every
// flushed word. That re-mount fired the `refine-fade-in` keyframe —
// `transform: translateY(2px) → translateY(0)` — which promotes the
// text into a GPU compositor layer in WebKitGTK / WebView2.
// Composited layers disable subpixel antialiasing, so the user saw
// fonts visibly aliasing for the duration of every streamed response.
//
// The fix routes the streaming branch through a stable
// `.prose-streaming` div (plain text, no animation) and defers the
// `.prose-content-fade` markdown branch until streaming completes.
// This spec pins both behaviours so the regression cannot reappear
// silently — DOM churn drops to zero during the stream and the
// fade-in animation is bounded to the completion swap.

test.describe("streaming render perf", () => {
  // The primary regression check: while a stream is in flight, the
  // markdown subtree (with its translateY-fade animation) must NOT
  // mount-and-remount per word. We count `.prose-content-fade`
  // mount events via MutationObserver during a 100-token burst and
  // assert the count stays at 0.
  test("prose subtree is not re-mounted per word during streaming", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Install a MutationObserver on the message column BEFORE we
    // send anything. We count every time a `.prose-content-fade`
    // node is added to the DOM. Reading the count via the test
    // hook below lets us check at any point during the stream.
    await page.evaluate(() => {
      (window as unknown as { __mountCount: number }).__mountCount = 0;
      const container = document.querySelector(".messages");
      if (!container) throw new Error("messages container missing at probe time");
      const obs = new MutationObserver((muts) => {
        for (const m of muts) {
          for (const node of Array.from(m.addedNodes)) {
            if (!(node instanceof HTMLElement)) continue;
            // Count both the node itself and any descendants that
            // match — Svelte mounts the fade div as a child of the
            // .sv-prose wrapper, so we check both directions.
            if (node.classList?.contains("prose-content-fade")) {
              (window as unknown as { __mountCount: number }).__mountCount++;
            }
            const inner = node.querySelectorAll?.(".prose-content-fade");
            if (inner) {
              (window as unknown as { __mountCount: number }).__mountCount +=
                inner.length;
            }
          }
        }
      });
      obs.observe(container, { childList: true, subtree: true });
      (window as unknown as { __mountObserver: MutationObserver }).__mountObserver = obs;
    });

    await page.locator(".input-area textarea").fill("write a long answer");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    // 100 tokens at 4ms cadence ≈ 400ms of streaming. Each token has
    // a trailing space so the word buffer flushes immediately (one
    // flush per token). This matches the cadence a fast remote
    // inference path would produce.
    const tokens = Array.from({ length: 100 }, (_, i) => `token${i} `);
    await chat.api.streamTokens(start.messageId, tokens, 4);

    // Sample the mount count WHILE streaming is still considered
    // "in flight" — before completeMessage flips isStreaming false.
    // The streaming branch must have absorbed every chunk into the
    // stable .prose-streaming div, NOT the fade-animated branch.
    const duringStreamMounts = await page.evaluate(
      () => (window as unknown as { __mountCount: number }).__mountCount,
    );

    console.log(`[perf] prose-content-fade mounts during stream: ${duringStreamMounts}`);
    expect(duringStreamMounts).toBe(0);

    // The streaming text must actually be on screen — guards
    // against the fix accidentally suppressing all rendering.
    await expect(page.locator(".sv-ai-msg .prose-streaming")).toContainText(
      "token0",
    );
    await expect(page.locator(".sv-ai-msg .prose-streaming")).toContainText(
      "token99",
    );

    // Complete the message — now the markdown branch should mount
    // exactly once as the formatted output swaps in.
    await chat.api.completeMessage(start.messageId, tokens.join(""));

    // One mount on completion is the budgeted animation. More than
    // that would mean the {#key content} block is still re-firing.
    await expect.poll(
      async () =>
        page.evaluate(
          () => (window as unknown as { __mountCount: number }).__mountCount,
        ),
      { timeout: 2000 },
    ).toBeGreaterThan(0);

    const finalMounts = await page.evaluate(
      () => (window as unknown as { __mountCount: number }).__mountCount,
    );
    expect(finalMounts).toBeLessThanOrEqual(2); // allow one for streaming-end + one slack

    // And the formatted markdown branch is now on screen.
    await expect(
      page.locator(".sv-ai-msg .prose-content-fade"),
    ).toBeVisible();
  });

  // Parse-coalesce regression check. The component should not run
  // `parseAssistantContent` once per chunk — it's O(n) per call and
  // would dominate the renderer thread under fast token cadence.
  // The rAF-coalesce flush means the .prose-streaming text content
  // updates at most once per frame (~60Hz). For a 100-token, 4ms
  // burst (~400ms) we expect at most ~25 text-content mutations,
  // not 100. We allow generous slack to absorb harness timing
  // jitter — the diagnostic value is in catching a 10× regression,
  // not in millisecond precision.
  test("parse pipeline is rAF-coalesced (≤1 update per frame)", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Per-frame text-content mutation counter. We count
    // characterData and childList mutations on .messages because
    // the streaming text lives inside a Svelte template expression
    // (`{proseText}`) that updates as a text node.
    await page.evaluate(() => {
      (window as unknown as { __textMutations: number }).__textMutations = 0;
      const container = document.querySelector(".messages");
      if (!container) throw new Error("messages container missing");
      const obs = new MutationObserver((muts) => {
        for (const m of muts) {
          // Only count mutations that affected a .prose-streaming
          // subtree — other parts of the DOM (typing dots, etc.)
          // can mutate too and would inflate the count.
          const target = m.target as HTMLElement;
          const inProse =
            target.closest?.(".prose-streaming") ||
            (target.parentElement && target.parentElement.closest(".prose-streaming"));
          if (inProse) {
            (window as unknown as { __textMutations: number })
              .__textMutations++;
          }
        }
      });
      obs.observe(container, {
        childList: true,
        characterData: true,
        subtree: true,
      });
    });

    await page.locator(".input-area textarea").fill("burst");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    const tokens = Array.from({ length: 100 }, (_, i) => `t${i} `);
    await chat.api.streamTokens(start.messageId, tokens, 4);

    // Allow one final rAF tick after the last chunk so the
    // coalesce flushes the trailing update.
    await page.evaluate(
      () =>
        new Promise<void>((resolve) => {
          requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
        }),
    );

    const muts = await page.evaluate(
      () =>
        (window as unknown as { __textMutations: number }).__textMutations,
    );

    console.log(`[perf] prose-streaming text-mutations during 100-token burst: ${muts}`);
    // 60Hz over ~400ms is ~24 frames. The harness setTimeout(0)
    // micro-yields between tokens often pack multiple tokens per
    // frame, so the realistic ceiling is well under 100.
    // 75 is the regression-detection budget: comfortably above the
    // observed 20-30 with coalescing (and the ~60 seen on a loaded
    // CI box, where rAF coalescing degrades), yet well below the ~100
    // (one mutation per token) we'd see WITHOUT coalescing. The point
    // is catching that ~10x regression, not millisecond precision — a
    // budget that sat right at the loaded value (60) flaked on load.
    expect(muts).toBeLessThan(75);
    expect(muts).toBeGreaterThan(0); // sanity: streaming did update
  });
});
