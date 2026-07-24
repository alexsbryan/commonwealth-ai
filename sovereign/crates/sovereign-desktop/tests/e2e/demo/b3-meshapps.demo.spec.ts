// SPDX-License-Identifier: AGPL-3.0-or-later
// B3 — Mesh Apps: the Enron task force, and today's news.
//
// The claim is that a corpus is a substrate other people can build
// applications on. So this beat must film the REAL thing: the shipped
// bundle, over the real host bridge, against the operator's real corpus.
//
// The synthetic specs (tests/e2e/specs/meshapp-*.spec.ts) mock
// `window.meshApp` on purpose — they verify the bundle's story logic
// headlessly. Filming that would be filming a mock. Here we instead
// serve the same bundle and inject the SHIPPED host shim
// (src-tauri/src/meshapp_shim.js, the file Rust `include_str!`s), whose
// only transport is `__TAURI_INTERNALS__.invoke` — which the real-mode
// fixture already points at the live command bridge. So the bundle in
// frame is byte-identical to the one the app opens in its sandboxed
// window, and every number on screen came from the daemon.
//
// The desktop opens mesh apps in a separate Tauri window, which
// Playwright cannot reach; this is the equivalent surface, not a
// substitute for it. Noted on the record rather than glossed.
import path from "node:path";
import { fileURLToPath } from "node:url";
import { beatTest, expect, demoClick } from "./beat";
import { atlasStats, hasCorpus } from "./preflight";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SHIM = path.resolve(__dirname, "../../../src-tauri/src/meshapp_shim.js");

const fmt = (n: number) => Number(n).toLocaleString("en-US");

// ─────────────────────────────────────────────────────────────────────
// B3a — Enron
// ─────────────────────────────────────────────────────────────────────
beatTest(
  {
    id: "b3-enron",
    title: "A task force over 3,722 emails nobody read",
    claim:
      "A corpus is a substrate: same data, purpose-built lens, and every pixel " +
      "dereferences to the document it came from.",
    gifPadSec: 1.0,
    gifMark: "reconciliation-reveal",
  },
  async ({ page, bridge, run }) => {
    const CORPUS = "enron-sample-multi-wide";
    run.requireOrSkip(await hasCorpus(CORPUS), `the \`${CORPUS}\` corpus is not hosted`);
    const stats = await atlasStats(bridge, CORPUS);
    run.requireOrSkip(
      stats !== null,
      `\`${CORPUS}\` has chunks but no built atlas — mesh-app ops read the atlas, ` +
        `not the chunk index. Enrich it before capturing B3.`,
    );

    await page.addInitScript({ path: SHIM });
    await page.goto("/meshapp/enron/index.html");

    await expect(page.locator("#loading")).toBeHidden({ timeout: 60_000 });
    await expect(
      page.locator("#error"),
      "the bundle must not fall into its error state (missing grant / missing atlas)",
    ).toBeHidden();
    run.mark("open");
    await run.dwell(1600);

    // ── Numeric honesty ──
    // Every headline number in the scale banner is re-read from the same
    // op the bundle called and compared. A demo that displays a number
    // the backend doesn't report is a failed beat, not a design choice.
    const banner = page.locator("#banner");
    await expect(banner).toBeVisible({ timeout: 30_000 });
    const bannerText = ((await banner.textContent()) ?? "").replace(/\s+/g, " ");
    for (const key of ["documents", "entities", "edges", "reconciled_merges", "claims"]) {
      const value = Number(stats![key] ?? NaN);
      expect(Number.isFinite(value), `atlas stats must report ${key}`).toBe(true);
      expect(
        bannerText,
        `scale banner must show the daemon's ${key} (${fmt(value)}), not a hand-typed number`,
      ).toContain(fmt(value));
    }
    run.mark("scale-banner");
    run.note(
      `banner verified against meshapp_corpus_stats: ` +
        `${fmt(Number(stats!.documents))} docs · ${fmt(Number(stats!.entities))} entities · ` +
        `${fmt(Number(stats!.reconciled_merges))} merges`,
    );
    await run.dwell(2600);

    // ── The graph settles, then we open one node. ──
    const nodes = page.locator("#map svg circle");
    await expect(nodes.first()).toBeVisible({ timeout: 45_000 });
    await run.dwell(2800); // force layout settling — let it come to rest on camera
    run.mark("graph");

    await demoClick(page, nodes.first(), { settleMs: 500 });
    await expect(
      page.locator("#d-name"),
      "clicking a node must open its cited detail panel",
    ).toContainText(/\S/, { timeout: 20_000 });
    run.mark("entity-detail");
    await run.dwell(2600);

    // ── Reconciliation: the reveal. ──
    // Three spellings of one company, merged, with the surface forms
    // shown. This is the beat's strongest single frame.
    const merges = page.locator("#merges .merge");
    if ((await merges.count()) > 0) {
      await merges.first().scrollIntoViewIfNeeded();
      await run.caption("Same company, three spellings. Reconciled.", 3200);
      run.mark("reconciliation-reveal");
      const first = ((await merges.first().textContent()) ?? "").replace(/\s+/g, " ");
      run.note(`reconciliation reveal: ${first.slice(0, 140)}`);
      await run.dwell(3400);
    } else {
      run.note(
        `no reconciliation merges rendered — atlas reports ` +
          `${stats!.reconciled_merges} merges but the panel is empty`,
      );
      expect(
        Number(stats!.reconciled_merges ?? 0),
        "if the atlas reports merges, the panel must render them",
      ).toBe(0);
    }

    // ── Timeline: the collapse, month by month. ──
    const tl = page.locator("#timeline .tl-col");
    if ((await tl.count()) > 0) {
      await tl.first().scrollIntoViewIfNeeded();
      await run.dwell(900);
      await demoClick(page, tl.nth(Math.min(3, (await tl.count()) - 1)), { settleMs: 500 });
      await expect(page.locator("#timeline-detail")).toContainText(/\S/, {
        timeout: 20_000,
      });
      run.mark("timeline");
      await run.dwell(2600);
    } else {
      run.note("timeline empty — no dated documents in this corpus");
    }

    // ── Drill all the way down to a real email. ──
    const readBtn = page.getByRole("button", { name: /read (the source )?email/i }).first();
    if (await readBtn.isVisible().catch(() => false)) {
      await run.caption("Down to the source email.", 2800);
      await demoClick(page, readBtn, { settleMs: 500 });
      run.mark("source-email");
      await run.dwell(3600);
      run.note("drilled through to a source email");
    } else {
      run.note("no read-email affordance in view — source drill-down not filmed");
    }
    await run.park();
  },
);

// ─────────────────────────────────────────────────────────────────────
// B3b — Today
// ─────────────────────────────────────────────────────────────────────
beatTest(
  {
    id: "b3-today",
    title: "Current events, on your machine, with no feed and no server",
    claim:
      "The same machinery reads the news: ingested locally, on a daily tick, with " +
      "no server in the loop and no telemetry going out.",
    gifPadSec: 1.0,
    gifMark: "story-open",
  },
  async ({ page, bridge, run }) => {
    const CORPUS = "wikipedia-newsworthy";
    run.requireOrSkip(await hasCorpus(CORPUS), `the \`${CORPUS}\` corpus is not hosted`);
    run.requireOrSkip(
      (await atlasStats(bridge, CORPUS)) !== null,
      `\`${CORPUS}\` has no built atlas — the Today feed reads the atlas`,
    );

    await page.addInitScript({ path: SHIM });
    await page.goto("/meshapp/today/index.html");

    await expect(page.locator("#loading")).toBeHidden({ timeout: 60_000 });
    await expect(page.locator("#error")).toBeHidden();
    run.mark("open");

    const feed = page.locator("#feed");
    await expect(feed).toBeVisible({ timeout: 30_000 });
    await expect(
      feed,
      "the feed must render ingested days, not the empty-corpus notice",
    ).not.toContainText(/No days ingested yet/i);
    run.mark("feed");
    await run.dwell(2600);

    // Freshness is the load-bearing claim here — "today" that is three
    // weeks old is a different product. Surface whatever it says.
    const freshness = page.locator("#freshness");
    if (await freshness.isVisible().catch(() => false)) {
      run.note(`freshness: "${((await freshness.textContent()) ?? "").trim()}"`);
    }

    // Open a story.
    const firstStory = feed.locator("li, article, .item, button").first();
    if (await firstStory.isVisible().catch(() => false)) {
      await demoClick(page, firstStory, { settleMs: 500 });
      await expect(page.locator("#p-title")).toContainText(/\S/, { timeout: 20_000 });
      await expect(
        page.locator("#p-body"),
        "an opened story must render its body from the local corpus",
      ).toContainText(/\S/);
      run.mark("story-open");
      run.note(`opened: "${((await page.locator("#p-title").textContent()) ?? "").trim()}"`);
      await run.park();
      await run.dwell(3400);
    } else {
      run.note("no story rows rendered in the feed — story panel not filmed");
    }
  },
);
