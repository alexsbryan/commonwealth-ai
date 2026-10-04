// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";
import type { Page } from "@playwright/test";

// Inner Work autosaves the draft to localStorage on a 400ms debounce, so
// closing the window does not cost the user what they wrote. This is the
// one surface in the app where the text IS the point and there is no send
// button standing between the user and losing it.
//
// `inner-work-page.spec.ts` already covers nav-away-and-back, but that
// cannot see this: App.svelte keeps the surface mounted and hides it with
// `display: none`, so the draft survives in component memory whether or
// not it was ever persisted. `sabotage-bank.mjs` inverted the store's
// empty check (`text.length === 0` → `>= 0`, making every save a
// `removeItem`) and the whole desktop gate stayed green — tracked as
// `hole-inner-work-draft-persistence`.
//
// So this spec asserts the two things that survive a process boundary:
// the localStorage key itself, and a genuine reload.

const DRAFT_PREFIX = "sovereign:inner_work:";
const DRAFT = "Sitting with the part that wants to be certain.";

/** Mirror `innerWorkSession.todayIsoDate` — LOCAL date parts, not
 *  `toISOString()`. A UTC key would drift off the real one for anyone
 *  west of Greenwich after 00:00Z and this spec would read an empty
 *  slot while the app happily saved to the right one. */
function todayIsoDate(): string {
  const now = new Date();
  return [
    now.getFullYear(),
    String(now.getMonth() + 1).padStart(2, "0"),
    String(now.getDate()).padStart(2, "0"),
  ].join("-");
}

const readDraftKey = (page: Page, key: string): Promise<string | null> =>
  page.evaluate((k) => localStorage.getItem(k), key);

async function openInnerWork(page: Page): Promise<void> {
  await page.getByTestId("nav-reflect").click();
  // The threshold holds the empty page for 800ms before the dateline and
  // the column fade in.
  await expect(page.locator(".dateline")).toBeVisible({ timeout: 3_000 });
  await expect(page.locator("textarea.column")).toBeVisible();
}

test.describe("inner work — draft persistence", () => {
  test("a draft is written to localStorage and survives a reload", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await openInnerWork(page);

    const key = DRAFT_PREFIX + todayIsoDate();
    expect(await readDraftKey(page, key)).toBeNull();

    const column = page.locator("textarea.column");
    await column.click();
    await column.fill(DRAFT);

    // ── The autosave actually happened. Polling rather than sleeping on
    //    the 400ms debounce: the assertion is "it lands", not "it lands
    //    in exactly 400ms". ──
    await expect.poll(() => readDraftKey(page, key), { timeout: 5_000 }).toBe(
      DRAFT,
    );

    // ── And it survives a real process boundary. Nav-away-and-back does
    //    not prove this — the surface stays mounted, so the text would
    //    come back from memory even if nothing were ever persisted. ──
    await bootToChat(page, chat);
    await openInnerWork(page);
    await expect(page.locator("textarea.column")).toHaveValue(DRAFT);
  });

  test("clearing the draft removes the key rather than persisting an empty one", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await openInnerWork(page);

    const key = DRAFT_PREFIX + todayIsoDate();
    const column = page.locator("textarea.column");
    await column.click();
    await column.fill(DRAFT);
    await expect.poll(() => readDraftKey(page, key), { timeout: 5_000 }).toBe(
      DRAFT,
    );

    // Emptying the column is a deliberate act — it must not leave a
    // resurrectable ghost behind, and a reload must come back blank.
    await column.fill("");
    await expect.poll(() => readDraftKey(page, key), { timeout: 5_000 }).toBeNull();

    await bootToChat(page, chat);
    await openInnerWork(page);
    await expect(page.locator("textarea.column")).toHaveValue("");
  });
});
