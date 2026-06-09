// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Folder-ingest v1 §3.7 — glassbox folder-detail surface.
//
// Pinned here because the spec is explicit: "without [the glassbox
// surfaces], every other v1 feature is a promise the user has to
// take on faith." A regression that drops the negative-space
// "What I don't have" surface, or that hides the sensitivity /
// sync-mode metadata, must fail before reaching the user.
//
// 2026-05-25 navigation rewrite: the standalone "Local Knowledge"
// Settings tab was merged into the "Knowledge" tab, which now
// embeds <LocalKnowledgeSection embedded /> below the catalog
// status. The watched-folder list + detail panel live inside that
// embedded section. The detail panel's enrichment OFF state also
// lost its pipeline picker (the tiered driver is universal across
// corpus shapes, so the {philosophy/referential/literary}_atlas
// radio group was removed); the honest-framing copy moved to
// "Worth it for / Skip it for / Easy to undo". The behaviours each
// assertion guards are unchanged — only the selectors/copy moved.
test.describe("watched-folder detail panel", () => {
  test("renders formats, skipped, failed, sensitivity, sync mode", async ({
    sovereignPage: page,
    chat,
  }) => {
    // bootToChat loads the shim onto window; setHandler must run
    // after navigation so window.__sovereign_test__ is defined.
    await bootToChat(page, chat);

    // Stub the daemon: one watched corpus with a richly-failed
    // mix of files. The list endpoint surfaces it; the details
    // endpoint provides the digest the panel renders.
    const lastSweepUnix = Math.floor(Date.now() / 1000) - 120;
    await page.evaluate((lastSweepUnix) => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (
            cmd: string,
            fn: (args: unknown) => unknown,
          ) => void;
        };
      };
      w.__sovereign_test__.setHandler("lc_watch_list", () => ({
        corpora: [
          {
            corpus_id: "watched-mock-001",
            display_name: "Research dump",
            root_path: "/tmp/research",
            status: {
              kind: "idle",
              last_sweep_unix: lastSweepUnix,
              live_docs: 5,
              tombstones: 1,
            },
            sync_mode: "manual",
            sensitive: true,
            additional_roots_count: 1,
          },
        ],
      }));
      w.__sovereign_test__.setHandler("lc_watch_details", () => ({
        corpus_id: "watched-mock-001",
        display_name: "Research dump",
        root_path: "/tmp/research",
        status: {
          kind: "idle",
          last_sweep_unix: lastSweepUnix,
          live_docs: 5,
          tombstones: 1,
        },
        sync_mode: "manual",
        sensitive: true,
        live_entries: 5,
        formats: { pdf: 3, md: 1, html: 1 },
        skipped_by_extension: { pages: 1, key: 2 },
        failed_files: [
          {
            doc_id: "secret.pdf",
            absolute_path: "/tmp/research/secret.pdf",
            kind: "password_protected",
            reason: "encrypted PDF",
            first_seen_unix: 0,
          },
          {
            doc_id: "broken.pdf",
            absolute_path: "/tmp/research/broken.pdf",
            kind: "corrupt",
            reason: "pdf parser panicked: DeviceN colorspace",
            first_seen_unix: 0,
          },
          {
            doc_id: "broken2.pdf",
            absolute_path: "/tmp/research/broken2.pdf",
            kind: "corrupt",
            reason: "pdf parse error: malformed xref",
            first_seen_unix: 0,
          },
        ],
        tombstones: 1,
        enrichment: { kind: "off" },
        last_sweep_unix: lastSweepUnix,
        roots: [
          {
            idx: 0,
            path: "/tmp/research",
            added_at_unix: 0,
            doc_count: 4,
            primary: true,
          },
          {
            idx: 1,
            path: "/tmp/research-extra",
            added_at_unix: lastSweepUnix - 86_400,
            doc_count: 1,
            primary: false,
          },
        ],
      }));
      w.__sovereign_test__.setHandler(
        "lc_watch_state",
        (args: unknown) => {
          const a = args as { corpusId: string };
          return {
            corpus_id: a.corpusId,
            status: {
              kind: "idle",
              last_sweep_unix: lastSweepUnix,
              live_docs: 5,
              tombstones: 1,
            },
            skipped_by_extension: { pages: 1, key: 2 },
            failed_files: [],
            tombstones: 1,
            live_entries: 5,
          };
        },
      );
      // The card list polls lc_enrichment_status per corpus on a 5s
      // cadence. Stub it benign so the poll doesn't log unstubbed
      // warnings; `{ state: null }` renders no enrichment label.
      w.__sovereign_test__.setHandler("lc_enrichment_status", () => ({
        state: null,
        is_stalled: false,
        fraction_complete: 0,
      }));
    }, lastSweepUnix);

    // Open Settings → Knowledge tab where the watched-folder surface
    // now lives (the former "Local Knowledge" tab was merged in).
    await page.getByTestId("nav-settings").click();
    await page.locator(".cfg").waitFor();
    // The Settings panel uses `.cfg` as its root and `.toc-item`
    // buttons for the left rail. Exact-text match on Knowledge.
    const knowledgeTab = page
      .locator(".cfg-toc .toc-item")
      .filter({ hasText: /^Knowledge$/ });
    await expect(knowledgeTab).toBeVisible();
    await knowledgeTab.click();

    // The watched-folder list should render the stubbed corpus
    // with the multi-root, Manual-sync, and Sensitive badges.
    const card = page.locator(".card").filter({ hasText: "Research dump" });
    await expect(card).toBeVisible();
    await expect(card).toContainText("+1 folder");
    await expect(card).toContainText("Manual sync");
    await expect(card).toContainText("Sensitive");

    // Click into the detail panel.
    await card.locator('button:has-text("Details")').click();

    // §3.1 acceptance: per-root section lists primary + additional
    // with their doc counts. Primary tagged "primary"; additional
    // gets a "Detach" affordance.
    await expect(
      page.locator(".section-title", { hasText: "Folders" }),
    ).toBeVisible();
    const rootList = page.locator(".root-list .root");
    await expect(rootList).toHaveCount(2);
    await expect(rootList.nth(0)).toContainText("/tmp/research");
    await expect(rootList.nth(0)).toContainText("primary");
    await expect(rootList.nth(0)).toContainText("4 docs");
    await expect(rootList.nth(1)).toContainText("/tmp/research-extra");
    await expect(rootList.nth(1)).toContainText("1 doc");
    // Detach button only on additional root.
    await expect(rootList.nth(0).locator('button:has-text("Detach")')).toHaveCount(0);
    await expect(rootList.nth(1).locator('button:has-text("Detach")')).toBeVisible();

    // §3.7 acceptance: indexed-format counts visible.
    await expect(page.locator(".section-title", { hasText: "Indexed formats" })).toBeVisible();
    await expect(page.locator(".bucket")).toContainText([".pdf", ".md", ".html"]);

    // The "What I don't have" surface — non-negotiable.
    await expect(
      page.locator(".section-title", { hasText: "What I don't have" }),
    ).toBeVisible();
    // Failed-extraction groups: 1 password-protected + 2 corrupt
    // (sorted by group size descending, so the corrupt group is
    // first).
    const groups = page.locator(".group");
    await expect(groups.first()).toContainText("Couldn't be parsed");
    await expect(groups.first().locator(".group-count")).toHaveText("2");
    await expect(
      groups.filter({ hasText: "Password-protected" }),
    ).toBeVisible();

    // Unsupported formats group aggregates the skipped count
    // (1 .pages + 2 .key = 3).
    const unsupported = groups.filter({ hasText: "Unsupported file formats" });
    await expect(unsupported).toBeVisible();
    await expect(unsupported.locator(".group-count")).toHaveText("3");

    // Sensitivity tag visible in the summary metric grid.
    await expect(
      page.locator(".sensitive-tag", { hasText: "Sensitive" }),
    ).toBeVisible();

    // Sync-mode metric reads "Manual".
    await expect(
      page.locator(".metric", { hasText: "Sync mode" }),
    ).toContainText("Manual");

    // §3.3 acceptance: enrichment surface in Off state shows honest
    // framing (when-it-works-well / when-it-works-less-well /
    // recoverability), the cost estimate, and the Enable button.
    //
    // Note: the pipeline picker (3-radio atlas chooser + legend) was
    // removed when the tiered driver became universal across corpus
    // shapes — the picker assertion is no longer expressible. The
    // honest-framing copy that the picker accompanied now lives in
    // the three-item .honest-list ("Worth it for" / "Skip it for" /
    // "Easy to undo"), which this still pins.
    await expect(
      page.locator(".enrichment").locator(".section-title", { hasText: "Atlas enrichment" }),
    ).toBeVisible();
    await expect(page.locator(".honest-list")).toContainText("Worth it for");
    await expect(page.locator(".honest-list")).toContainText("Skip it for");
    await expect(page.locator(".honest-list")).toContainText("Easy to undo");
    await expect(page.locator(".cost")).toContainText("Estimated build time");
    await expect(
      page.locator(".enrichment button.primary", { hasText: "Enable enrichment" }),
    ).toBeEnabled();

    // Back returns to the folder list — onClose flips
    // LocalKnowledgeSection back to mode:idle and re-renders the list.
    // (This was briefly un-clickable: SettingsPanel's embedded-mode
    // `.lk-section .head { display:none }` rule also matched
    // WatchedFolderDetail's `.detail > .head`, hiding this button. Fixed
    // by narrowing that rule to a direct-child selector — see
    // SettingsPanel.svelte.)
    const back = page.locator(".back");
    await expect(back).toHaveText(/Back to folders/);
    await back.click();
    // Detail panel gone; the folder list (the "Research dump" card) is
    // back.
    await expect(page.locator(".detail")).toHaveCount(0);
    await expect(
      page.locator(".card").filter({ hasText: "Research dump" }),
    ).toBeVisible();
  });

  test("renders enrichment Building state with progress", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Stub the daemon so the corpus is mid-build with a partial
    // progress counter. The UI must render the phase + counter +
    // progress bar, and surface a Cancel & disable button.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("lc_watch_list", () => ({
        corpora: [
          {
            corpus_id: "watched-mock-002",
            display_name: "Building corpus",
            root_path: "/tmp/building",
            status: { kind: "idle", last_sweep_unix: 0, live_docs: 5, tombstones: 0 },
            sync_mode: "continuous",
            sensitive: false,
            additional_roots_count: 0,
          },
        ],
      }));
      w.__sovereign_test__.setHandler("lc_watch_details", () => ({
        corpus_id: "watched-mock-002",
        display_name: "Building corpus",
        root_path: "/tmp/building",
        status: { kind: "idle", last_sweep_unix: 0, live_docs: 5, tombstones: 0 },
        sync_mode: "continuous",
        sensitive: false,
        live_entries: 5,
        formats: { md: 5 },
        skipped_by_extension: {},
        failed_files: [],
        tombstones: 0,
        enrichment: {
          kind: "building",
          pipeline_id: "philosophy_atlas",
          phase: "phase1: chapter-3",
          current: 3,
          total: 8,
          started_at_unix: Math.floor(Date.now() / 1000) - 30,
        },
        last_sweep_unix: 0,
        roots: [
          { idx: 0, path: "/tmp/building", added_at_unix: 0, doc_count: 5, primary: true },
        ],
      }));
      w.__sovereign_test__.setHandler("lc_enrichment_status", () => ({
        state: null,
        is_stalled: false,
        fraction_complete: 0,
      }));
    });

    await page.getByTestId("nav-settings").click();
    await page.locator(".cfg").waitFor();
    await page
      .locator(".cfg-toc .toc-item")
      .filter({ hasText: /^Knowledge$/ })
      .click();
    await page
      .locator(".card")
      .filter({ hasText: "Building corpus" })
      .locator('button:has-text("Details")')
      .click();

    // Building state: pipeline name, phase, counter, progress bar.
    await expect(page.locator(".enrichment")).toContainText("philosophy_atlas");
    await expect(page.locator(".phase")).toContainText("phase1: chapter-3");
    await expect(page.locator(".counter")).toContainText("3 / 8");
    await expect(page.locator(".progress-bar")).toBeVisible();
    // Cancel & disable surfaces during a build.
    await expect(
      page.locator(".enrichment button", { hasText: "Cancel & disable" }),
    ).toBeVisible();
    // Enable / honest-framing UI gone in Building (only the Off
    // state renders the honest-list + cost + Enable button).
    await expect(page.locator(".honest-list")).toHaveCount(0);
    await expect(
      page.locator(".enrichment button", { hasText: "Enable enrichment" }),
    ).toHaveCount(0);
  });

  test("renders enrichment Complete with stale-docs callout", async ({
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
      w.__sovereign_test__.setHandler("lc_watch_list", () => ({
        corpora: [
          {
            corpus_id: "watched-mock-003",
            display_name: "Complete corpus",
            root_path: "/tmp/done",
            status: { kind: "idle", last_sweep_unix: 0, live_docs: 12, tombstones: 0 },
            sync_mode: "continuous",
            sensitive: false,
            additional_roots_count: 0,
          },
        ],
      }));
      w.__sovereign_test__.setHandler("lc_watch_details", () => ({
        corpus_id: "watched-mock-003",
        display_name: "Complete corpus",
        root_path: "/tmp/done",
        status: { kind: "idle", last_sweep_unix: 0, live_docs: 12, tombstones: 0 },
        sync_mode: "continuous",
        sensitive: false,
        // 12 live now; atlas was built at 10. UI should call out
        // that 2 new docs have landed since the last build.
        live_entries: 12,
        formats: { md: 12 },
        skipped_by_extension: {},
        failed_files: [],
        tombstones: 0,
        enrichment: {
          kind: "complete",
          pipeline_id: "philosophy_atlas",
          built_at_unix: Math.floor(Date.now() / 1000) - 3600,
          doc_count: 10,
          current_doc_count: 12,
        },
        last_sweep_unix: 0,
        roots: [
          { idx: 0, path: "/tmp/done", added_at_unix: 0, doc_count: 12, primary: true },
        ],
      }));
      w.__sovereign_test__.setHandler("lc_enrichment_status", () => ({
        state: null,
        is_stalled: false,
        fraction_complete: 0,
      }));
    });

    await page.getByTestId("nav-settings").click();
    await page.locator(".cfg").waitFor();
    await page
      .locator(".cfg-toc .toc-item")
      .filter({ hasText: /^Knowledge$/ })
      .click();
    await page
      .locator(".card")
      .filter({ hasText: "Complete corpus" })
      .locator('button:has-text("Details")')
      .click();

    // Complete state: pipeline name, stale-docs callout, Rebuild +
    // Disable affordances.
    await expect(page.locator(".enrichment")).toContainText("philosophy_atlas");
    await expect(page.locator(".stale")).toContainText("2 new documents");
    await expect(
      page.locator(".enrichment button", { hasText: "Rebuild" }),
    ).toBeVisible();
    await expect(
      page.locator(".enrichment button", { hasText: "Disable" }),
    ).toBeVisible();
  });
});
