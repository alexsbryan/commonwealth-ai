// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";
import type { Page } from "@playwright/test";

// Library shelf — the knowledge home (Phase 1 UX refactor).
//
// Pins the contract that the Library:
//   1. lists every notebook off the single `notebook_list` command,
//      surfacing source-kind, chunk count, and the explorable (✦) badge;
//   2. shows an Add CTA when empty;
//   3. hosts the Add sheet (the five ingest paths folded into one
//      surface with three sections);
//   4. opens a notebook on its Ask or Explore tab from the card actions.

const NOW = Math.floor(Date.now() / 1000);

const NOTEBOOKS = [
  {
    id: "my-vault",
    name: "Research Vault",
    source_kind: "obsidian",
    doc_count: 1234,
    explorable: true,
    updated_unix: NOW - 3600,
    scope: "local",
  },
  {
    id: "wikipedia",
    name: "Wikipedia (English)",
    source_kind: "catalog",
    doc_count: 980000,
    explorable: false,
    updated_unix: NOW - 86_400 * 5,
    scope: "public",
  },
];

async function seedNotebooks(page: Page, list: unknown[]): Promise<void> {
  await page.evaluate((nb) => {
    const w = window as unknown as {
      __sovereign_test__: {
        setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
      };
    };
    w.__sovereign_test__.setHandler("notebook_list", () => nb);
  }, list);
}

test.describe("Library shelf", () => {
  test("lists notebooks with kind, count, and the explorable badge", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedNotebooks(page, NOTEBOOKS);
    await page.getByTestId("nav-library").click();

    const cards = page.getByTestId("notebook-card");
    await expect(cards).toHaveCount(2);

    // The vault: name, Obsidian source chip, an explorable ✦ badge.
    const vault = cards.filter({ hasText: "Research Vault" });
    await expect(vault).toContainText("Obsidian");
    await expect(vault).toContainText("1,234 passages");
    await expect(vault.getByText("✦")).toBeVisible();

    // The catalog corpus: Catalog chip, no ✦ (not explorable).
    const wiki = cards.filter({ hasText: "Wikipedia (English)" });
    await expect(wiki).toContainText("Catalog");
    await expect(wiki.getByText("✦")).toHaveCount(0);
  });

  test("empty state offers an Add CTA when there are no notebooks", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedNotebooks(page, []);
    await page.getByTestId("nav-library").click();

    await expect(page.getByTestId("library-empty")).toBeVisible();
    await expect(page.getByTestId("library-empty-add")).toBeVisible();
    await expect(page.getByTestId("notebook-card")).toHaveCount(0);
  });

  test("+ Add opens the Add sheet with the three source sections", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedNotebooks(page, NOTEBOOKS);
    await page.getByTestId("nav-library").click();

    await page.getByTestId("library-add").click();
    await expect(page.getByTestId("add-sheet")).toBeVisible();
    // The five ingest paths folded into three sections.
    await expect(page.getByTestId("add-section-files")).toBeVisible();
    await expect(page.getByTestId("add-section-imports")).toBeVisible();
    await expect(page.getByTestId("add-section-catalog")).toBeVisible();

    // Closing returns to the shelf.
    await page.getByTestId("add-sheet-close").click();
    await expect(page.getByTestId("library-view")).toBeVisible();
    await expect(page.getByTestId("add-sheet")).toHaveCount(0);
  });

  test("a card's Ask and Explore actions open the notebook on the right tab", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedNotebooks(page, NOTEBOOKS);
    await page.getByTestId("nav-library").click();

    // Ask opens the detail on its Ask tab (the scoped chat).
    await page.getByTestId("notebook-ask").first().click();
    await expect(page.getByTestId("notebook-detail")).toBeVisible();
    await expect(page.getByTestId("notebook-tab-ask")).toHaveClass(/active/);

    // Back returns to the shelf.
    await page.getByTestId("notebook-detail-back").click();
    await expect(page.getByTestId("library-view")).toBeVisible();

    // Explore on the non-explorable catalog notebook opens its Explore
    // tab (the "Make explorable" surface — no atlas to mount).
    const wiki = page
      .getByTestId("notebook-card")
      .filter({ hasText: "Wikipedia (English)" });
    await wiki.getByTestId("notebook-explore").click();
    await expect(page.getByTestId("notebook-detail")).toBeVisible();
    await expect(page.getByTestId("notebook-tab-explore")).toHaveClass(/active/);
    await expect(page.getByTestId("notebook-make-explorable")).toBeVisible();
  });
});
