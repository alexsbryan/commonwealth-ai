// SPDX-License-Identifier: AGPL-3.0-or-later
// Per-run global setup for the mocked (synthetic) e2e suite:
//
//  1. Truncates the coverage ledger so coverage-report.mjs always
//     reflects exactly one suite run (parallel workers append to it
//     during the run; see fixtures/test-base.ts).
//  2. Warms the Vite dev server by loading the app once, before any
//     test starts.
//
// Why the warm-up: Playwright's `webServer` waits only for the port to
// answer, not for Vite to have transformed the module graph. The first
// `page.goto("/")` of a run therefore pays the whole cold transform —
// measured at ~30s on this app (162 Svelte components + the chat-ui
// source alias), against a 30s per-test timeout. Whichever tests happen
// to start first would eat that cost and time out, then pass on retry:
// exactly the "one hard failure plus a handful of flakes, all at
// precisely 30s" signature seen before this landed. Paying it here puts
// it outside every test's clock, where it belongs.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const LEDGER_PATH = path.resolve(
  __dirname,
  "../../test-artifacts/ledger-synthetic.jsonl",
);
const SHIM_PATH = path.resolve(__dirname, "./fixtures/tauri-shim.js");

// Generous: this is a cold compile on a possibly-loaded dev box, and it
// is not competing with anything. Well above the ~30s observed.
const WARMUP_TIMEOUT_MS = 180_000;

export default async function globalSetup(config) {
  fs.rmSync(LEDGER_PATH, { force: true });

  const baseURL =
    config?.projects?.[0]?.use?.baseURL ?? "http://localhost:5173";
  const started = Date.now();
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    // Same shim the fixture injects, so the warm-up exercises the same
    // module graph the tests will (the app aborts its boot without the
    // Tauri bridge, leaving lazily-imported views untransformed).
    await page.addInitScript({ path: SHIM_PATH });
    await page.goto(baseURL, { timeout: WARMUP_TIMEOUT_MS });
    await page
      .locator(".loading-screen, .chat-view, .app-layout")
      .first()
      .waitFor({ timeout: WARMUP_TIMEOUT_MS });
    const secs = ((Date.now() - started) / 1000).toFixed(1);
    console.log(`[e2e warm-up] vite dev graph transformed in ${secs}s`);
  } catch (err) {
    // Never block the run on the warm-up: a failure here just means the
    // first test pays the cost as it did before. Say so loudly.
    console.warn(
      `[e2e warm-up] failed after ${((Date.now() - started) / 1000).toFixed(1)}s ` +
        `— tests will pay the cold-start cost themselves: ${err}`,
    );
  } finally {
    await browser.close();
  }
}
