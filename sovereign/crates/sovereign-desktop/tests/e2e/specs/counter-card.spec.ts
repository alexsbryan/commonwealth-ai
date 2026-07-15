// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Verification-counter regression tests. A grounded turn holds every
// token behind the gate for ~a minute; the CounterCard reframes that
// hold as three stations (Gather → Draft → Check) with per-claim
// stamps. These pin the load-bearing UX invariants:
//
//   1. Struct-form retrieval_complete activates the counter and
//      suppresses the promoted narration line + chip stack (one calm
//      surface, not three renditions of the same progress).
//   2. synthesis_progress heartbeats drive the Draft station ticker.
//   3. claim_check_start/claim_verdict stamp rows in place;
//      claim_revision_start takes the headline.
//   4. message-complete unmounts the counter with the loading slot.
//   5. String-form narration (tool turns, doc ops — the flows
//      chat-placeholder.spec.ts pins) never wakes the counter.

test.describe("verification counter", () => {
  test("gather → draft → check stations follow the frames", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("grounded question");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    // Gather: struct retrieval_complete wakes the counter with counts
    // + the user's own passage titles.
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: {
            retrieval_complete: {
              chunks_in: 14,
              corpora: ["project-notes", "meetings"],
              top_titles: ["Pipeline convergence — Phase 2", "Standup 06-09"],
            },
          },
          text: "Read 14 chunks across 2 sources.",
          elapsed_ms: 6_000,
        },
      });
    }, ctx.conversationId);

    const card = page.locator('[data-testid="counter-card"]');
    await expect(card).toBeVisible();
    // Retrieval complete + no heartbeat yet = Draft station warming up.
    await expect(card).toHaveAttribute("data-station", "draft");
    await expect(card).toContainText("Warming up the primary model");
    await expect(card).toContainText("drafting from 14 passages");

    // While the counter is active, the promoted narration line and the
    // chip stack must be suppressed.
    await expect(
      page.locator('.doc-progress-indicator[data-source="narration"]'),
    ).toHaveCount(0);
    await expect(page.locator('[data-testid="narration-stack"]')).toHaveCount(
      0,
    );

    // Draft: heartbeat ticks the held-token counter.
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: { synthesis_progress: { tokens: 142 } },
          text: "",
          elapsed_ms: 20_000,
        },
      });
    }, ctx.conversationId);
    await expect(card).toContainText("142");
    await expect(card).toContainText("tokens");

    // Check: the claim list opens and rows stamp as verdicts land.
    await page.evaluate((cid) => {
      const emit = (phase: unknown, text: string, elapsed_ms: number) =>
        window.__sovereign_test__.emit("turn-narration", {
          session_id: "s1",
          conversation_id: cid,
          event: { phase, text, elapsed_ms },
        });
      emit(
        {
          claim_check_start: {
            claims: [
              "The pipelines merged into one shared core",
              "The merge shipped on June 12",
            ],
            recheck: false,
          },
        },
        "Checking 2 claims against your sources.",
        60_000,
      );
      emit({ claim_verdict: { index: 0, supported: true } }, "", 64_000);
      emit({ claim_verdict: { index: 1, supported: false } }, "", 68_000);
    }, ctx.conversationId);

    await expect(card).toHaveAttribute("data-station", "check");
    await expect(card).toContainText("Checking 2 claims against your sources");
    const claims = page.locator('[data-testid="counter-claims"] .claim');
    await expect(claims).toHaveCount(2);
    await expect(claims.nth(0)).toHaveClass(/supported/);
    await expect(claims.nth(1)).toHaveClass(/unsupported/);
    await expect(card).toContainText("1 of 2 confirmed");

    // Revision: the amber headline takes over.
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: { claim_revision_start: { failed: 1 } },
          text: "Couldn't confirm 1 — revising from the sources.",
          elapsed_ms: 70_000,
        },
      });
    }, ctx.conversationId);
    await expect(card).toContainText("revising from the sources");

    // Serve: completion unmounts the counter, the RECEIPT persists on
    // the bubble, and the chip stack must NOT pop back in above the
    // freshly served answer.
    await chat.api.completeMessage(ctx.messageId, "The verified answer.", {
      grounding_gate: {
        action: "rewrite_released",
        retried: true,
        mode: "per_claim",
        claims_checked: 2,
        failed_claims: [],
      },
    });
    await expect(card).toHaveCount(0);
    const receipt = page.locator('[data-testid="verification-receipt"]');
    await expect(receipt).toBeVisible();
    await expect(receipt).toContainText("Verified against your sources");
    await expect(receipt).toContainText("2 claims checked");
    await expect(receipt).toContainText("revised once");
    await expect(page.locator('[data-testid="narration-stack"]')).toHaveCount(
      0,
    );
  });

  test("ungated turn: counter yields once tokens stream with no gate signal", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("ungated question");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    // Retrieval frames fire on ungated turns too — the counter may
    // provisionally appear…
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: {
            retrieval_complete: { chunks_in: 6, corpora: ["sep"] },
          },
          text: "Read 6 chunks across 1 source.",
          elapsed_ms: 2_000,
        },
      });
    }, ctx.conversationId);
    await expect(page.locator('[data-testid="counter-card"]')).toBeVisible();

    // …but the moment tokens stream live (no heartbeat, no claim
    // frames), this is an ungated turn — the counter yields to the
    // legacy narration line instead of sitting on "warming up".
    await page.evaluate((mid) => {
      window.__sovereign_test__.emit("message-chunk", {
        message_id: mid,
        chunk: "Tokens are streaming live. ",
      });
    }, ctx.messageId);
    await expect(page.locator('[data-testid="counter-card"]')).toHaveCount(0);
    await expect(
      page.locator('.doc-progress-indicator[data-source="narration"]'),
    ).toBeVisible();
  });

  test("gate frames outrank the document-progress line (attached-doc consolidation)", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("doc-flavoured turn");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    // Early doc phases narrate via the doc-progress line…
    await page.evaluate(() => {
      window.__sovereign_test__.emit("document:operation", {
        type: "Retrieving",
      });
    });
    await expect(page.locator(".doc-progress-indicator")).toBeVisible();
    await expect(page.locator(".doc-progress-indicator")).toContainText(
      "Retrieving relevant passages",
    );

    // …but the moment the gate opens the claim check, the counter owns
    // the wait — on every gated surface alike.
    await page.evaluate((cid) => {
      window.__sovereign_test__.emit("turn-narration", {
        session_id: "s1",
        conversation_id: cid,
        event: {
          phase: { claim_check_start: { claims: [], recheck: false } },
          text: "Reading the draft back against your sources.",
          elapsed_ms: 30_000,
        },
      });
    }, ctx.conversationId);
    await expect(page.locator('[data-testid="counter-card"]')).toBeVisible();
    await expect(
      page.locator('[data-testid="counter-card"]'),
    ).toHaveAttribute("data-station", "check");
    await expect(page.locator(".doc-progress-indicator")).toHaveCount(0);
  });

  test("string-form narration alone never wakes the counter", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("tool-ish turn");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

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

    // The legacy indicators own this flow; no counter card.
    await expect(
      page.locator('.doc-progress-indicator[data-source="narration"]'),
    ).toBeVisible();
    await expect(page.locator('[data-testid="counter-card"]')).toHaveCount(0);
  });
});
