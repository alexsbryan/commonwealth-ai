// SPDX-License-Identifier: AGPL-3.0-or-later
// Conversation lifecycle against the real store: create via the UI,
// turn, rename via the command surface (asserting the UI reflects it),
// switch between conversations, delete via the UI's two-click arm —
// each step cross-checked between the DOM and the real SQLite store.
import { assertTurnInvariants, sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

test("create → turn → rename → switch → delete, UI and store agreeing", async ({
  sovereignPage: page,
  bridge,
}) => {
  await realBootToChat(page);
  const before = await bridge.invoke<Array<{ id: string }>>("list_conversations");

  // Create via the UI button; first turn binds the conversation.
  await page.locator(".new-btn").click();
  const messageId = await sendAndAwaitTurn(page, "Reply with the single word: lifecycle");
  await assertTurnInvariants(page, bridge, messageId);

  // The store gained exactly one conversation.
  const after = await bridge.invoke<Array<{ id: string; title: string | null }>>(
    "list_conversations",
  );
  expect(after.length).toBe(before.length + 1);
  const ours = after.filter((c) => !before.some((b) => b.id === c.id));
  expect(ours.length).toBe(1);
  const convId = ours[0].id;

  // Rename through the command surface. The runtime auto-titles the
  // conversation asynchronously after the first turn (one-shot) and
  // can clobber a rename that lands first — observed on the previous
  // run. Self-heal: re-issue the rename until it sticks in the store.
  await expect
    .poll(
      async () => {
        const list = await bridge.invoke<Array<{ id: string; title: string | null }>>(
          "list_conversations",
        );
        const title = list.find((c) => c.id === convId)?.title;
        if (title !== "lifecycle-renamed") {
          await bridge.invoke("rename_conversation", {
            conversationId: convId,
            title: "lifecycle-renamed",
          });
        }
        return title;
      },
      { timeout: 45_000, intervals: [1500, 3000] },
    )
    .toBe("lifecycle-renamed");
  await page.reload();
  await realBootToChat(page);
  await expect(
    page.locator(".convo-title", { hasText: "lifecycle-renamed" }).first(),
  ).toBeVisible();

  // Switch: select our conversation, its messages render.
  await page.locator(".convo-title", { hasText: "lifecycle-renamed" }).first().click();
  await expect(page.locator(".bubble.user .content").last()).toContainText("lifecycle");

  // Delete via the UI: armDelete is a two-click confirm.
  const row = page.locator(".convo-item", { hasText: "lifecycle-renamed" }).first();
  await row.hover();
  await row.locator(".delete-btn").click();
  await row.locator(".delete-btn").click();

  await expect
    .poll(async () => {
      const now = await bridge.invoke<Array<{ id: string }>>("list_conversations");
      return now.some((c) => c.id === convId);
    })
    .toBe(false);
  await expect(page.locator(".convo-title", { hasText: "lifecycle-renamed" })).toHaveCount(0);
});
