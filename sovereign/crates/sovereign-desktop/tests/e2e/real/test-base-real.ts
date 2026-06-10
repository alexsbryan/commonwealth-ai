// SPDX-License-Identifier: AGPL-3.0-or-later
// Real-mode test base. Mirrors fixtures/test-base.ts (pageerror gate,
// fatal Svelte console patterns) but injects tauri-shim-real.js so
// every invoke reaches the REAL sovereign-desktop process launched by
// global-setup.ts, and real backend events flow into the page.
import { test as base, expect, type Page } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REAL_SHIM_PATH = path.resolve(__dirname, "../fixtures/tauri-shim-real.js");
export const BRIDGE_URL = "http://127.0.0.1:9745";

/** Read-only assertion surface exposed by tauri-shim-real.js. */
export interface SovereignRealAPI {
  captured: Array<{ seq: number; event: string; payload: unknown }>;
  chunksFor(messageId: string): string[];
  completeFor(messageId: string): {
    message_id: string;
    full_text: string;
    metadata: unknown;
  } | null;
  lagged(): boolean;
}

declare global {
  interface Window {
    __sovereign_real__: SovereignRealAPI;
  }
}

/** Header-safe (ASCII) spec attribution for the coverage ledger. */
function asciiSpecName(titlePath: string[]): string {
  // eslint-disable-next-line no-control-regex
  return titlePath.join(" > ").replace(/[^\x20-\x7e]/g, "?");
}

// Keep in lockstep with fixtures/test-base.ts — same Svelte runtime
// invariants, now standing against real streams.
const SVELTE_CONSOLE_FAIL_PATTERNS: RegExp[] = [
  /each_key_duplicate/,
  /non_reactive_update/,
  /state_referenced_locally/,
  /hydration_mismatch/,
  /store_invalid_shape/,
  /effect_in_teardown/,
  /effect_in_unowned_derived/,
  /derived_references_self/,
  /Svelte error:/i,
];

interface RealHarness {
  /** Invoke a command directly on the bridge (bypassing the page) —
   *  for setup/assertion plumbing, not for driving user flows. */
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  /** Run a closure against window.__sovereign_real__. */
  real<T>(fn: (api: SovereignRealAPI) => T): Promise<T>;
}

export const test = base.extend<{
  sovereignPage: Page;
  bridge: RealHarness;
}>({
  sovereignPage: async ({ page }, use, testInfo) => {
    const pageErrors: Error[] = [];
    const fatalConsoleErrors: string[] = [];
    page.on("pageerror", (err) => pageErrors.push(err));
    page.on("console", (msg) => {
      if (msg.type() !== "error" && msg.type() !== "warning") return;
      const text = msg.text();
      if (SVELTE_CONSOLE_FAIL_PATTERNS.some((p) => p.test(text))) {
        fatalConsoleErrors.push(text);
      }
    });
    // Order matters: globals first, then the shim that reads them.
    // ASCII-only: the spec name travels as an HTTP header value
    // (X-Sovereign-Spec) and fetch() rejects non ISO-8859-1 strings.
    const spec = asciiSpecName(testInfo.titlePath);
    await page.addInitScript(
      ([bridgeUrl, specName]) => {
        (window as unknown as Record<string, unknown>).__SOVEREIGN_BRIDGE_URL__ =
          bridgeUrl;
        (window as unknown as Record<string, unknown>).__SOVEREIGN_SPEC_NAME__ =
          specName;
      },
      [BRIDGE_URL, spec] as const,
    );
    await page.addInitScript({ path: REAL_SHIM_PATH });
    await use(page);
    const allowed = testInfo.annotations.some((a) => a.type === "allow-page-errors");
    if (pageErrors.length > 0 && !allowed) {
      throw new Error(
        `Uncaught page errors during real-mode test (${pageErrors.length}):\n` +
          pageErrors.map((e, i) => `  [${i}] ${e.stack ?? String(e)}`).join("\n"),
      );
    }
    if (fatalConsoleErrors.length > 0 && !allowed) {
      throw new Error(
        `Fatal Svelte runtime diagnostics during real-mode test ` +
          `(${fatalConsoleErrors.length}):\n` +
          fatalConsoleErrors.map((t, i) => `  [${i}] ${t}`).join("\n"),
      );
    }
  },

  bridge: async ({ sovereignPage }, use, testInfo) => {
    const spec = asciiSpecName(testInfo.titlePath);
    const harness: RealHarness = {
      invoke: async (cmd, args) => {
        const res = await fetch(`${BRIDGE_URL}/invoke`, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "x-sovereign-spec": spec,
          },
          body: JSON.stringify({ cmd, args: args ?? {} }),
        });
        const body = (await res.json()) as { ok: boolean; result?: unknown; error?: unknown };
        if (!body.ok) {
          throw new Error(`bridge invoke ${cmd} failed: ${JSON.stringify(body.error)}`);
        }
        return body.result as never;
      },
      // The closure is serialized into the page and applied to the
      // shim's global — evaluate(fn, arg) would pass `undefined` as
      // `api` instead.
      real: async (fn) =>
        (await sovereignPage.evaluate(
          (fnSrc) =>
            // eslint-disable-next-line no-eval
            (0, eval)(`(${fnSrc})`)(window.__sovereign_real__),
          fn.toString(),
        )) as never,
    };
    await use(harness);
  },
});

export { expect };

/** Boot to the chat surface against the real backend. No poll-emit:
 *  the sticky backend-ready replay from the bridge delivers the
 *  handshake as soon as App.svelte registers its listener. */
export async function realBootToChat(page: Page): Promise<void> {
  await page.goto("/");
  await page.locator(".chat-view").waitFor({ state: "visible", timeout: 30_000 });
}
