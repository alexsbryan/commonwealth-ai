// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";
import type { Page } from "@playwright/test";

// Notebook detail — Ask / Explore in the header, Sources / Settings in ⋯.
//
// Pins the per-notebook contract the Library opens into:
//   1. the sections switch and surface the right content;
//   2. Explore on a notebook with no map offers "Make explorable", which
//      runs the standard enrich path (init + build) and shows progress;
//   3. Ask scopes the conversation to this notebook (the existing
//      outerWorkScopeStore bridge, no ChatView change);
//   4. Settings removes the notebook and returns to the shelf.

async function seedOneNotebook(
  page: Page,
  nb: Record<string, unknown>,
): Promise<void> {
  await page.evaluate((notebook) => {
    const w = window as unknown as {
      __sovereign_test__: {
        setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
      };
    };
    w.__sovereign_test__.setHandler("notebook_list", () => [notebook]);
  }, nb);
}

const CATALOG_NB = {
  id: "wikipedia",
  name: "Wikipedia (English)",
  source_kind: "catalog",
  doc_count: 4096,
  explorable: false,
  updated_unix: Math.floor(Date.now() / 1000) - 7200,
  scope: "public",
};

test.describe("Notebook detail", () => {
  test("the sections switch and surface their content", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedOneNotebook(page, CATALOG_NB);
    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-ask").first().click();

    // Opens on Ask.
    await expect(page.getByTestId("notebook-detail")).toBeVisible();
    await expect(page.getByTestId("notebook-tab-ask")).toHaveClass(/active/);

    // Explore (segmented) → the Make-explorable surface.
    await page.getByTestId("notebook-tab-explore").click();
    await expect(page.getByTestId("notebook-tab-explore")).toHaveClass(/active/);
    await expect(page.getByTestId("notebook-make-explorable")).toBeVisible();

    // Sources + Settings live in the ⋯ overflow now.
    await page.getByTestId("notebook-more").click();
    await page.getByTestId("notebook-tab-sources").click();
    await expect(page.getByText("Installed from the public catalog")).toBeVisible();

    await page.getByTestId("notebook-more").click();
    await page.getByTestId("notebook-tab-settings").click();
    await expect(page.getByTestId("notebook-remove")).toBeVisible();
  });

  test("Explore offers Make explorable, which runs the enrich path", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedOneNotebook(page, CATALOG_NB);

    // Track the enrich kickoff (init scaffolds the atlas config, build
    // runs it) and hand back a streaming job handle.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      (window as unknown as { __enrichCalls: string[] }).__enrichCalls = [];
      w.__sovereign_test__.setHandler("recipe_enrich_init_from_corpus", (args) => {
        const a = args as { corpusId?: string };
        (window as unknown as { __enrichCalls: string[] }).__enrichCalls.push(
          `init:${a.corpusId}`,
        );
        return "literary_atlas";
      });
      w.__sovereign_test__.setHandler("enrich_build_async", (args) => {
        const a = args as { corpusId?: string };
        (window as unknown as { __enrichCalls: string[] }).__enrichCalls.push(
          `build:${a.corpusId}`,
        );
        return {
          job_id: "nb-job-1",
          corpus_id: a.corpusId,
          channel: "enrich://progress/nb-job-1",
        };
      });
    });

    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-explore").first().click();
    await page.getByTestId("notebook-make-explorable").click();

    // The init + build pair fired against this corpus, and the live
    // build surface (the shared EnrichmentStage) is up.
    await expect(page.getByText("Building the map…")).toBeVisible();
    const calls = await page.evaluate(
      () => (window as unknown as { __enrichCalls: string[] }).__enrichCalls,
    );
    expect(calls).toEqual(["init:wikipedia", "build:wikipedia"]);
  });

  test("Ask scopes the conversation to this notebook", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedOneNotebook(page, CATALOG_NB);
    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-ask").first().click();

    await expect(page.getByTestId("notebook-detail")).toBeVisible();

    // The scoped ChatView mints an empty conversation and applies the
    // notebook's id as the corpus allow-list via the existing
    // outerWorkScopeStore bridge. The shim records the most recent
    // set_conversation_enabled_corpora payload.
    await expect
      .poll(
        async () =>
          page.evaluate(() => {
            const last = (
              window as unknown as { __sovereign_test__: { _lastEnabledCorpora?: unknown } }
            ).__sovereign_test__._lastEnabledCorpora;
            return last ? JSON.stringify(last) : "";
          }),
        { timeout: 8_000, intervals: [100, 200, 400] },
      )
      .toContain("wikipedia");
  });

  test("Settings removes the notebook and returns to the shelf", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedOneNotebook(page, CATALOG_NB);

    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      (window as unknown as { __removeCalls: unknown[] }).__removeCalls = [];
      w.__sovereign_test__.setHandler("remove_corpus", (args) => {
        (window as unknown as { __removeCalls: unknown[] }).__removeCalls.push(args);
        return 1;
      });
    });

    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-explore").first().click();
    await page.getByTestId("notebook-more").click();
    await page.getByTestId("notebook-tab-settings").click();
    await page.getByTestId("notebook-remove").click();
    await page.getByTestId("notebook-remove-confirm").click();

    // remove_corpus fired (catalog notebooks route to the catalog
    // remove path), and the detail closed back to the shelf.
    const removeCalls = await page.evaluate(
      () => (window as unknown as { __removeCalls: unknown[] }).__removeCalls,
    );
    expect(removeCalls).toHaveLength(1);
    expect((removeCalls[0] as { corpusId: string }).corpusId).toBe("wikipedia");
    await expect(page.getByTestId("library-view")).toBeVisible();
  });

  test("the Settings tab bridges to the Workshop (use→make)", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seedOneNotebook(page, CATALOG_NB);
    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-explore").first().click();
    await page.getByTestId("notebook-more").click();
    await page.getByTestId("notebook-tab-settings").click();

    // The "Built by … → Open in Workshop" hinge opens the Workshop.
    await expect(page.getByTestId("notebook-open-workshop")).toBeVisible();
    await page.getByTestId("notebook-open-workshop").click();
    await expect(page.getByTestId("workshop-view")).toBeVisible();
  });
});
