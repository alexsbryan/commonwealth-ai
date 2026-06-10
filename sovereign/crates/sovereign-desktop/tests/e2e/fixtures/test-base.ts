// SPDX-License-Identifier: AGPL-3.0-or-later
import { test as base, expect, type Page } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { TtfiReport } from "./scenario-player";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SHIM_PATH = path.resolve(__dirname, "./tauri-shim.js");
const TTFI_PROBE_PATH = path.resolve(__dirname, "./ttfi-probe.js");

// Coverage ledger: one JSONL row per command invoke the shim observed,
// attributed to the spec that triggered it. Appends are line-atomic
// (O_APPEND, rows ≪ PIPE_BUF) so parallel workers can share the file.
// Truncated per-run by global-setup-ledger.mjs; joined against the
// generate_handler! manifest by scripts/coverage-report.mjs.
export const LEDGER_PATH = path.resolve(
  __dirname,
  "../../../test-artifacts/ledger-synthetic.jsonl",
);
let ledgerDirReady = false;
function appendLedger(row: Record<string, unknown>): void {
  if (!ledgerDirReady) {
    fs.mkdirSync(path.dirname(LEDGER_PATH), { recursive: true });
    ledgerDirReady = true;
  }
  fs.appendFileSync(LEDGER_PATH, `${JSON.stringify(row)}\n`);
}

/** Surface exposed by tauri-shim.js on `window.__sovereign_test__`. */
export interface SovereignTestAPI {
  setHandler(cmd: string, fn: ((args: unknown) => unknown) | null): void;
  emit(eventName: string, payload: unknown): number;
  signalBackendReady(): number;
  streamTokens(
    messageId: string,
    tokens: string[],
    gapMs?: number,
  ): Promise<void>;
  completeMessage(
    messageId: string,
    fullText: string,
    metadata?: unknown,
  ): number;
  errorMessage(message: string): number;
  lastStreamStart(): { conversationId: string; messageId: string } | null;
  lastCancel(): { conversationId: string } | null;
  lastConsent(): { shareGpu: boolean } | null;
  reset(): void;
}

declare global {
  interface Window {
    __sovereign_test__: SovereignTestAPI;
  }
}

interface ChatHarness {
  /** Drives the Tauri shim from inside the page. */
  api: {
    /** Run an arbitrary closure against window.__sovereign_test__. */
    drive<T>(
      fn: (api: SovereignTestAPI) => T,
    ): Promise<T extends Promise<infer U> ? U : T>;
    /** Convenience: signal backend-ready (gates the chat view). */
    signalBackendReady(): Promise<void>;
    /** Convenience: stream a list of tokens at a given cadence. */
    streamTokens(
      messageId: string,
      tokens: string[],
      gapMs?: number,
    ): Promise<void>;
    /** Convenience: emit message-complete. */
    completeMessage(
      messageId: string,
      fullText?: string,
      metadata?: unknown,
    ): Promise<void>;
    /** Convenience: emit message-error. */
    errorMessage(error: string): Promise<void>;
    /** Convenience: peek the last send_message_stream invocation's id. */
    lastStreamStart(): Promise<{
      conversationId: string;
      messageId: string;
    } | null>;
    /** Convenience: peek the last cancel_stream invocation. */
    lastCancel(): Promise<{ conversationId: string } | null>;
    /** Time-to-First-Intelligence probe. The probe is installed
     *  via addInitScript on every page; tests anchor t0 immediately
     *  before the Send click and read the report after the scenario's
     *  terminal state. See `fixtures/ttfi-probe.js`. */
    ttfi: {
      /** Anchor t0 = now. Resets all markers. Call IMMEDIATELY before
       *  the Send-button click. */
      markStart(): Promise<void>;
      /** Read the latest report. Any tier still null means the marker
       *  hasn't appeared yet. */
      getReport(): Promise<TtfiReport>;
    };
  };
}

// Substrings that, when they appear in a console.error/warning,
// indicate a Svelte runtime invariant has been violated and the
// app is now in a degraded reactive state. Each one corresponds
// to a documented Svelte error code at https://svelte.dev/e/<code>
// and would silently freeze a subtree of the UI without this gate.
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

export const test = base.extend<{
  chat: ChatHarness;
  sovereignPage: Page;
}>({
  // Inject the Tauri shim BEFORE any app script runs. addInitScript with
  // path: feeds a classic script into every page Playwright opens.
  //
  // Every uncaught page error is collected and fails the test at
  // teardown. This is the universal chaos detector: any test that
  // triggers a JS exception in the WebView fails, even if no explicit
  // assertion noticed. Tests that EXPECT errors (rare) can opt out via
  // testInfo annotations.
  //
  // Console.error is also collected — Svelte 5's runtime
  // diagnostics (`each_key_duplicate`, hydration mismatches, store
  // contract violations) ride console.error rather than throwing,
  // so without this gate a freezing reactivity bug like duplicate
  // each-keys would slip through every existing test. See the
  // SVELTE_CONSOLE_FAIL_PATTERNS list below for what we treat as
  // hard failures.
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
    // Coverage-ledger sink. The shim calls this binding (when present)
    // once per invoke; absence is fine (manual dev, non-fixture pages).
    await page.exposeBinding(
      "__sovereign_ledger_record__",
      (_source, cmd: string, ok: boolean) => {
        appendLedger({ cmd, ok, spec: testInfo.titlePath.join(" › ") });
      },
    );
    await page.addInitScript({ path: SHIM_PATH });
    // TTFI probe installs window.__ttfi__ — independent of the shim,
    // but must load before app code so MutationObserver can be primed
    // from page-load. Tests that don't measure TTFI simply never call
    // markStart() and the probe stays inert.
    await page.addInitScript({ path: TTFI_PROBE_PATH });
    await use(page);
    const allowed = testInfo.annotations.some(
      (a) => a.type === "allow-page-errors",
    );
    if (pageErrors.length > 0 && !allowed) {
      throw new Error(
        `Uncaught page errors during test (${pageErrors.length}):\n` +
          pageErrors
            .map((e, i) => `  [${i}] ${e.stack ?? String(e)}`)
            .join("\n"),
      );
    }
    if (fatalConsoleErrors.length > 0 && !allowed) {
      throw new Error(
        `Fatal Svelte runtime diagnostics during test ` +
          `(${fatalConsoleErrors.length}):\n` +
          fatalConsoleErrors.map((t, i) => `  [${i}] ${t}`).join("\n"),
      );
    }
  },

  chat: async ({ sovereignPage }, use) => {
    const harness: ChatHarness = {
      api: {
        drive: async (fn) =>
          (await sovereignPage.evaluate(fn, undefined as never)) as never,
        signalBackendReady: async () => {
          await sovereignPage.evaluate(() => {
            window.__sovereign_test__.signalBackendReady();
          });
        },
        streamTokens: async (messageId, tokens, gapMs = 0) => {
          await sovereignPage.evaluate(
            async ({ messageId, tokens, gapMs }) => {
              await window.__sovereign_test__.streamTokens(
                messageId,
                tokens,
                gapMs,
              );
            },
            { messageId, tokens, gapMs },
          );
        },
        completeMessage: async (messageId, fullText = "", metadata) => {
          await sovereignPage.evaluate(
            ({ messageId, fullText, metadata }) => {
              window.__sovereign_test__.completeMessage(
                messageId,
                fullText,
                metadata,
              );
            },
            { messageId, fullText, metadata },
          );
        },
        errorMessage: async (error) => {
          await sovereignPage.evaluate((error) => {
            window.__sovereign_test__.errorMessage(error);
          }, error);
        },
        lastStreamStart: async () =>
          sovereignPage.evaluate(() =>
            window.__sovereign_test__.lastStreamStart(),
          ),
        lastCancel: async () =>
          sovereignPage.evaluate(() => window.__sovereign_test__.lastCancel()),
        ttfi: {
          markStart: async () => {
            await sovereignPage.evaluate(() => window.__ttfi__.markStart());
          },
          getReport: async () =>
            sovereignPage.evaluate(() => window.__ttfi__.getReport()),
        },
      },
    };
    await use(harness);
  },
});

export { expect };

/** Boot the chat surface: navigate, wait for app mount, dispatch the
 *  backend-ready handshake, and assert we landed on the chat view.
 *
 *  Robustness note: App.svelte's onMount awaits initEventListeners
 *  before the backend-ready listener is wired. We poll-emit
 *  backend-ready until the chat view appears so we don't race the
 *  registration. Cheap (in-page event) and idempotent. */
export async function bootToChat(page: Page, chat: ChatHarness): Promise<void> {
  await page.goto("/");
  await page
    .locator(".loading-screen, .chat-view, .app-layout")
    .first()
    .waitFor();
  const chatView = page.locator(".chat-view");
  await expect
    .poll(
      async () => {
        await chat.api.signalBackendReady();
        return chatView.count();
      },
      { timeout: 10_000, intervals: [50, 100, 200, 500] },
    )
    .toBeGreaterThan(0);
  await chatView.waitFor({ state: "visible" });
}
