// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Atlas corpus view — windowed (virtualized) atom list.
//
// SEP and other large corpora list thousands of atoms. Rendering one
// <li> per atom made the page so tall the scroll container couldn't
// reach the bottom (and choked on the node count). AtlasCorpusView now
// renders only the rows in (and just around) the viewport, inside a
// sizer that reserves the full scroll height. This spec pins the two
// guarantees that matters to the user:
//   (1) only a small window of rows is in the DOM for a large list, and
//   (2) the scroller can actually reach the bottom — the last atom
//       becomes reachable — which is the bug this fixed.

const TOTAL = 1000;

test.describe("atlas corpus view — windowed atom list", () => {
  test("renders only a window of rows and can scroll to the bottom of a large list", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await page.evaluate((total) => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      // Atlas index: one selectable atom corpus, no conversation
      // corpora (so the per-conv progress probe never fires).
      w.__sovereign_test__.setHandler("atlas_list_corpora", () => [
        {
          corpus_id: "sep",
          display_name: "Stanford Encyclopedia of Philosophy",
          total_atoms: total,
          atom_counts: { Entity: total },
        },
      ]);
      w.__sovereign_test__.setHandler("atlas_list_conv_corpora", () => []);

      // Library shelf: one explorable notebook. Its Explore tab opens
      // this corpus's atom map via AtlasSurface(startingCorpusId).
      w.__sovereign_test__.setHandler("notebook_list", () => [
        {
          id: "sep",
          name: "Stanford Encyclopedia of Philosophy",
          source_kind: "catalog",
          doc_count: total,
          explorable: true,
          updated_unix: Math.floor(Date.now() / 1000),
          scope: "public",
        },
      ]);

      // Atom list: serve the requested page slice out of `total` atoms.
      // Unique atom_id per atom (the #each key) — duplicates would trip
      // the harness console gate.
      w.__sovereign_test__.setHandler("atlas_list_atoms", (args: unknown) => {
        const a = args as {
          filter?: { atom_type?: string };
          page?: { offset?: number; limit?: number };
        };
        // Honour the atom_type filter the real backend applies. The
        // Explore surface also asks for Question atoms (the "open
        // questions your sources raise" chip row); this corpus is all
        // Entity atoms, so that request must come back empty — otherwise
        // the same display names render twice on the page.
        if (a.filter?.atom_type && a.filter.atom_type !== "Entity") {
          return { items: [], total_matching: 0, next_offset: undefined };
        }
        const offset = a.page?.offset ?? 0;
        const limit = a.page?.limit ?? 200;
        const end = Math.min(offset + limit, total);
        const items = [];
        for (let i = offset; i < end; i++) {
          items.push({
            atom_id: `entity-${i}`,
            stable_key: `entity-${i}`,
            atom_type: "Entity",
            display_name: `Atom ${i}`,
            enrichment_depth: "extracted",
            evidence_chunk_count: 0,
            curation_status: "generated",
            overlay_supports: false,
          });
        }
        return {
          items,
          total_matching: total,
          next_offset: end < total ? end : undefined,
        };
      });
    }, TOTAL);

    // Open the notebook's Explore tab → AtlasSurface seeds straight to
    // this corpus (startingCorpusId) and AtlasCorpusView mounts.
    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-explore").first().click();

    // The windowed scroll container + first rows render.
    const scroll = page.locator(".atom-scroll");
    await expect(scroll).toBeVisible();
    const rows = page.locator('[data-testid="atlas-atom-row"]');
    await expect(rows.first()).toBeVisible();

    // (1) Windowed: far fewer rows in the DOM than the 1000 loaded.
    // Even a tall viewport renders only tens of rows + overscan.
    expect(await rows.count()).toBeLessThan(100);
    // Row-scoped: other surfaces on this page (the open-questions chip
    // row) can carry atom display names too, so "is Atom N on screen?"
    // must mean "is it in the windowed list?".
    const row = (name: string) => rows.getByText(name, { exact: true });
    // The list starts at the top — Atom 0 is the first row.
    await expect(row("Atom 0")).toBeVisible();

    // (2) The container can actually scroll to the bottom. Repeatedly
    // scroll to the end; infinite-scroll loads further pages until all
    // 1000 atoms exist, then the LAST atom becomes reachable. Reaching
    // "Atom 999" is the proof the scroller hits its bottom.
    await expect
      .poll(
        async () => {
          await scroll.evaluate((el) => el.scrollTo(0, el.scrollHeight));
          return row("Atom 999").count();
        },
        { timeout: 12_000, intervals: [150, 250, 400, 600] },
      )
      .toBe(1);

    // Still windowed at the bottom — the DOM never held all 1000, and
    // the top rows have been recycled out (Atom 0 is no longer present).
    expect(await rows.count()).toBeLessThan(100);
    await expect(row("Atom 0")).toHaveCount(0);
  });

  // Freshness — atoms whose source doc was re-indexed after install
  // (a wikipedia-newsworthy fetch, a watched-folder edit) carry an
  // `updated_at`. The backend already sorts them fresh-first; the view
  // renders a "fresh" badge so the user sees *why* a row leads the list.
  // Baseline (install-time) atoms have no `updated_at` and no badge.
  test("renders a fresh badge on recently-reindexed atoms, baseline atoms unmarked", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("atlas_list_corpora", () => [
        {
          corpus_id: "wikipedia",
          display_name: "Wikipedia (English)",
          total_atoms: 3,
          atom_counts: { Entity: 3 },
        },
      ]);
      w.__sovereign_test__.setHandler("atlas_list_conv_corpora", () => []);
      w.__sovereign_test__.setHandler("notebook_list", () => [
        {
          id: "wikipedia",
          name: "Wikipedia (English)",
          source_kind: "catalog",
          doc_count: 3,
          explorable: true,
          updated_unix: Math.floor(Date.now() / 1000),
          scope: "public",
        },
      ]);

      const nowSecs = Math.floor(Date.now() / 1000);
      // Returned in backend sort order: two freshly-reindexed atoms
      // first (Gaza newest, then Earthquake), baseline atom last with
      // no updated_at.
      const items = [
        {
          atom_id: "e-gaza",
          stable_key: "e-gaza",
          atom_type: "Entity",
          display_name: "Gaza",
          enrichment_depth: "extracted",
          evidence_chunk_count: 1,
          curation_status: "generated",
          overlay_supports: false,
          updated_at: nowSecs - 120, // ~2m ago
        },
        {
          atom_id: "e-quake",
          stable_key: "e-quake",
          atom_type: "Entity",
          display_name: "Earthquake",
          enrichment_depth: "extracted",
          evidence_chunk_count: 1,
          curation_status: "generated",
          overlay_supports: false,
          updated_at: nowSecs - 3600, // ~1h ago
        },
        {
          atom_id: "e-aristotle",
          stable_key: "e-aristotle",
          atom_type: "Entity",
          display_name: "Aristotle",
          enrichment_depth: "extracted",
          evidence_chunk_count: 1,
          curation_status: "generated",
          overlay_supports: false,
          // no updated_at — baseline install-time content
        },
      ];
      w.__sovereign_test__.setHandler("atlas_list_atoms", () => ({
        items,
        total_matching: items.length,
        next_offset: undefined,
      }));
    });

    await page.getByTestId("nav-library").click();
    await page.getByTestId("notebook-explore").first().click();

    const rows = page.locator('[data-testid="atlas-atom-row"]');
    await expect(rows).toHaveCount(3);

    // Fresh-first ordering is the backend's; the view preserves it.
    await expect(rows.nth(0)).toContainText("Gaza");
    await expect(rows.nth(1)).toContainText("Earthquake");
    await expect(rows.nth(2)).toContainText("Aristotle");

    // The two reindexed atoms carry a fresh badge; the baseline one
    // does not. Exactly two badges in the DOM.
    const badges = page.locator('[data-testid="atlas-atom-fresh"]');
    await expect(badges).toHaveCount(2);
    await expect(rows.nth(0).getByTestId("atlas-atom-fresh")).toBeVisible();
    await expect(rows.nth(1).getByTestId("atlas-atom-fresh")).toBeVisible();
    await expect(rows.nth(2).getByTestId("atlas-atom-fresh")).toHaveCount(0);

    // Badge text is a human relative time, not a raw timestamp.
    await expect(rows.nth(0).getByTestId("atlas-atom-fresh")).toContainText("2m ago");
    await expect(rows.nth(1).getByTestId("atlas-atom-fresh")).toContainText("1h ago");
  });
});
