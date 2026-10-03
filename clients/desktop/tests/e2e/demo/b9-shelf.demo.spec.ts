// SPDX-License-Identifier: AGPL-3.0-or-later
// B9 — The shelf, and the ask.
//
// Everything you just saw is one shelf in a library you own, and we've
// barely started.
//
// The point of this beat — beyond the outro — is that the numbers in the
// closing caption are READ FROM THE DAEMON at capture time and injected
// into the overlay. They are not typed into a design file, and they
// cannot silently drift from what the product does. If the caption says
// 2.2 million passages, the machine reported 2.2 million passages, in
// this run, seconds before the frame was written.
//
// That is the whole posture of this suite applied to the one place demo
// reels lie most casually: the closing stat card.
import { beatTest, expect, demoClick, glideToLocator } from "./beat";
import { realBootToChat } from "./demo-base";
import { shelfFacts } from "./preflight";

const fmt = (n: number) => Math.round(n).toLocaleString("en-US");

beatTest(
  {
    id: "b9-shelf",
    title: "One shelf in a library you own",
    claim:
      "Everything shown is one shelf of a library that's yours, on hardware that's " +
      "yours — and it's early. Come build the rest.",
    gifPadSec: 1.4,
    gifMark: "stats",
  },
  async ({ page, run }) => {
    const facts = await shelfFacts();
    run.note(
      `live: ${facts.corpora} corpora · ${fmt(facts.chunks)} chunks · ` +
        `${facts.peersOnline}/${facts.peersTotal} peers on "${facts.meshName}" · ` +
        `${facts.pooledVramGb} GB pooled VRAM`,
    );
    // A closing card built on zeros is worse than no closing card.
    expect(facts.corpora, "the shelf must have something on it").toBeGreaterThan(0);
    expect(facts.chunks, "the chunk count must be live, not zero").toBeGreaterThan(0);

    await realBootToChat(page);
    await run.dwell(700);
    await demoClick(page, page.getByTestId("nav-library"), { settleMs: 600 });

    const library = page.getByTestId("library-view");
    await expect(library).toBeVisible({ timeout: 30_000 });
    const cards = page.getByTestId("notebook-card");
    await expect(cards.first()).toBeVisible({ timeout: 20_000 });
    run.mark("shelf");
    await run.dwell(1800);

    // Slow pan down the shelf. Scrolling rather than clicking keeps the
    // cursor still — the eye should be on the breadth, not on a pointer.
    // The glide first is load-bearing, not decorative: mouse.wheel scrolls
    // whatever is UNDER the pointer, and after clicking the rail the
    // pointer is still on the rail, which doesn't scroll.
    await glideToLocator(page, cards.first());
    await run.dwell(400);
    for (let i = 0; i < 6; i += 1) {
      await page.mouse.wheel(0, 260);
      await run.dwell(650);
    }
    await run.dwell(1200);
    run.mark("pan");

    // ── The ask. Numbers live-read above; nothing here is hand-typed. ──
    await run.caption(
      `${facts.corpora} notebooks · ${fmt(facts.chunks)} passages · ` +
        `${facts.peersOnline} machines · ${fmt(facts.pooledVramGb)} GB pooled`,
      4200,
    );
    await run.dwell(4600);
    run.mark("stats");

    await run.caption("And this is the part we've built. There's so much more.", 3800);
    await run.dwell(4000);

    await run.caption("AI for the people, by the people. Come build it with us.", 4600);
    run.mark("ask");
    await run.dwell(5000);
  },
);
