// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// The top-level shape of the app, asserted where a user meets it: the nav
// rail. The requirement is that the application is organised around user
// INTENT — Ask, Library, Reflect, Workshop, Settings — rather than around
// system structure, and that Ask is where you land.
//
// Nothing pinned this before. The rail's `marks` array is five literals in a
// component; a surface could be dropped, renamed to an internal noun ("Atlas",
// "Corpora", "Enrichment"), or reordered, and every other spec would stay
// green because they reach their surface by testid and never look at the set.
// The set IS the requirement.

test.describe("nav rail", () => {
  // The tag must stay on ONE line with the title: the generator that writes
  // quality/conformance/desktop.toml matches `test("...", { tag: [...] }` with
  // a single-line regex, and a wrapped call is silently not a claim.
  // eslint-disable-next-line prettier/prettier
  test("is the five intent surfaces, in order, and lands on Ask", { tag: ["@UI-4"] }, async ({ sovereignPage: page, chat }) => {
      await bootToChat(page, chat);

      const marks = page.locator(".nav-rail .mark");
      await expect(marks).toHaveCount(5);

      // Order is part of the shape: Ask first because it is the landing
      // surface, Settings last because it is not a place you go to work.
      const testids = await marks.evaluateAll((els) =>
        els.map((el) => el.getAttribute("data-testid")),
      );
      expect(testids).toEqual([
        "nav-ask",
        "nav-library",
        "nav-reflect",
        "nav-workshop",
        "nav-settings",
      ]);

      // Labels are what a newcomer actually reads. A rename to a system noun
      // is the failure this half catches — the testid could survive it.
      const labels = await marks.evaluateAll((els) =>
        els.map((el) => el.getAttribute("aria-label")),
      );
      expect(labels).toEqual([
        "Ask",
        "Library",
        "Reflect",
        "Workshop",
        "Settings",
      ]);

      // "Ask (grounded conversation, the landing surface)" — asserted as the
      // two things a user would notice: the chat surface is what is on screen,
      // and the rail says so.
      await expect(page.locator(".chat-view")).toBeVisible();
      await expect(page.getByTestId("nav-ask")).toHaveAttribute(
        "aria-current",
        "page",
      );
      // Exactly one current surface — a rail with two would mean the active
      // mode and the rendered surface had drifted apart.
      await expect(page.locator('.nav-rail [aria-current="page"]')).toHaveCount(
        1,
      );

      // The rail is a navigation landmark, not a row of buttons, so a screen
      // reader user can jump to it.
      await expect(page.locator("nav.nav-rail")).toHaveAttribute(
        "aria-label",
        "Main navigation",
      );
  });
});
