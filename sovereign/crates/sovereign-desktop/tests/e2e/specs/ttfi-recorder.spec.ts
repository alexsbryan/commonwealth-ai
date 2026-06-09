// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// TTFI recorder round-trip: drive a known event timeline through the
// chat UI, then verify the recorder captured a Scenario-shaped log
// that the harness could replay. This is the contract that lets us
// trust scenarios harvested from real usage.
//
// The recorder is a production module gated behind `?ttfi=record` or
// `localStorage.ttfi_record`. We activate it via addInitScript so it
// boots alongside the app, exactly like a user would.

declare global {
  interface Window {
    __ttfi_recorder__?: {
      status: "inactive" | "idle" | "recording" | "finalized";
      events: unknown[];
      query: string;
      enable(): void;
      reset(): void;
      exportScenario(name: string): {
        name: string;
        description: string;
        query: string;
        events: { kind: string; atMs: number; [k: string]: unknown }[];
        terminal: { kind: string; selector?: string };
      };
      exportScenarioTs(name: string): string;
    };
  }
}

test.describe("TTFI recorder round-trip", () => {
  test.beforeEach(async ({ sovereignPage: page }) => {
    // Pre-activate the recorder before app code runs. Equivalent to a
    // user navigating to ?ttfi=record but doesn't pollute the URL.
    await page.addInitScript(() => {
      window.localStorage.setItem("ttfi_record", "1");
    });
  });

  test("captures a streaming turn end-to-end and exports a replayable scenario", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Sanity: recorder loaded and is in idle (active, awaiting click).
    const initialStatus = await page.evaluate(
      () => window.__ttfi_recorder__?.status,
    );
    expect(initialStatus).toBe("idle");

    await page.locator(".input-area textarea").fill("recorded query");
    await page.locator(".send-btn").click();

    // After click, status flips to recording.
    await expect
      .poll(() => page.evaluate(() => window.__ttfi_recorder__?.status), {
        timeout: 2_000,
      })
      .toBe("recording");

    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    // Drive a small timeline through the shim. Real-time deltas (not
    // mocked clocks) so the recorder sees realistic atMs values.
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "rec-session",
        conversation_id: cid,
        event: {
          phase: "routing_committed",
          text: "Recorded — choosing path.",
          elapsed_ms: 0,
        },
      });
    }, start.conversationId);
    await page.waitForTimeout(120);

    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "rec-session",
        conversation_id: cid,
        event: {
          phase: "retrieval_complete",
          text: "Recorded — found sources.",
          elapsed_ms: 120,
        },
      });
    }, start.conversationId);
    await page.waitForTimeout(80);

    await chat.api.streamTokens(start.messageId, ["hello ", "from ", "rec "], 0);
    await chat.api.completeMessage(start.messageId, "hello from rec");

    // Status should finalize on message-complete.
    await expect
      .poll(() => page.evaluate(() => window.__ttfi_recorder__?.status), {
        timeout: 2_000,
      })
      .toBe("finalized");

    const captured = await page.evaluate(
      () => window.__ttfi_recorder__?.exportScenario("recorded-turn"),
    );
    expect(captured).toBeTruthy();
    expect(captured!.query).toBe("recorded query");
    expect(captured!.terminal.kind).toBe("send-btn-visible");

    // Order matters: two narrations, then chunks, then complete.
    const kinds = captured!.events.map((e) => e.kind);
    expect(kinds[0]).toBe("narration");
    expect(kinds[1]).toBe("narration");
    expect(kinds.includes("chunk")).toBe(true);
    expect(kinds[kinds.length - 1]).toBe("complete");

    // atMs is monotonically non-decreasing across the captured events.
    const ats = captured!.events.map((e) => e.atMs);
    for (let i = 1; i < ats.length; i++) {
      expect(ats[i]).toBeGreaterThanOrEqual(ats[i - 1] - 1);
    }

    // The second narration was sent ~120ms after the first; assert the
    // recorder caught a meaningfully non-zero delta (not necessarily
    // exactly 120 — setTimeout / IPC jitter applies).
    expect(ats[1] - ats[0]).toBeGreaterThan(50);

    // The exported .ts module string must contain a `Scenario` export,
    // the literal events, and the captured query — anyone could drop
    // it into tests/e2e/scenarios/ and the harness would replay it.
    const ts = await page.evaluate(
      () => window.__ttfi_recorder__?.exportScenarioTs("recorded_turn"),
    );
    expect(ts).toContain('Scenario =');
    expect(ts).toContain('"recorded query"');
    expect(ts).toContain('"routing_committed"');
    expect(ts).toContain('"hello "');
  });

  test("captures clarification turns and marks them as terminal", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await page.locator(".input-area textarea").fill("ambiguous");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("clarification-request", {
        session_id: "rec-session",
        conversation_id: cid,
        question: "Which one did you mean?",
        options: [
          { label: "Frankfurt", follow_up: "tell me about frankfurt", intent_hint: "deep_query" },
          { label: "Strawson", follow_up: "tell me about strawson", intent_hint: "deep_query" },
        ],
      });
    }, start.conversationId);

    // Recorder finalises on clarification — that's the terminal state
    // for synthesis-suppressed turns.
    await expect
      .poll(() => page.evaluate(() => window.__ttfi_recorder__?.status), {
        timeout: 2_000,
      })
      .toBe("finalized");

    const captured = await page.evaluate(
      () => window.__ttfi_recorder__?.exportScenario("ambiguous-turn"),
    );
    expect(captured!.terminal.kind).toBe("selector-visible");
    if (captured!.terminal.kind === "selector-visible") {
      expect(captured!.terminal.selector).toBe(".clarification-card");
    }
    expect(captured!.events[captured!.events.length - 1].kind).toBe(
      "clarification",
    );
  });

  test("ignores empty submits — t0 anchors only on real turns", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Click Send with empty input — chat surface guards against this
    // (Send is disabled until non-whitespace text exists), but a stray
    // .send-btn somewhere shouldn't false-anchor the recorder either.
    // We force a click via dispatchEvent because the disabled button
    // wouldn't accept a normal click.
    await page.evaluate(() => {
      const btn = document.querySelector(".send-btn") as HTMLButtonElement;
      btn?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await page.waitForTimeout(100);

    const status = await page.evaluate(
      () => window.__ttfi_recorder__?.status,
    );
    expect(status).toBe("idle"); // never advanced to recording

    // Now do a real turn: status should advance to recording.
    await page.locator(".input-area textarea").fill("real query");
    await page.locator(".send-btn").click();
    await expect
      .poll(() => page.evaluate(() => window.__ttfi_recorder__?.status), {
        timeout: 2_000,
      })
      .toBe("recording");
  });
});
