// SPDX-License-Identifier: AGPL-3.0-or-later
// Real-mode e2e config: specs under tests/e2e/real/ drive the Vite-
// served frontend against a REAL sovereign-desktop process (command
// bridge, hermetic scratch profile) launched by global-setup.ts.
//
// Deliberately a separate config file rather than a project in
// playwright.config.ts: real mode is serial (one shared backend, one
// model), slow (real inference), and owns app lifecycle via
// global-setup — none of which should leak into the synthetic suite.
//
// Run: npm run test:e2e:real
// Guard: refuses to start while a daemon owns :9741 (attach-mode
// hermeticity leak) unless SOVEREIGN_REAL_ALLOW_ATTACH=1.
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e/real",
  globalSetup: "./tests/e2e/real/global-setup.ts",
  globalTeardown: "./tests/e2e/real/global-teardown.ts",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? "github" : "list",
  // Real inference: a turn on the fast profile is seconds, but cold
  // slot loads + retrieval can stack. Specs assert terminal events.
  timeout: 180_000,
  expect: { timeout: 30_000 },

  use: {
    baseURL: "http://localhost:5173",
    trace: "retain-on-failure",
    video: "retain-on-failure",
    screenshot: "only-on-failure",
  },

  projects: [
    {
      name: "real-chromium",
      use: { ...devices["Desktop Chrome"] },
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
