// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Library layout AUDIT PROBE — an instrument, not a gate.
//
// The user reports two defects across the Library subpages: scrolling is
// broken on some, and many subviews have no margin (content squeezed to
// the window edge). Reading CSS can only suggest which; only measuring
// the real, composited layout can *prove* it. This probe drives every
// Library route in a real browser and prints, per route:
//
//   - the effective left/right gutter of the body content (distance from
//     the content-area edge to the nearest painted content), so an
//     inconsistent or zero gutter is a number, not an impression;
//   - every scroll container in the subtree, whether it is actually
//     scrollable, and whether a scrollable one is NESTED inside another
//     (the scroll-trap that makes a wheel gesture do nothing);
//   - every element whose content overflows with NO reachable scroller
//     above it — content the user simply cannot get to.
//
// Run: npx playwright test specs/library-layout-audit.spec.ts --reporter=line
import { test, expect, bootToChat, type Page } from "../fixtures/test-base";
import fs from "node:fs";
import path from "node:path";

const OUT_DIR =
  process.env.LIBRARY_AUDIT_OUT ?? "test-artifacts/library-audit";

const NOW = Math.floor(Date.now() / 1000);

const PLAIN_NB = {
  id: "my-vault",
  name: "Research Vault",
  source_kind: "obsidian",
  doc_count: 1234,
  explorable: false,
  updated_unix: NOW - 3600,
  scope: "local",
  open_conflicts: null,
};

const EXPLORABLE_NB = {
  ...PLAIN_NB,
  id: "explorable-vault",
  name: "Explorable Vault",
  explorable: true,
};

const GOV_NB = {
  ...PLAIN_NB,
  id: "gov-corpus",
  name: "HOA Governance",
  source_kind: "folder",
  explorable: true,
  open_conflicts: 2,
};

function govPayload() {
  return {
    view: {
      rules: [
        {
          id: "claim-a",
          text: "Quiet hours begin at 11 PM.",
          status: { status: "active" },
          citation: { chunk_id: "sec_1" },
        },
        {
          id: "claim-b",
          text: "Quiet hours begin at 10 PM on weeknights.",
          status: { status: "active" },
          citation: { chunk_id: "sec_2" },
        },
      ],
      tensions: [
        {
          id: "edge-1",
          rule_a: "claim-a",
          text_a: "Quiet hours begin at 11 PM.",
          rule_b: "claim-b",
          text_b: "Quiet hours begin at 10 PM on weeknights.",
          why: "When do quiet hours begin now?",
          confidence: 0.9,
          disposition: { disposition: "open" },
        },
      ],
      issues: [],
    },
    section_titles: {
      sec_1: "Charter, Article II",
      sec_2: "Decision — Feb 10",
    },
    section_chunks: {},
    scope_names: {},
    vocabulary: null,
    decisions: {},
    docs_changed_since_build: false,
  };
}

/**
 * A governance payload with `n` open tensions. One conflict fits on a
 * screen and proves nothing; the clipping defect only shows when the
 * panel is taller than `.nb-body`. This is the fixture that makes the
 * failure reproducible instead of theoretical.
 */
function bigGovPayload(n: number) {
  const rules = [];
  const tensions = [];
  const section_titles: Record<string, string> = {};
  for (let i = 0; i < n; i++) {
    rules.push({
      id: `claim-a${i}`,
      text: `Rule ${i}A — quiet hours begin at 11 PM.`,
      status: { status: "active" },
      citation: { chunk_id: `sec_${i}a` },
    });
    rules.push({
      id: `claim-b${i}`,
      text: `Rule ${i}B — quiet hours begin at 10 PM on weeknights.`,
      status: { status: "active" },
      citation: { chunk_id: `sec_${i}b` },
    });
    tensions.push({
      id: `edge-${i}`,
      rule_a: `claim-a${i}`,
      text_a: `Rule ${i}A — quiet hours begin at 11 PM.`,
      rule_b: `claim-b${i}`,
      text_b: `Rule ${i}B — quiet hours begin at 10 PM on weeknights.`,
      why: `Conflict ${i}: when do quiet hours begin now?`,
      confidence: 0.9,
      disposition: { disposition: "open" },
    });
    section_titles[`sec_${i}a`] = `Charter, Article ${i}`;
    section_titles[`sec_${i}b`] = `Decision — item ${i}`;
  }
  return {
    view: { rules, tensions, issues: [] },
    section_titles,
    section_chunks: {},
    scope_names: {},
    vocabulary: null,
    decisions: {},
    docs_changed_since_build: false,
  };
}

async function seed(
  page: Page,
  notebooks: unknown[],
  gov: unknown = govPayload(),
): Promise<void> {
  await page.evaluate(
    ({ nb, gov }) => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("notebook_list", () => nb);
      w.__sovereign_test__.setHandler("governance_get_view", () => gov);
      w.__sovereign_test__.setHandler("notebook_conversations", () => []);
      w.__sovereign_test__.setHandler("lc_list", () => []);
    },
    { nb: notebooks, gov },
  );
}

/**
 * The measurement. Runs in the page; returns plain data.
 *
 * `gutter` deliberately ignores the header band and any full-bleed
 * decoration (rules, dividers, backdrops) and looks only at painted
 * *content* — text nodes and cards — because that is what the eye reads
 * as "the margin".
 */
const MEASURE = `(() => {
  const root = document.querySelector('.app-chrome-content');
  if (!root) return { error: 'no .app-chrome-content' };
  const rootRect = root.getBoundingClientRect();

  const sel = (el) => {
    const parts = [];
    let n = el;
    for (let i = 0; n && i < 4 && n !== root; i++) {
      let s = n.tagName.toLowerCase();
      if (n.className && typeof n.className === 'string') {
        const c = n.className.trim().split(/\\s+/).slice(0, 2).join('.');
        if (c) s += '.' + c;
      }
      parts.unshift(s);
      n = n.parentElement;
    }
    return parts.join(' > ');
  };

  const all = Array.from(root.querySelectorAll('*'));
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return false;
    const cs = getComputedStyle(el);
    return cs.display !== 'none' && cs.visibility !== 'hidden' && cs.opacity !== '0';
  };

  // ── scroll containers ───────────────────────────────────────────
  const scrollerEls = all.filter((el) => {
    const cs = getComputedStyle(el);
    return (cs.overflowY === 'auto' || cs.overflowY === 'scroll') && visible(el);
  });
  const scrollers = scrollerEls.map((el) => {
    const nestedIn = scrollerEls.filter((o) => o !== el && o.contains(el)).map(sel);
    return {
      sel: sel(el),
      clientHeight: el.clientHeight,
      scrollHeight: el.scrollHeight,
      overflows: el.scrollHeight > el.clientHeight + 2,
      nestedIn,
    };
  });

  // ── unreachable overflow: content taller than its box, and the box
  //    (plus every ancestor up to root) clips instead of scrolling ──
  const clipped = [];
  for (const el of all) {
    if (!visible(el)) continue;
    if (el.scrollHeight <= el.clientHeight + 2) continue;
    const cs = getComputedStyle(el);
    if (cs.overflowY === 'auto' || cs.overflowY === 'scroll') continue;
    if (cs.overflowY !== 'hidden' && cs.overflowY !== 'clip') continue;
    // Does any ancestor scroll instead?
    let n = el.parentElement, rescued = false;
    while (n && n !== root.parentElement) {
      const acs = getComputedStyle(n);
      if ((acs.overflowY === 'auto' || acs.overflowY === 'scroll') && n.scrollHeight > n.clientHeight + 2) {
        rescued = true; break;
      }
      n = n.parentElement;
    }
    if (!rescued) {
      clipped.push({ sel: sel(el), clientHeight: el.clientHeight, scrollHeight: el.scrollHeight, hidden: el.scrollHeight - el.clientHeight });
    }
  }

  // ── gutter: leftmost / rightmost painted CONTENT ────────────────
  //
  // Measured twice. The page chrome (header band, tab nav) and the page
  // BODY are two different gutters, and conflating them hides the defect:
  // a detail tab whose header sits at 18px reports 18px even when its body
  // runs flush to the edge. The body figures exclude anything under a
  // header or nav — that is what "the subview has no margin" means.
  const CONTENT = 'p,h1,h2,h3,h4,li,span,button,a,input,textarea,label,td,th,code,pre';
  const inChrome = (el) => !!el.closest('header, nav');
  const gutter = (predicate) => {
    let minLeft = Infinity, maxRight = -Infinity, leftEl = null, rightEl = null;
    for (const el of root.querySelectorAll(CONTENT)) {
      if (!visible(el)) continue;
      if (!predicate(el)) continue;
      const t = (el.textContent || '').trim();
      if (!t) continue;
      const r = el.getBoundingClientRect();
      // ignore things scrolled/clipped outside the viewport
      if (r.bottom < rootRect.top || r.top > rootRect.bottom) continue;
      if (r.left < minLeft) { minLeft = r.left; leftEl = el; }
      if (r.right > maxRight) { maxRight = r.right; rightEl = el; }
    }
    return {
      left: leftEl ? Math.round(minLeft - rootRect.left) : null,
      right: rightEl ? Math.round(rootRect.right - maxRight) : null,
      leftEl: leftEl ? sel(leftEl) : null,
      rightEl: rightEl ? sel(rightEl) : null,
    };
  };
  const anyG = gutter(() => true);
  const bodyG = gutter((el) => !inChrome(el));

  return {
    rootWidth: Math.round(rootRect.width),
    rootHeight: Math.round(rootRect.height),
    gutterLeft: anyG.left,
    gutterRight: anyG.right,
    gutterLeftEl: anyG.leftEl,
    gutterRightEl: anyG.rightEl,
    bodyGutterLeft: bodyG.left,
    bodyGutterRight: bodyG.right,
    bodyGutterLeftEl: bodyG.leftEl,
    bodyGutterRightEl: bodyG.rightEl,
    scrollers,
    clipped,
  };
})()`;

type Measurement = {
  rootWidth: number;
  rootHeight: number;
  gutterLeft: number | null;
  gutterRight: number | null;
  gutterLeftEl: string | null;
  gutterRightEl: string | null;
  bodyGutterLeft: number | null;
  bodyGutterRight: number | null;
  bodyGutterLeftEl: string | null;
  bodyGutterRightEl: string | null;
  scrollers: {
    sel: string;
    clientHeight: number;
    scrollHeight: number;
    overflows: boolean;
    nestedIn: string[];
  }[];
  clipped: {
    sel: string;
    clientHeight: number;
    scrollHeight: number;
    hidden: number;
  }[];
};

const results: Record<string, Measurement> = {};

/** Boot → seed → land on the Library shelf. Every route test starts here. */
async function toShelf(
  page: Page,
  chat: Parameters<typeof bootToChat>[1],
  gov: unknown = govPayload(),
): Promise<void> {
  await page.setViewportSize({ width: 1280, height: 800 });
  await bootToChat(page, chat);
  await seed(page, [PLAIN_NB, EXPLORABLE_NB, GOV_NB], gov);
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-view").waitFor();
}

async function probe(page: Page, name: string): Promise<void> {
  // Let layout settle (transitions on the shelf cards, menu pops).
  await page.waitForTimeout(450);
  const m = (await page.evaluate(MEASURE)) as Measurement;
  results[name] = m;
  fs.mkdirSync(OUT_DIR, { recursive: true });
  await page.screenshot({
    path: path.join(OUT_DIR, `${name}.png`),
    fullPage: false,
  });
  // One file per route: tests run in separate workers, so there is no
  // shared in-process accumulator to summarize from. `report.mjs` joins
  // them back into the gutter/scroll tables.
  fs.writeFileSync(
    path.join(OUT_DIR, `${name}.json`),
    JSON.stringify(m, null, 2),
  );
  // eslint-disable-next-line no-console
  console.log(`\n── ${name} ────────────────────────────────`);
  // eslint-disable-next-line no-console
  console.log(
    `   gutter  chrome L=${m.gutterLeft} R=${m.gutterRight}  |  BODY L=${m.bodyGutterLeft} R=${m.bodyGutterRight}  (body-L via ${m.bodyGutterLeftEl})`,
  );
  for (const s of m.scrollers) {
    const flag = s.nestedIn.length ? ` ⚠ NESTED inside ${s.nestedIn.join(" | ")}` : "";
    // eslint-disable-next-line no-console
    console.log(
      `   scroller ${s.sel}  ${s.scrollHeight}/${s.clientHeight}${s.overflows ? " (overflows)" : ""}${flag}`,
    );
  }
  for (const c of m.clipped) {
    // eslint-disable-next-line no-console
    console.log(
      `   ⚠ UNREACHABLE ${c.sel}  hides ${c.hidden}px (${c.scrollHeight} in ${c.clientHeight})`,
    );
  }

  // ── The gate ───────────────────────────────────────────────────
  //
  // Two invariants, both of which the Library violated in July 2026.

  // 1. Nothing is unreachable. Every surface host in this app is a
  //    bounded clipping box, so a body that fails to establish its own
  //    scroller loses everything past the fold with no scrollbar to
  //    reveal it. This is what hid 2,442px of governance conflicts.
  expect(
    m.clipped,
    `${name}: content is clipped with no reachable scroller — ` +
      m.clipped.map((c) => `${c.sel} hides ${c.hidden}px`).join("; "),
  ).toEqual([]);

  // 2. The body respects the shared gutter. `--gutter` is 28px; allow a
  //    little slack for a scrollbar gutter and sub-pixel rounding, and
  //    skip surfaces whose body is a centred column (gutter > --gutter
  //    is intentional there) or has no measurable body content.
  if (m.bodyGutterLeft !== null && m.bodyGutterLeft < 200) {
    expect(
      m.bodyGutterLeft,
      `${name}: body content sits ${m.bodyGutterLeft}px from the edge; ` +
        `--gutter is 28px (via ${m.bodyGutterLeftEl})`,
    ).toBeGreaterThanOrEqual(24);
  }
}


// One test per route. Isolation is the point: a route that wedges (a
// missing testid, a slow enrich poll) must not blind every route after
// it — which is exactly what a single monolithic walk did on the first
// run, costing the whole measurement for two screenshots.
test.describe("library layout audit", () => {
  test("01 shelf", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await probe(page, "01-shelf");
  });

  test("02 add sheet", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page.getByTestId("library-add").click();
    await page.getByTestId("add-sheet").waitFor();
    await probe(page, "02-add-sheet");
  });

  test("02b add sheet · conversations", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page.getByTestId("library-add").click();
    await page.getByTestId("add-section-imports").click();
    await probe(page, "02b-add-conversations");
  });

  test("02c add sheet · catalog", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page.getByTestId("library-add").click();
    await page.getByTestId("add-section-catalog").click();
    await probe(page, "02c-add-catalog");
  });

  test("03 detail · ask", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page.locator('[data-notebook-id="my-vault"]').getByTestId("notebook-ask").click();
    await page.getByTestId("notebook-detail").waitFor();
    await probe(page, "03-detail-ask");
  });

  test("04 detail · explore (no map)", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page.locator('[data-notebook-id="my-vault"]').getByTestId("notebook-explore").click();
    await page.getByTestId("notebook-detail").waitFor();
    await probe(page, "04-detail-explore-nomap");
  });

  test("05 detail · sources", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page.locator('[data-notebook-id="my-vault"]').getByTestId("notebook-ask").click();
    await page.getByTestId("notebook-detail").waitFor();
    await page.getByTestId("notebook-more").click();
    await page.getByTestId("notebook-tab-sources").click();
    await probe(page, "05-detail-sources");
  });

  test("06 detail · settings", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page.locator('[data-notebook-id="my-vault"]').getByTestId("notebook-ask").click();
    await page.getByTestId("notebook-detail").waitFor();
    await page.getByTestId("notebook-more").click();
    await page.getByTestId("notebook-tab-settings").click();
    await probe(page, "06-detail-settings");
  });

  test("07 detail · explore (atlas)", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page
      .locator('[data-notebook-id="explorable-vault"]')
      .getByTestId("notebook-explore")
      .click();
    await page.getByTestId("notebook-detail").waitFor();
    await page.waitForTimeout(1200);
    await probe(page, "07-detail-explore-atlas");
  });

  test("08 detail · conflicts", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat);
    await page.locator('[data-notebook-id="gov-corpus"]').getByTestId("notebook-ask").click();
    await page.getByTestId("notebook-detail").waitFor();
    await page.getByTestId("notebook-tab-conflicts").click();
    await page.waitForTimeout(600);
    await probe(page, "08-detail-conflicts");
  });

  // The one that matters: a governance notebook with a realistic number
  // of open conflicts. One tension fits on screen and hides the bug.
  test("08b detail · conflicts (12 open)", async ({ sovereignPage: page, chat }) => {
    await toShelf(page, chat, bigGovPayload(12));
    await page.locator('[data-notebook-id="gov-corpus"]').getByTestId("notebook-ask").click();
    await page.getByTestId("notebook-detail").waitFor();
    await page.getByTestId("notebook-tab-conflicts").click();
    await page.waitForTimeout(800);
    await probe(page, "08b-detail-conflicts-many");
  });
});
