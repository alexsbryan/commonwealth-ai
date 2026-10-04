// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";
import type { Page } from "@playwright/test";

// The hover ✕ on a conversation row is a two-click affordance: the first
// click ARMS (✕ → ✓, 3s window), the second CONFIRMS. Nothing else in the
// app deletes user content with one click and no undo, and the button sits
// under the cursor's path along the whole row — a mis-click is not a rare
// event, it is the expected failure.
//
// This spec exists because the arm/confirm was measured to be undefended:
// `sabotage-bank.mjs` inverted the arm test (`pendingDeleteId === id` →
// `!== id`, which deletes on the FIRST click) and the entire desktop gate —
// svelte-check, 370 vitest tests, 267 Playwright tests — stayed green. It
// was tracked as `hole-delete-confirm-inverted`.
//
// So the load-bearing assertion here is the negative one: after ONE click
// the row is still present and `delete_conversation` has not been called.
// Asserting only that two clicks delete would pass under the inversion.

const CONVERSATIONS = [
  { id: "conv-alpha", title: "Conversation Alpha", created_at: 1, updated_at: 1 },
  { id: "conv-bravo", title: "Conversation Bravo", created_at: 2, updated_at: 2 },
];

/** Seed the sidebar list and record every `delete_conversation` call.
 *
 *  The backing array is mutable and `delete_conversation` splices from it,
 *  so a `conversations:changed` re-list cannot resurrect a deleted row and
 *  quietly turn a real deletion into an apparent no-op. */
async function seedConversations(page: Page, list: unknown[]): Promise<void> {
  await page.addInitScript((seed) => {
    const w = window as unknown as {
      __sovereign_test__?: {
        setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
      };
      __deleteCalls__?: string[];
    };
    w.__deleteCalls__ = [];
    const live = JSON.parse(JSON.stringify(seed)) as { id: string }[];
    const wait = setInterval(() => {
      if (!w.__sovereign_test__) return;
      clearInterval(wait);
      w.__sovereign_test__.setHandler("list_conversations", () => live.slice());
      w.__sovereign_test__.setHandler("delete_conversation", (args) => {
        const { conversationId } = args as { conversationId: string };
        w.__deleteCalls__!.push(conversationId);
        const i = live.findIndex((c) => c.id === conversationId);
        if (i >= 0) live.splice(i, 1);
        return undefined;
      });
    }, 1);
  }, list);
}

const deleteCalls = (page: Page): Promise<string[]> =>
  page.evaluate(
    () => (window as unknown as { __deleteCalls__?: string[] }).__deleteCalls__ ?? [],
  );

const row = (page: Page, id: string) =>
  page.locator(`.convo-item[data-conversation-id="${id}"]`);

test.describe("conversation delete — two-click confirm", () => {
  test("one click arms and deletes nothing; the second confirms", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedConversations(page, CONVERSATIONS);
    await bootToChat(page, chat);

    const alpha = row(page, "conv-alpha");
    await expect(alpha).toBeVisible();
    const del = alpha.locator(".delete-btn");

    // Idle: the ✕ is hover-revealed (opacity, so Playwright still counts it
    // visible) and titled "Delete".
    await alpha.hover();
    await expect(del).toHaveAttribute("title", "Delete");
    await expect(del).not.toHaveClass(/armed/);

    // ── First click: ARMS ONLY. This is the assertion the inverted
    //    mutant fails — under it, the row is gone by now. ──
    await del.click();
    await expect(alpha).toBeVisible();
    await expect(del).toHaveClass(/armed/);
    await expect(del).toHaveAttribute("title", "Click again to confirm delete");
    expect(await deleteCalls(page)).toEqual([]);

    // The sibling row is untouched and unarmed — arming is per-row state.
    const bravo = row(page, "conv-bravo");
    await expect(bravo).toBeVisible();
    await expect(bravo.locator(".delete-btn")).not.toHaveClass(/armed/);

    // ── Second click: CONFIRMS. ──
    await del.click();
    await expect(alpha).toHaveCount(0);
    expect(await deleteCalls(page)).toEqual(["conv-alpha"]);

    // Only the confirmed row went.
    await expect(bravo).toBeVisible();
  });

  test("the armed state disarms itself, so a stale ✕ cannot delete", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedConversations(page, CONVERSATIONS);
    await bootToChat(page, chat);

    const alpha = row(page, "conv-alpha");
    const del = alpha.locator(".delete-btn");
    await alpha.hover();
    await del.click();
    await expect(del).toHaveClass(/armed/);

    // The arm window is 3s (ConversationList.svelte `armDelete`). Past it
    // the button returns to idle: someone who armed a row, got distracted,
    // and came back to click the ✕ again arms rather than deletes.
    await expect(del).not.toHaveClass(/armed/, { timeout: 6_000 });
    await expect(del).toHaveAttribute("title", "Delete");

    await alpha.hover();
    await del.click();
    await expect(alpha).toBeVisible();
    expect(await deleteCalls(page)).toEqual([]);
  });

  // The deliberate path stays one action, and it must keep passing when the
  // arm/confirm mutation is applied: that is what makes the two cases above
  // a SURGICAL kill rather than a page crash any spec would notice. If this
  // one starts failing alongside them, the mutant is breaking the component,
  // not the invariant it claims to break.
  test("right-click → Delete is single-action — confirm is for the mis-clickable ✕", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedConversations(page, CONVERSATIONS);
    await bootToChat(page, chat);

    const bravo = row(page, "conv-bravo");
    await expect(bravo).toBeVisible();
    await bravo.click({ button: "right" });

    const menu = page.locator(".ctx-menu");
    await expect(menu).toBeVisible();
    await menu.getByRole("menuitem", { name: "Delete" }).click();

    await expect(bravo).toHaveCount(0);
    expect(await deleteCalls(page)).toEqual(["conv-bravo"]);
    await expect(menu).toHaveCount(0);
  });
});
