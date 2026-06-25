// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, type Page } from "../fixtures/test-base";
import type { ChatHarness } from "../fixtures/test-base";

// Home hub — the launcher / landing (Phase 2 UX refactor, D5).
//
// Pins the contract that Home, the default landing, is a summary over
// data that already exists: an ask box (→ global Ask), a recent-notebooks
// strip (→ Library / a notebook), recent threads (→ a conversation), and
// a first-run empty state. Home boots WITHOUT the bootToChat hop.

const NOW = Math.floor(Date.now() / 1000);

const NOTEBOOKS = [
  {
    id: "vault",
    name: "Research Vault",
    source_kind: "obsidian",
    doc_count: 1234,
    explorable: true,
    updated_unix: NOW - 3600,
    scope: "local",
  },
  {
    id: "wiki",
    name: "Wikipedia (English)",
    source_kind: "catalog",
    doc_count: 980000,
    explorable: false,
    updated_unix: NOW - 86_400,
    scope: "public",
  },
];

const CONVS = [
  { id: "c1", title: "Kestrel hunting behaviour", created_at: NOW - 7200, updated_at: NOW - 3600 },
  { id: "c2", title: null, created_at: NOW - 100_000, updated_at: NOW - 90_000 },
];

/** Seed the Home data sources. Must run after goto() (fresh shim) but
 *  before backend-ready mounts HomeView (which fetches on mount). */
async function seedHome(page: Page, notebooks: unknown[], convs: unknown[]) {
  await page.evaluate(
    ({ nb, cv }) => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("notebook_list", () => nb);
      w.__sovereign_test__.setHandler("list_conversations", () => cv);
    },
    { nb: notebooks, cv: convs },
  );
}

/** Boot to Home: poll-emit backend-ready until the hub renders. No hop. */
async function bootToHome(page: Page, chat: ChatHarness) {
  await page
    .locator("[data-testid='home-view'], .loading-screen, .app-layout")
    .first()
    .waitFor();
  await expect
    .poll(
      async () => {
        await chat.api.signalBackendReady();
        return page.getByTestId("home-view").count();
      },
      { timeout: 10_000, intervals: [50, 100, 200, 500] },
    )
    .toBeGreaterThan(0);
}

test.describe("Home hub", () => {
  test("renders recent notebooks and recent threads", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.goto("/");
    await seedHome(page, NOTEBOOKS, CONVS);
    await bootToHome(page, chat);

    // Notebook tiles (not counting the "+ Add" tile, which has its own id).
    const tiles = page.getByTestId("home-notebook-tile");
    await expect(tiles).toHaveCount(2);
    await expect(tiles.filter({ hasText: "Research Vault" })).toBeVisible();
    await expect(tiles.filter({ hasText: "Wikipedia (English)" })).toBeVisible();

    // Recent threads — newest first; a null title falls back gracefully.
    const threads = page.getByTestId("home-thread");
    await expect(threads).toHaveCount(2);
    await expect(threads.first()).toContainText("Kestrel hunting behaviour");
    await expect(threads.nth(1)).toContainText("Untitled conversation");
  });

  test("the ask box hands the question to the global chat", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.goto("/");
    await seedHome(page, NOTEBOOKS, []);
    await bootToHome(page, chat);

    await page.getByTestId("home-ask-input").fill("what did I conclude about kestrels?");
    await page.getByTestId("home-ask-submit").click();

    // Lands on the global Ask surface (the seed is consumed by ChatView).
    await page.locator(".chat-view").waitFor({ state: "visible" });
  });

  test("first-run empty state offers Add", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.goto("/");
    await seedHome(page, [], []);
    await bootToHome(page, chat);

    await expect(page.getByTestId("home-empty")).toBeVisible();
    await expect(page.getByTestId("home-empty-add")).toBeVisible();
    await expect(page.getByTestId("home-notebook-tile")).toHaveCount(0);
  });

  test("a notebook tile opens that notebook in the Library", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.goto("/");
    await seedHome(page, [NOTEBOOKS[0]], []);
    await bootToHome(page, chat);

    await page.getByTestId("home-notebook-tile").first().click();

    // The Library opens the notebook's detail (via the libraryNav handoff).
    await expect(page.getByTestId("notebook-detail")).toBeVisible();
    await expect(page.getByTestId("notebook-tab-ask")).toHaveClass(/active/);
  });

  test("a recent thread opens in Ask", async ({ sovereignPage: page, chat }) => {
    await page.goto("/");
    await seedHome(page, [NOTEBOOKS[0]], CONVS);
    await bootToHome(page, chat);

    await page.getByTestId("home-thread").first().click();
    await page.locator(".chat-view").waitFor({ state: "visible" });
  });

  test("+ Add opens the Library Add sheet", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.goto("/");
    await seedHome(page, [NOTEBOOKS[0]], []);
    await bootToHome(page, chat);

    await page.getByTestId("home-add").click();
    await expect(page.getByTestId("add-sheet")).toBeVisible();
  });
});
