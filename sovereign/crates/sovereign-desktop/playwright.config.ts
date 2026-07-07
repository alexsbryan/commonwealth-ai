// SPDX-License-Identifier: AGPL-3.0-or-later
import { defineConfig, devices } from "@playwright/test";

// Runs against `npm run dev` (vite) with the Tauri bridge mocked via
// addInitScript — see tests/e2e/fixtures/tauri-shim.js. We don't drive
// the real Tauri runtime because the chat surface is 100% TypeScript +
// Svelte; the Rust side only emits events, which the shim simulates
// with deterministic cadence (the whole point of these tests).
export default defineConfig({
  testDir: "./tests/e2e/specs",
  // Truncates test-results/ledger-synthetic.jsonl (the command-coverage
  // ledger) so each run's coverage report is self-contained.
  globalSetup: "./tests/e2e/global-setup-ledger.mjs",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  // One retry everywhere. This mocked suite runs `fullyParallel` against a
  // single Vite dev server, so at high local worker counts a few
  // animation/timeout-sensitive specs (inner-work witness fade-in, the
  // knowledge-layer chip dispatch) occasionally time out under transient
  // contention — they pass solo. A retry self-heals those without masking a
  // real regression (a consistent failure still fails every attempt), matching
  // the reliability posture CI already runs with.
  retries: 1,
  // Cap local parallelism below the 50%-of-cores default: that default is what
  // saturates the shared dev server on high-core dev boxes and drives the
  // flakes above. CI stays at 2.
  workers: process.env.CI ? 2 : 4,
  reporter: process.env.CI ? "github" : "list",
  timeout: 30_000,
  expect: { timeout: 5_000 },

  use: {
    baseURL: "http://localhost:5173",
    trace: "retain-on-failure",
    video: "retain-on-failure",
    screenshot: "only-on-failure",
  },

  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],

  webServer: {
    command: "npm run dev",
    url: "http://localhost:5173",
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
    stdout: "pipe",
    stderr: "pipe",
  },
});
