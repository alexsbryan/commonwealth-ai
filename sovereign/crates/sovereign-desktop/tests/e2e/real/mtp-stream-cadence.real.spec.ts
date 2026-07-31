// SPDX-License-Identifier: AGPL-3.0-or-later
// MTP-on-streaming live check. Run with the chat slot pointed at an
// MTP-capable gguf (SOVEREIGN_REAL_CHAT_MODEL=…-MTP-….gguf) — the
// managed default is a dense 2B, which exercises the same code path
// but never dispatches MTP, so the cadence recorded here is only
// interesting under an MTP slot.
//
// What this proves that the cadence-tuned journeys can't: MTP emits at
// piece-commit sites (verify bursts), so chunks reach the UI in clumps
// with near-zero intra-burst gaps. The specs tuned to single-token
// cadence would misread that as a fault; this spec instead asserts the
// things that must hold under ANY cadence — stream integrity, rendered
// tail, auto-scroll, prompt cancel — and RECORDS the arrival rhythm to
// test-artifacts/mtp-cadence.json for the human verdict.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assertTurnInvariants, sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ARTIFACTS = path.resolve(__dirname, "../../../test-artifacts");

/** Stamp every captured SSE row with its page-arrival time. Registered
 *  before goto; init scripts run in registration order, so the shim's
 *  __sovereign_real__ exists by the time this executes. */
async function stampArrivalTimes(page: import("@playwright/test").Page) {
  await page.addInitScript(() => {
    const api = (window as unknown as { __sovereign_real__?: { captured: unknown[] } })
      .__sovereign_real__;
    if (!api) return;
    const arr = api.captured as Array<Record<string, unknown>>;
    const orig = arr.push.bind(arr);
    arr.push = (...rows: Array<Record<string, unknown>>) => {
      for (const r of rows) r.t = performance.now();
      return orig(...rows);
    };
  });
}

interface ChunkRow {
  t: number;
  len: number;
}

async function chunkRowsFor(
  page: import("@playwright/test").Page,
  messageId: string,
): Promise<ChunkRow[]> {
  return page.evaluate(
    (mid) =>
      (
        window as unknown as {
          __sovereign_real__: {
            captured: Array<{
              event: string;
              t?: number;
              payload?: { message_id?: string; chunk?: string };
            }>;
          };
        }
      ).__sovereign_real__.captured
        .filter((r) => r.event === "message-chunk" && r.payload?.message_id === mid)
        .map((r) => ({ t: r.t ?? 0, len: (r.payload?.chunk ?? "").length })),
    messageId,
  );
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const idx = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
  return sorted[idx];
}

/** Collapse to lowercase alphanumerics so raw model text and its
 *  markdown-rendered DOM projection compare stably. */
function norm(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "");
}

test("streamed turn: integrity + rendering + scroll hold under MTP cadence; rhythm recorded", async ({
  sovereignPage: page,
  bridge,
}) => {
  await stampArrivalTimes(page);
  await realBootToChat(page);
  await page.locator(".new-btn").click();

  // Generative prose: no tools, no forced choice, and — unlike
  // DeepQuery synthesis, which delivers its whole answer as ONE chunk
  // after silent server-side assembly (observed live 2026-07-30:
  // 33.6s inert, then a single 1.4k-char chunk) — the generative
  // stream path emits token-wise, which is what a cadence measurement
  // needs. First turn pays the cold slot load, hence the window.
  const messageId = await sendAndAwaitTurn(
    page,
    "Write a vivid story, around 300 words, about a lighthouse keeper's hardest night at sea.",
    { timeoutMs: 170_000 },
  );

  // Cadence first, asserts after — a failed invariant must still leave
  // the rhythm data on disk for the human verdict.
  const rows = await chunkRowsFor(page, messageId);
  const gaps = rows.slice(1).map((r, i) => r.t - rows[i].t);
  const sorted = [...gaps].sort((a, b) => a - b);
  // A "burst" = consecutive arrivals ≤4ms apart (same verify batch
  // flushing through the channel; single-token decode on any real
  // model is 5-40ms/token, far above this line).
  let maxBurst = 1;
  let cur = 1;
  for (const g of gaps) {
    cur = g <= 4 ? cur + 1 : 1;
    if (cur > maxBurst) maxBurst = cur;
  }
  const totalChars = rows.reduce((n, r) => n + r.len, 0);
  const streamMs = rows.length > 1 ? rows[rows.length - 1].t - rows[0].t : 0;
  const cadence = {
    model: process.env.SOVEREIGN_REAL_CHAT_MODEL ?? "(managed default)",
    message_id: messageId,
    chunk_events: rows.length,
    total_chars: totalChars,
    stream_ms: Math.round(streamMs),
    chars_per_s: streamMs > 0 ? Math.round((totalChars / streamMs) * 1000) : null,
    mean_chunk_chars: Math.round(totalChars / Math.max(1, rows.length)),
    gap_ms: {
      p50: Math.round(percentile(sorted, 50) * 10) / 10,
      p95: Math.round(percentile(sorted, 95) * 10) / 10,
      max: Math.round((sorted[sorted.length - 1] ?? 0) * 10) / 10,
    },
    max_burst_run: maxBurst,
    gaps_under_4ms: gaps.filter((g) => g <= 4).length,
    gaps_over_20ms: gaps.filter((g) => g > 20).length,
  };
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  fs.writeFileSync(path.join(ARTIFACTS, "mtp-cadence.json"), JSON.stringify(cadence, null, 2));
  console.log(`[mtp-cadence] ${JSON.stringify(cadence)}`);

  const facts = await assertTurnInvariants(page, bridge, messageId);
  expect(facts.chunkCount, "a real streamed turn delivers multiple chunks").toBeGreaterThan(3);

  // ── Rendered tail: the word buffer flushed everything. The DOM is a
  // markdown projection of full_text, so compare normalized tails
  // rather than bytes (byte-exactness is already asserted at the event
  // layer by assertTurnInvariants). A tail stuck in WordBufferedStream
  // is exactly what this would catch. ──
  const bubbleText = await page
    .locator(".sv-ai-msg .sv-prose")
    .last()
    .innerText();
  const tail = norm(facts.complete.full_text.slice(-80));
  if (tail.length > 0) {
    expect(
      norm(bubbleText),
      "rendered assistant bubble must contain the final words of full_text — a missing tail means the word buffer never flushed",
    ).toContain(tail);
  }

  // ── Auto-scroll: after a streamed turn the scroller sits at (or
  // within a bubble's margin of) the bottom. ──
  const scroll = await page
    .locator(".chat-view .messages")
    .evaluate((el) => ({
      top: el.scrollTop,
      height: el.scrollHeight,
      client: el.clientHeight,
    }));
  expect(
    scroll.height - (scroll.top + scroll.client),
    `scroller should rest near the bottom after streaming (scrollHeight=${scroll.height} scrollTop=${scroll.top} clientHeight=${scroll.client})`,
  ).toBeLessThan(160);

  await page.screenshot({ path: path.join(ARTIFACTS, "mtp-stream-final.png") });
});

test("stop mid-stream on an MTP slot cancels cleanly and the session recovers", async ({
  sovereignPage: page,
  bridge,
}) => {
  await stampArrivalTimes(page);
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

  // Wait until tokens are genuinely flowing. Generous window: the slot
  // is warm after test 1, but MTP prefill + retrieval can still stack.
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            window.__sovereign_real__.captured.filter((r) => r.event === "message-chunk")
              .length,
        ),
      { timeout: 150_000, intervals: [250, 500, 1000] },
    )
    .toBeGreaterThan(3);
  await page.screenshot({ path: path.join(ARTIFACTS, "mtp-stream-midstream.png") });
  const stoppedAt = Date.now();
  await page.locator(".stop-btn").click();

  // Under MTP the cancel granularity is a draft piece (a few tokens),
  // not a single token — still far inside this window.
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            window.__sovereign_real__.captured.filter(
              (r) => r.event === "message-complete",
            ).length,
        ),
      { timeout: 45_000, intervals: [250, 500, 1000] },
    )
    .toBeGreaterThan(before);
  const cancelLatencyMs = Date.now() - stoppedAt;
  console.log(`[mtp-cadence] cancel→complete latency: ${cancelLatencyMs}ms`);

  const cancelledId = await page.evaluate(() => {
    const completes = window.__sovereign_real__.captured.filter(
      (r) => r.event === "message-complete",
    );
    return (completes[completes.length - 1].payload as { message_id: string }).message_id;
  });
  await assertTurnInvariants(page, bridge, cancelledId, { expectFinish: "cancelled" });

  // No stragglers: chunk flow for the cancelled message stops.
  const countNow = await page.evaluate(
    (mid) =>
      window.__sovereign_real__.captured.filter(
        (r) =>
          r.event === "message-chunk" &&
          (r.payload as { message_id?: string })?.message_id === mid,
      ).length,
    cancelledId,
  );
  await page.waitForTimeout(3000);
  const countLater = await page.evaluate(
    (mid) =>
      window.__sovereign_real__.captured.filter(
        (r) =>
          r.event === "message-chunk" &&
          (r.payload as { message_id?: string })?.message_id === mid,
      ).length,
    cancelledId,
  );
  expect(countLater, "no chunks may arrive after message-complete").toBe(countNow);

  // Recovery: a fresh turn in the same conversation completes.
  const nextId = await sendAndAwaitTurn(page, "Reply with the single word: recovered", {
    timeoutMs: 150_000,
  });
  const facts = await assertTurnInvariants(page, bridge, nextId);
  expect(facts.complete.full_text.length).toBeGreaterThan(0);
});
