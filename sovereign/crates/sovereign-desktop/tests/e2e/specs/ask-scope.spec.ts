// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat, type Page } from "../fixtures/test-base";
import type { ChatHarness } from "../fixtures/test-base";

// AskScopeBar — scope made visible (elegance phase, Move 1).
//
// Pins that the bar states what a question reaches in plain language on
// the global Ask ("everything you know") and reveals the toggle strip
// when clicked. Inside a notebook the bar is suppressed — the header
// already names the scope, so a bar there would be redundant.

const CORPORA = [
  { id: "vault", name: "Research Vault", description: "", size_compressed_gb: 0, size_indexed_gb: 0, license: "", tiers: [], status: "installed", chunks_count: 100, enrichment_enabled: false, indexed_at: 0, embedding_model: null, embedding_dimensions: null, vector_index_ready: true, parent_corpus_id: null, catalog_status: "hidden" },
  { id: "wiki", name: "Wikipedia", description: "", size_compressed_gb: 0, size_indexed_gb: 0, license: "", tiers: [], status: "installed", chunks_count: 100, enrichment_enabled: false, indexed_at: 0, embedding_model: null, embedding_dimensions: null, vector_index_ready: true, parent_corpus_id: null, catalog_status: "featured" },
];

/** Boot to chat with `list_corpora` seeded before the chat mounts (the
 *  scope bar + strip fetch corpora on mount). Mirrors bootToChat but
 *  injects the handler before the chat view appears. */
async function bootChatWithCorpora(page: Page, chat: ChatHarness, corpora: unknown[]) {
  await page.goto("/");
  await page.evaluate((c) => {
    const w = window as unknown as {
      __sovereign_test__: { setHandler: (cmd: string, fn: (a: unknown) => unknown) => void };
    };
    w.__sovereign_test__.setHandler("list_corpora", () => c);
  }, corpora);
  await page
    .locator(".loading-screen, .chat-view, .app-layout")
    .first()
    .waitFor();
  const chatView = page.locator(".chat-view");
  await expect
    .poll(
      async () => {
        await chat.api.signalBackendReady();
        return chatView.count();
      },
      { timeout: 10_000, intervals: [50, 100, 200, 500] },
    )
    .toBeGreaterThan(0);
  await chatView.first().waitFor({ state: "visible" });
}

test.describe("Ask scope bar", () => {
  test("states 'everything you know' on the global Ask", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await expect(page.getByTestId("ask-scope-bar")).toBeVisible();
    await expect(page.getByTestId("ask-scope-label")).toHaveText("everything you know");
  });

  test("clicking the bar reveals the filter strip", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootChatWithCorpora(page, chat, CORPORA);
    // Collapsed by default — the strip is hidden.
    await expect(page.locator(".corpus-filter-strip")).toHaveCount(0);
    await page.getByTestId("ask-scope-bar").click();
    // Now the strip's chips are visible.
    await expect(page.locator(".corpus-filter-strip")).toBeVisible();
    await expect(page.locator(".corpus-filter-strip .kb-tag")).toHaveCount(2);
  });

  test("a notebook's Ask states scope in the header, not a bar", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.goto("/");
    await page.evaluate((c) => {
      const w = window as unknown as {
        __sovereign_test__: { setHandler: (cmd: string, fn: (a: unknown) => unknown) => void };
      };
      w.__sovereign_test__.setHandler("list_corpora", () => c);
      w.__sovereign_test__.setHandler("notebook_list", () => [
        {
          id: "vault",
          name: "Research Vault",
          source_kind: "obsidian",
          doc_count: 100,
          explorable: true,
          updated_unix: 0,
          scope: "local",
        },
      ]);
    }, CORPORA);
    await page
      .locator(".loading-screen, .chat-view, .app-layout")
      .first()
      .waitFor();
    await expect
      .poll(
        async () => {
          await chat.api.signalBackendReady();
          return page.getByTestId("nav-library").count();
        },
        { timeout: 10_000, intervals: [50, 100, 200, 500] },
      )
      .toBeGreaterThan(0);

    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-ask").first().click();
    await expect(page.getByTestId("notebook-detail")).toBeVisible();

    // The header names the notebook — that IS the scope statement. The
    // global AskScopeBar is suppressed inside a notebook (redundant).
    await expect(page.getByTestId("notebook-detail").locator("h1")).toHaveText(
      "Research Vault",
    );
    await expect(page.getByTestId("ask-scope-bar")).toHaveCount(0);
  });
});
