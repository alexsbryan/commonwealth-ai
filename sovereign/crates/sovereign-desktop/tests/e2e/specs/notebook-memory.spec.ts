// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat, type Page } from "../fixtures/test-base";

// Notebook conversation memory (elegance phase, Move 2).
//
// A notebook's Ask tab remembers the conversations you've had with it:
// it resumes the most recent on re-open and offers a thread switcher +
// "+ New". Backed by the `notebook_conversations(corpus_id)` query.

const NOTEBOOK = {
  id: "vault",
  name: "Research Vault",
  source_kind: "obsidian",
  doc_count: 100,
  explorable: true,
  updated_unix: 0,
  scope: "local",
};

const CONVS = [
  { id: "c-new", title: "Kestrel hunting", created_at: 100, updated_at: 200 },
  { id: "c-old", title: "Sparrowhawks", created_at: 50, updated_at: 80 },
];

async function seed(page: Page) {
  await page.evaluate(
    (data) => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (a: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("notebook_list", () => [data.nb]);
      w.__sovereign_test__.setHandler("notebook_conversations", () => data.convs);
      // The resumed/selected conversation loads via get_conversation.
      w.__sovereign_test__.setHandler("get_conversation", (args) => {
        const a = args as { conversationId: string };
        return {
          id: a.conversationId,
          title: a.conversationId,
          messages: [],
          created_at: 0,
          updated_at: 0,
          enabled_corpora: ["vault"],
        };
      });
    },
    { nb: NOTEBOOK, convs: CONVS },
  );
}

test.describe("Notebook conversation memory", () => {
  test("resumes the most recent and shows the thread switcher", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seed(page);
    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-ask").first().click();
    await expect(page.getByTestId("notebook-detail")).toBeVisible();

    // The conversation switcher lives behind a header dropdown.
    await page.getByTestId("notebook-conv-menu").click();
    await expect(page.getByTestId("notebook-ask-history")).toBeVisible();
    const pills = page.getByTestId("notebook-ask-thread");
    await expect(pills).toHaveCount(2);
    // The newest (first) is resumed/active.
    await expect(pills.first()).toHaveClass(/active/);
    await expect(pills.first()).toContainText("Kestrel hunting");
  });

  test("'+ New' starts a fresh thread (no past thread active)", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seed(page);
    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-ask").first().click();
    await page.getByTestId("notebook-conv-menu").click();
    await expect(page.getByTestId("notebook-ask-thread").first()).toHaveClass(/active/);

    await page.getByTestId("notebook-ask-new").click();
    // Selecting closes the dropdown; reopen to verify no past thread is active.
    await page.getByTestId("notebook-conv-menu").click();
    await expect(
      page.locator('[data-testid="notebook-ask-thread"].active'),
    ).toHaveCount(0);
  });

  test("selecting an older thread switches to it", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seed(page);
    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-ask").first().click();
    await page.getByTestId("notebook-conv-menu").click();

    const pills = page.getByTestId("notebook-ask-thread");
    await pills.nth(1).click();
    // Selecting closes the dropdown; reopen to verify the switch stuck.
    await page.getByTestId("notebook-conv-menu").click();
    await expect(pills.nth(1)).toHaveClass(/active/);
    await expect(pills.first()).not.toHaveClass(/active/);
  });
});
