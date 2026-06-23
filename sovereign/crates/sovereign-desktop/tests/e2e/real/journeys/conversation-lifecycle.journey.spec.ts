// SPDX-License-Identifier: AGPL-3.0-or-later
// J4 (Tier 1) — the conversation sidebar lifecycle: create, switch
// (history must rehydrate), rename, and delete.
//
// Two things this spec is careful about:
//   • The real-mode backend is shared and serial, so the list accumulates
//     other journeys' conversations. It therefore identifies its own
//     conversation by the stable `data-conversation-id` on the list item
//     (not by position or title).
//   • The backend auto-generates a title from the first message (async,
//     fires conversations:changed → list reload). Renaming is done AFTER
//     the switch so that settle has happened, and the rename is verified
//     to stick — racing the inline edit against the reload was a real
//     flake.
import { expect, journeyTest, realBootToChat } from "./journey";
import { J_CONVERSATION_LIFECYCLE } from "./manifest";

journeyTest(J_CONVERSATION_LIFECYCLE, async ({ page, run }) => {
  await realBootToChat(page);

  // ── Conversation A: a turn with a distinctive user message ──
  await run.turn(
    "In what year was the schooner Tamarind rescued near the Meridian Lighthouse?",
  );
  const activeA = page.locator(".convo-item.selected");
  await expect(
    activeA,
    "the active conversation must appear in the sidebar after its first turn",
  ).toBeVisible({ timeout: 15_000 });
  const idA = await activeA.getAttribute("data-conversation-id");
  expect(idA, "conversation list items must expose a stable id").toBeTruthy();
  const itemA = page.locator(`.convo-item[data-conversation-id="${idA}"]`);

  // ── Conversation B: new, with its own turn ──
  await page.locator(".new-btn").click();
  await run.turn("In one sentence, who was Elowen Marsh?");

  // ── Switch back to A by stable id: prior history must rehydrate ──
  await itemA.click();
  await expect(itemA, "clicking a conversation must select it").toHaveClass(/selected/);
  await expect(
    page.locator(".bubble.user", { hasText: "Tamarind" }),
    "A's prior user message must rehydrate when switching back to it",
  ).toBeVisible({ timeout: 15_000 });

  // ── Rename A (auto-title has settled; verify the rename sticks) ──
  await itemA.locator(".convo-title").dblclick();
  const renameInput = itemA.locator(".convo-title-input");
  await expect(renameInput, "double-click must open the inline rename input").toBeVisible();
  await renameInput.fill("J4 lighthouse log");
  await renameInput.press("Enter");
  await expect(
    itemA.locator(".convo-title"),
    "rename must persist on the list item",
  ).toHaveText("J4 lighthouse log", { timeout: 15_000 });

  // ── Delete A (two-click confirm) ──
  const del = itemA.locator(".delete-btn");
  await del.click();
  await expect(del, "first click must arm the delete").toHaveClass(/armed/);
  await del.click();
  await expect(itemA, "the deleted conversation must leave the list").toHaveCount(0);

  run.note(
    "created, switched (history persisted), renamed, and deleted a conversation by stable id",
  );
});
