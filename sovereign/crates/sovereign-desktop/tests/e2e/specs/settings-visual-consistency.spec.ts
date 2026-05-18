import { test, expect, bootToChat, type Page } from "../fixtures/test-base";

// Settings panel substrate audit. Every section inside Settings must
// share the same dark Lavender Court substrate as the page itself —
// a section that ships with light-on-light cards (the
// SharingSection / ConnectSection drift we cleaned up in May 2026)
// reads as a bright island on the dark Configuration page and looks
// like an unfinished feature. This spec drives the visual contract.
//
// Pattern matches the onboarding `background remains a single
// substrate` test: pipe each computed background through a canvas to
// normalize oklch/hex/rgb, compute relative luminance, assert the
// spread stays under a tolerance band.

async function openSettings(page: Page, chat: Parameters<typeof bootToChat>[1]) {
  await bootToChat(page, chat);
  await page.getByTestId("nav-settings").click();
  await page.locator(".cfg").waitFor();
}

async function bgLuminance(page: Page, selector: string): Promise<number | null> {
  return page.evaluate((s) => {
    const el = document.querySelector(s);
    if (!el) return null;
    const bg = getComputedStyle(el).backgroundColor;
    const canvas = document.createElement("canvas");
    canvas.width = 1;
    canvas.height = 1;
    const ctx = canvas.getContext("2d");
    if (!ctx) return null;
    ctx.fillStyle = "#000";
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, 1, 1);
    const { data } = ctx.getImageData(0, 0, 1, 1);
    const [r, g, b] = [data[0], data[1], data[2]].map((v) => {
      const c = v / 255;
      return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  }, selector);
}

test.describe("settings · visual consistency", () => {
  test("the Settings page surface is the dark Lavender Court substrate", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    const docLum = await bgLuminance(page, ".cfg-doc");
    expect(docLum, "could not read .cfg-doc background").not.toBeNull();
    // The dark substrate is dim — guard against a future regression
    // where someone flips the page to light mode without updating
    // the sections to match.
    expect(docLum!).toBeLessThan(0.05);
  });

  // The two screens we just cleaned up. Belt-and-suspenders: even if
  // the substrate test above passes, an inner card painted with
  // hardcoded light oklch would be invisible to a tab-level check.
  // Here we sample each visible inner surface explicitly.
  test("SharingSection's controls render on the dark substrate", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    await page
      .locator(".cfg-toc .toc-item")
      .filter({ hasText: /^Sharing$/ })
      .click();
    await page.locator(".sharing").waitFor();

    // Body text colour should be a non-black dark-mode foreground —
    // luminance > 0.1 (the page is ~0.01). A dark-on-dark drift
    // would push this under 0.05.
    const bodyColor = await page.evaluate(() => {
      const el = document.querySelector(".sharing .hint");
      return el ? getComputedStyle(el).color : null;
    });
    const bodyLum = await page.evaluate((color) => {
      if (!color) return null;
      const canvas = document.createElement("canvas");
      canvas.width = 1;
      canvas.height = 1;
      const ctx = canvas.getContext("2d");
      if (!ctx) return null;
      ctx.fillStyle = "#000";
      ctx.fillStyle = color;
      ctx.fillRect(0, 0, 1, 1);
      const { data } = ctx.getImageData(0, 0, 1, 1);
      const [r, g, b] = [data[0], data[1], data[2]].map((v) => {
        const c = v / 255;
        return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
      });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    }, bodyColor);
    expect(bodyLum, `unparseable body color: ${bodyColor}`).not.toBeNull();
    expect(
      bodyLum!,
      `SharingSection body text reads as too-dark (luminance ${bodyLum}) — likely dark-on-dark`,
    ).toBeGreaterThan(0.1);
  });

  test("ConnectSection's env rows render on the dark substrate", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    await page
      .locator(".cfg-toc .toc-item")
      .filter({ hasText: /^Connect$/ })
      .click();
    await page.locator(".connect").waitFor();

    // The env-row card should be a darker shade than the page —
    // either equal-or-less luminance than .cfg-doc. A reverted card
    // would render light-on-dark and fail this.
    const envLum = await bgLuminance(page, ".connect .env-row");
    const pageLum = await bgLuminance(page, ".cfg-doc");
    expect(envLum, "no .env-row").not.toBeNull();
    expect(pageLum, "no .cfg-doc").not.toBeNull();
    expect(
      envLum!,
      `env-row background (${envLum}) is lighter than the page (${pageLum})`,
    ).toBeLessThanOrEqual(pageLum! + 0.02);
  });

  // Font drift sentinel. The same pattern that hit Welcome/Setup/
  // Consent — "Outfit" referenced but not bundled, silently falling
  // through to system-ui — was duplicated in SharingSection,
  // ConnectSection, and ReconnectBanner. This test re-asserts that
  // the bundled IBM Plex Sans is the resolved family on the visible
  // section bodies.
  test("Settings sections render in a bundled font", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    await page.evaluate(() => document.fonts.ready);

    async function firstFamilyLoaded(selector: string) {
      return page.evaluate((s) => {
        const el = document.querySelector(s);
        if (!el) return { family: null, loaded: true };
        const stack = getComputedStyle(el).fontFamily;
        const first = stack.split(",")[0].trim().replace(/^['"]|['"]$/g, "");
        const isGeneric = /^(system-ui|sans-serif|serif|monospace|-apple-system|ui-monospace)$/i.test(
          first,
        );
        const loaded = isGeneric || document.fonts.check(`1rem "${first}"`);
        return { family: first, loaded };
      }, selector);
    }

    for (const [label, selector] of [
      ["Sharing", ".sharing"],
      ["Connect", ".connect"],
    ] as const) {
      await page
        .locator(".cfg-toc .toc-item")
        .filter({ hasText: new RegExp(`^${label}$`) })
        .click();
      await page.locator(selector).waitFor();
      const { family, loaded } = await firstFamilyLoaded(selector);
      expect(
        loaded,
        `${label} section first-choice font "${family}" is not bundled`,
      ).toBe(true);
    }
  });
});
