// SPDX-License-Identifier: AGPL-3.0-or-later
// Demo-mode test base: the real-mode fixture (real app, real bridge,
// real daemon) plus the capture layer.
//
// It extends `real/test-base-real.ts` rather than forking it, so the
// pageerror gate and the fatal-Svelte-console gate apply to every frame
// we film. That is not incidental: it means a beat cannot be captured
// while the app is throwing in the background. Footage of a broken build
// is worse than no footage.
import { test as realTest, expect } from "../real/test-base-real";
import { installCursor } from "./cursor";
import type { Page } from "@playwright/test";

export const test = realTest.extend<{ demoPage: Page }>({
  demoPage: async ({ sovereignPage }, use) => {
    // addInitScript, so the overlay survives the mesh-app navigations in
    // B3 (which leave the app shell entirely).
    await installCursor(sovereignPage);
    await use(sovereignPage);
  },
});

export { expect };
export { realBootToChat } from "../real/test-base-real";
