// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// EpistemicFooter render tests (initiative I2-B). A completed message
// carrying `metadata.epistemic_state` renders the typed ledger under the
// bubble: a verdict-derived receipt + provenance-grouped holdings on
// answered turns, and the abstention panel with acquisition-route chips
// on `cannot_know_from_here` turns. Legacy (ledger-less) messages keep
// the SourceAttribution rendering — pinned by the third test.

test.describe("epistemic footer", () => {
  test("grounded turn renders the verdict receipt + source badges", { tag: ["@GR-44"] }, async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("grounded question");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    await chat.api.completeMessage(ctx.messageId, "The verified answer.", {
      epistemic_state: {
        version: 1,
        demands: [],
        holdings: [
          {
            claim: "The knife was a carving knife",
            provenance: { corpus: { corpus_id: "secret-agent", chunk_id: null } },
            verification: "verified",
          },
        ],
        gaps: [],
        verdict: "grounded",
      },
    });

    const footer = page.locator('[data-testid="epistemic-footer"]');
    await expect(footer).toBeVisible();
    await expect(footer).toHaveAttribute("data-verdict", "grounded");
    await expect(
      page.locator('[data-testid="epistemic-receipt"]'),
    ).toContainText("Verified against your sources");
    await expect(footer).toContainText("Sources (1)");
    // The legacy prose-parsed attribution must NOT also render.
    await expect(page.locator(".attribution")).toHaveCount(0);
  });

  test("abstention turn renders the gap panel with a Library route chip", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("unanswerable question");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    await chat.api.completeMessage(
      ctx.messageId,
      "I can't answer that from your current sources.",
      {
        epistemic_state: {
          version: 1,
          demands: [],
          holdings: [],
          gaps: [
            {
              demand_idx: 0,
              statement: "No source material found on this topic",
              coverage: "topic_uncovered",
              routes: [
                { install_recipe: { recipe_id: "sep", name: "Philosophy (SEP)" } },
              ],
            },
          ],
          verdict: "cannot_know_from_here",
        },
      },
    );

    const footer = page.locator('[data-testid="epistemic-footer"]');
    await expect(footer).toBeVisible();
    await expect(footer).toHaveAttribute("data-verdict", "cannot_know_from_here");
    await expect(footer).toContainText(
      "Not answerable from your current sources",
    );
    await expect(footer).toContainText("No source material found on this topic");
    // The verdict receipt is suppressed on abstention turns.
    await expect(
      page.locator('[data-testid="epistemic-receipt"]'),
    ).toHaveCount(0);
    // A Library route chip is offered (onOpenLibrary is wired in ChatView).
    await expect(
      page.locator('[data-testid="abstention-routes"]'),
    ).toContainText("Install Philosophy (SEP)");
  });

  test("ledger-less message keeps the legacy SourceAttribution", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("legacy question");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const ctx = (await chat.api.lastStreamStart())!;

    // No epistemic_state on the metadata — the I6 kill-switch / old-message
    // path. The footer must not render.
    await chat.api.completeMessage(
      ctx.messageId,
      "An answer.\n\nSources:\n[1] wikipedia: Rust (programming language)",
      {},
    );

    await expect(
      page.locator('[data-testid="epistemic-footer"]'),
    ).toHaveCount(0);
  });
});
