import { test, expect, bootToChat } from "../fixtures/test-base";

// Knowledge picker grouping — pin the chip contract.
//
// 2026-05-10 regression: clicking a Layers chip body did nothing
// (only the tiny "+" / "×" icon inside was the click target). The
// user reported "didn't look enabled" + "another instance of the
// desktop launched" — the second was almost certainly a dock
// double-click after the chip click silently failed. The chip is
// now a single `<button>` whose whole surface fires the toggle.
//
// These tests pin three load-bearing properties:
//
//   1. `list_corpora` results with `parent_corpus_id` are GROUPED:
//      the parent renders once at the top level, children render
//      under it. Internal `*-partition-*` ids never render.
//   2. Each chip is a real `<button>` with the whole surface
//      clickable — no <span class="layer-chip"> with the only
//      action target being a 12px "+" glyph.
//   3. Click → `install_corpus({corpusId: <child>})` for an
//      available chip; click → `remove_corpus({corpusId: <child>})`
//      for an installed chip.

const FIXTURE = {
  wikipediaCore: {
    id: "wikipedia",
    name: "Wikipedia",
    description: "Wikipedia Core",
    size_compressed_gb: 12,
    size_indexed_gb: 18,
    license: "CC-BY-SA-4.0",
    tiers: ["essential"],
    status: "not_installed" as const,
    chunks_count: null,
    enrichment_enabled: false,
    indexed_at: null,
    embedding_model: null,
    embedding_dimensions: null,
    vector_index_ready: false,
    parent_corpus_id: null,
    // KnowledgeStatus only renders a `.corpus-row` for top-level
    // corpora whose `catalog_status` is "featured" — everything
    // else falls into the "Coming soon" grid (preview) or is hidden.
    // The parent must be featured for its row (and the layer chips
    // nested inside it) to appear at all.
    catalog_status: "featured",
  },
  wikipediaSimple: {
    id: "wikipedia-simple",
    name: "Simple English Wikipedia",
    description: "Layer 0 satellite.",
    size_compressed_gb: 0.4,
    size_indexed_gb: 1.0,
    license: "CC-BY-SA-4.0",
    tiers: [],
    status: "not_installed" as const,
    chunks_count: null,
    enrichment_enabled: false,
    indexed_at: null,
    embedding_model: null,
    embedding_dimensions: null,
    vector_index_ready: false,
    parent_corpus_id: "wikipedia",
    // A child layer — grouped under its parent via parent_corpus_id,
    // never a top-level row, so catalog_status is irrelevant here.
    catalog_status: null,
  },
  wikipediaNewsworthy: {
    id: "wikipedia-newsworthy",
    name: "Wikipedia — Newsworthy",
    description: "Layer 2 freshness daemon.",
    size_compressed_gb: 0.05,
    size_indexed_gb: 0.2,
    license: "CC-BY-SA-4.0",
    tiers: [],
    status: "not_installed" as const,
    chunks_count: null,
    enrichment_enabled: false,
    indexed_at: null,
    embedding_model: null,
    embedding_dimensions: null,
    vector_index_ready: false,
    parent_corpus_id: "wikipedia",
    catalog_status: null,
  },
  wikipediaPartition: {
    id: "wikipedia-partition-node-deadbeef00000001",
    name: "wikipedia (partition)",
    description: "Internal collaborative-ingest partition.",
    size_compressed_gb: 0,
    size_indexed_gb: 0,
    license: "CC-BY-SA-4.0",
    tiers: [],
    status: "installed" as const,
    chunks_count: 42,
    enrichment_enabled: false,
    indexed_at: 1_700_000_000,
    embedding_model: "qwen-embedding-0.6b",
    embedding_dimensions: 1024,
    vector_index_ready: false,
    parent_corpus_id: null,
    // The `*-partition-node-*` id is filtered out of top-level rows
    // by KnowledgeStatus's isPartition() guard regardless of
    // catalog_status — this entry must never reach the DOM.
    catalog_status: "featured",
  },
  wikipediaFetched: {
    id: "wikipedia-fetched",
    name: "Wikipedia (fetched)",
    description: "On-demand fetched articles.",
    size_compressed_gb: 0,
    size_indexed_gb: 0.1,
    license: "CC-BY-SA-4.0",
    tiers: [],
    status: "installed" as const,
    chunks_count: 128,
    enrichment_enabled: false,
    indexed_at: 1_700_000_000,
    embedding_model: "qwen-embedding-0.6b",
    embedding_dimensions: 1024,
    vector_index_ready: true,
    // A child of wikipedia, but a BYPRODUCT of the Catalog add-on
    // (filled by on-demand fetches) — not a user-toggleable layer.
    // KnowledgeStatus must filter it out of the add-on chips.
    parent_corpus_id: "wikipedia",
    catalog_status: "preview",
  },
};

async function openKnowledgeTab(page: import("@playwright/test").Page) {
  // From chat → settings via the cog in the left nav rail.
  await page.getByTestId("nav-settings").click();
  // From Settings → Knowledge tab. The TOC items are <button> not <a>,
  // and the panel filters tabs by feature flag — wait for the
  // Knowledge entry to be present before clicking.
  const knowledgeTab = page.getByRole("button", { name: /^Knowledge$/ });
  await expect(knowledgeTab).toBeVisible({ timeout: 5_000 });
  await knowledgeTab.click();
  await expect(
    page.getByRole("heading", { name: "Knowledge", exact: true }),
  ).toBeVisible({ timeout: 5_000 });
}

test.describe("knowledge picker — layer grouping", () => {
  test.beforeEach(async ({ sovereignPage: page, chat }) => {
    // bootToChat does the goto + waits for the chat-view to render.
    // Install handlers AFTER the page is loaded but BEFORE we
    // navigate into Settings — KnowledgeStatus calls list_corpora on
    // mount, so the handler has to be in place by the time the tab
    // becomes active.
    await bootToChat(page, chat);
    await page.evaluate((fixture) => {
      // Return the parent + both children + a partition. The frontend
      // is responsible for hiding the partition and grouping the
      // children under their parent.
      window.__sovereign_test__.setHandler("list_corpora", () => [
        fixture.wikipediaCore,
        fixture.wikipediaSimple,
        fixture.wikipediaNewsworthy,
        fixture.wikipediaFetched,
        fixture.wikipediaPartition,
      ]);
      // Capture install/remove invocations so each test can assert
      // them via window.__chipInstallCalls / __chipRemoveCalls.
      (window as unknown as { __chipInstallCalls: unknown[] }).__chipInstallCalls = [];
      (window as unknown as { __chipRemoveCalls: unknown[] }).__chipRemoveCalls = [];
      window.__sovereign_test__.setHandler("install_corpus", (args) => {
        (window as unknown as { __chipInstallCalls: unknown[] }).__chipInstallCalls.push(args);
        return null;
      });
      window.__sovereign_test__.setHandler("remove_corpus", (args) => {
        (window as unknown as { __chipRemoveCalls: unknown[] }).__chipRemoveCalls.push(args);
        return null;
      });
      // `lc_start_layered_setup` is the bundle install for Wikipedia
      // Core; capture but no-op so we can assert it isn't invoked by
      // a layer chip click.
      (window as unknown as { __chipBundleCalls: unknown[] }).__chipBundleCalls = [];
      window.__sovereign_test__.setHandler("lc_start_layered_setup", (args) => {
        (window as unknown as { __chipBundleCalls: unknown[] }).__chipBundleCalls.push(args);
        return null;
      });
      // KnowledgeStatus polls newsworthy status (for the parent's
      // layer-status line) and probes expandability per installed
      // corpus on mount. Stub both benign so the picker renders
      // without unstubbed-invoke noise and without the extra
      // newsworthy detail block interfering with the chip assertions.
      window.__sovereign_test__.setHandler("lc_newsworthy_status", () => null);
      window.__sovereign_test__.setHandler("lc_can_expand", () => false);
    }, FIXTURE);
    await openKnowledgeTab(page);
  });

  test("renders one Wikipedia row, not four — children grouped, partition hidden", async ({
    sovereignPage: page,
  }) => {
    // Only the parent renders as a top-level row. Filter to corpus
    // rows under the Knowledge tab to avoid catching rows in other
    // surfaces that happen to share the class.
    const knowledgePanel = page
      .getByRole("heading", { name: "Knowledge", exact: true })
      .locator("xpath=ancestor::section");
    const rows = knowledgePanel.locator(".corpus-row");
    await expect(rows).toHaveCount(1);

    // Pin the row's identity to Wikipedia via the corpus-name element
    // — exact-text match so "Simple English Wikipedia" doesn't count.
    await expect(
      rows.locator(".corpus-name").filter({ hasText: /^\s*Wikipedia\s*$/ }),
    ).toHaveCount(1);

    // The partition id must never reach the DOM as its own row, and
    // its name must not appear in any top-level corpus-name slot.
    await expect(
      rows.locator(".corpus-name").filter({ hasText: /partition-node-/ }),
    ).toHaveCount(0);

    // Simple English + Newsworthy live in the layers panel, not as
    // their own top-level rows.
    const layersPanel = page.getByTestId("corpus-layers");
    await expect(layersPanel).toBeVisible();
    await expect(layersPanel).toContainText("Simple English Wikipedia");
    await expect(layersPanel).toContainText("Wikipedia — Newsworthy");

    // Defensive: neither child name appears as a top-level
    // corpus-name (the rendered name inside a parent row).
    await expect(
      rows
        .locator(".corpus-name")
        .filter({ hasText: "Simple English Wikipedia" }),
    ).toHaveCount(0);
    await expect(
      rows
        .locator(".corpus-name")
        .filter({ hasText: "Wikipedia — Newsworthy" }),
    ).toHaveCount(0);

    // `wikipedia-fetched` is a child of wikipedia but a BYPRODUCT of
    // the Catalog add-on, not a toggle — it must be filtered out of the
    // add-on chips AND never appear as a top-level row. (Its fetched
    // count surfaces as status text under the Catalog chip instead.)
    await expect(
      page.locator(
        '[data-testid="layer-chip"][data-layer-id="wikipedia-fetched"]',
      ),
    ).toHaveCount(0);
    await expect(layersPanel).not.toContainText("Wikipedia (fetched)");
    await expect(
      rows.locator(".corpus-name").filter({ hasText: "Wikipedia (fetched)" }),
    ).toHaveCount(0);
  });

  test("each layer chip is a button with the whole surface clickable", async ({
    sovereignPage: page,
  }) => {
    const chips = page.getByTestId("layer-chip");
    await expect(chips).toHaveCount(2);

    // Every chip must be a <button>. Regression for the original
    // <span> chip with only a tiny "+" icon clickable.
    for (const handle of await chips.elementHandles()) {
      const tagName = await handle.evaluate((el) => el.tagName);
      expect(tagName).toBe("BUTTON");
    }

    // Available chips carry a visible action label ("Add") so the
    // user knows clicking does something.
    const newsworthyChip = page.locator(
      '[data-testid="layer-chip"][data-layer-id="wikipedia-newsworthy"]',
    );
    await expect(newsworthyChip).toContainText(/add/i);
    await expect(newsworthyChip).toHaveAttribute("data-layer-status", "not_installed");
    await expect(newsworthyChip).toHaveAttribute("aria-pressed", "false");
  });

  test("clicking an available chip invokes install_corpus, not the bundle setup", async ({
    sovereignPage: page,
  }) => {
    const newsworthyChip = page.locator(
      '[data-testid="layer-chip"][data-layer-id="wikipedia-newsworthy"]',
    );
    await newsworthyChip.click();

    // Install was called with the child id, not "wikipedia".
    const installCalls = await page.evaluate(
      () => (window as unknown as { __chipInstallCalls: unknown[] }).__chipInstallCalls,
    );
    expect(installCalls).toHaveLength(1);
    expect((installCalls[0] as { corpusId: string }).corpusId).toBe(
      "wikipedia-newsworthy",
    );

    // The bundle path must NOT have fired — that's the parent's
    // install button, not the chip's job.
    const bundleCalls = await page.evaluate(
      () => (window as unknown as { __chipBundleCalls: unknown[] }).__chipBundleCalls,
    );
    expect(bundleCalls).toHaveLength(0);
  });

  test("clicking an installed chip invokes remove_corpus", async ({
    sovereignPage: page,
  }) => {
    // Flip Newsworthy to installed, refresh the picker.
    await page.evaluate((fixture) => {
      const installed = {
        ...fixture.wikipediaNewsworthy,
        status: "installed",
        chunks_count: 57,
        vector_index_ready: true,
        indexed_at: 1_700_000_000,
        embedding_model: "qwen-embedding-0.6b",
        embedding_dimensions: 1024,
      };
      window.__sovereign_test__.setHandler("list_corpora", () => [
        fixture.wikipediaCore,
        fixture.wikipediaSimple,
        installed,
        fixture.wikipediaPartition,
      ]);
    }, FIXTURE);

    // Trigger a re-fetch by navigating away and back. KnowledgeStatus
    // refreshes its corpus list on mount, so toggling tabs is enough.
    await page.getByRole("button", { name: /^Models$/ }).click();
    await page.getByRole("button", { name: /^Knowledge$/ }).click();

    const newsworthyChip = page.locator(
      '[data-testid="layer-chip"][data-layer-id="wikipedia-newsworthy"]',
    );
    await expect(newsworthyChip).toHaveAttribute("data-layer-status", "installed");
    await expect(newsworthyChip).toContainText(/remove/i);
    await expect(newsworthyChip).toHaveAttribute("aria-pressed", "true");

    await newsworthyChip.click();

    const removeCalls = await page.evaluate(
      () => (window as unknown as { __chipRemoveCalls: unknown[] }).__chipRemoveCalls,
    );
    expect(removeCalls).toHaveLength(1);
    expect((removeCalls[0] as { corpusId: string }).corpusId).toBe(
      "wikipedia-newsworthy",
    );

    // Install must NOT have been called by the toggle on an
    // already-installed chip.
    const installCalls = await page.evaluate(
      () => (window as unknown as { __chipInstallCalls: unknown[] }).__chipInstallCalls,
    );
    expect(installCalls).toHaveLength(0);
  });
});
