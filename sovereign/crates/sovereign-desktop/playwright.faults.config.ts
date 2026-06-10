// SPDX-License-Identifier: AGPL-3.0-or-later
// Fault-injection suite: a SUPERVISED desktop (daemon as child
// process) plus per-spec instances for boot-time faults. Serial by
// nature — specs kill processes and own ports.
//
// Run: npm run test:e2e:faults
// Requires :9741 free (stop the dev daemon) — the supervised child
// owns it.
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e/real/faults",
  globalSetup: "./tests/e2e/real/faults/global-setup.ts",
  globalTeardown: "./tests/e2e/real/faults/global-teardown.ts",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? "github" : "list",
  // Kill → detect → restart → recover cycles stack heartbeat windows.
  timeout: 300_000,
  expect: { timeout: 30_000 },

  use: {
    baseURL: "http://localhost:5173",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },

  projects: [{ name: "faults-chromium", use: { ...devices["Desktop Chrome"] } }],

  webServer: {
    command: "npm run dev",
    url: "http://localhost:5173",
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
