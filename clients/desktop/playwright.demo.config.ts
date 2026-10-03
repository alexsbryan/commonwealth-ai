// SPDX-License-Identifier: AGPL-3.0-or-later
// Demo-capture config: the product reel, as an executable spec.
// Beats live in tests/e2e/demo/ — see tests/e2e/demo/DEMO_BEATS.md.
//
// A third config rather than a project inside playwright.real.config.ts,
// for the same reason real mode isn't a project inside the synthetic
// suite: demo mode owns a different world (attach to the operator's LIVE
// daemon, not a fixture-scoped one), a different clock (beats hold on
// frames; tests don't), and a different failure meaning (a red beat
// means "don't ship this footage"). None of that should leak into a
// correctness run.
//
//   npm run demo                    # capture every beat
//   npm run demo -- --grep b1       # one beat
//   npm run demo:export             # trim + encode + gif
//
// Geometry is fixed and deliberate: 1280×800, recorded at 1280×800.
// Identical framing on every take is most of what makes a multi-clip reel
// look intentional.
//
// The video size MUST equal the viewport. Playwright's screencast captures
// CSS pixels, not device pixels, and it only ever scales the picture DOWN
// to fit `size` — it never scales up. So `size: viewport × 2` does not buy
// a 2× master; it letterboxes the page into the top-left quadrant and pads
// the other three quarters with dead grey. (Observed on the first b9 take:
// a 2560×1600 file with 1280×800 of content in the corner.)
//
// deviceScaleFactor stays at 2 because page.screenshot() IS device-pixel —
// failure shots and any still we pull for docs come out at 2560×1600.
import { defineConfig, devices } from "@playwright/test";

const num = (v: string | undefined, d: number) => (v ? Number(v) : d);

const VIEWPORT = {
  width: num(process.env.SOVEREIGN_DEMO_WIDTH, 1280),
  height: num(process.env.SOVEREIGN_DEMO_HEIGHT, 800),
};
const SCALE = num(process.env.SOVEREIGN_DEMO_SCALE, 2);

export default defineConfig({
  testDir: "./tests/e2e/demo",
  testMatch: "**/*.demo.spec.ts",
  globalSetup: "./tests/e2e/demo/global-setup.ts",
  // The real teardown kills the app + managed daemon and is mode-agnostic.
  globalTeardown: "./tests/e2e/real/global-teardown.ts",

  // One shared backend, one shared model, and video files whose names must
  // map 1:1 to beats. Serial, always.
  fullyParallel: false,
  workers: 1,
  // No retries: a retry would leave two videos for one beat and the
  // exporter would have to guess. A flaky beat is a beat to fix.
  retries: 0,
  forbidOnly: !!process.env.CI,
  reporter: [
    ["list"],
    ["json", { outputFile: "test-artifacts/demo/report.json" }],
  ],
  // Beats type at human cadence and wait on real 35B synthesis. B4's
  // agentic authoring loop is the long pole.
  timeout: num(process.env.SOVEREIGN_DEMO_TIMEOUT_MS, 900_000),
  expect: { timeout: 45_000 },

  // Videos land here, one per beat. Playwright wipes outputDir at run
  // start — which is why the ledger lives one level up in
  // test-artifacts/demo/, not inside it.
  outputDir: "test-artifacts/demo/video",

  use: {
    baseURL: "http://localhost:5173",
    trace: "off",
    screenshot: "only-on-failure",
    // Real motion is the product here — never reduce it.
    contextOptions: { reducedMotion: "no-preference" },
  },

  // Geometry lives in the PROJECT, after the device spread: project-level
  // `use` merges over the top level, so devices["Desktop Chrome"] (1280×720
  // at dsf 1) would silently clobber it the other way round.
  projects: [
    {
      name: "demo",
      use: {
        ...devices["Desktop Chrome"],
        viewport: VIEWPORT,
        deviceScaleFactor: SCALE,
        video: { mode: "on", size: VIEWPORT },
      },
    },
  ],

  webServer: {
    command: "npm run dev",
    url: "http://localhost:5173",
    reuseExistingServer: true,
    timeout: 60_000,
    stdout: "pipe",
    stderr: "pipe",
  },
});
