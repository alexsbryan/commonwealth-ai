// SPDX-License-Identifier: AGPL-3.0-or-later
// J5 (Tier 1, CRITICAL) — first launch through setup and consent into a
// working chat. A user who can't get through setup never sees anything
// else, so this is the highest-stakes journey; it's also the only one
// that can't ride the shared real-mode harness, which bakes
// `setup_complete = true` and boots straight past onboarding.
//
// This spec owns its own app instance on a separate bridge port
// (reusing the fault-suite's spawnDesktop), booted with
// `setup_complete = false` so the boot guard routes to the wizard
// (main.rs §setup-required). The baked model paths point at the present
// 2B + embed GGUFs, so complete_setup_auto's pick_path reuses them and
// skips every download (setup_flow.rs §4) — setup completes in seconds,
// not a multi-GB pull, which is what makes this runnable locally.
//
// Hard gates (prove first-launch works end-to-end): setup-required →
// setup flow mounts → backend-ready → chat → a turn completes with
// invariants. The setup-progress phase telemetry is asserted as glassbox
// when captured, but a capture gap never fails the journey — the
// user-visible outcome does.
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";
import {
  ARTIFACTS,
  awaitSticky,
  bridgeInvoke,
  type DesktopInstance,
  eventsRecent,
  portInUse,
  spawnDesktop,
} from "../faults/spawn";
import { assertTurnInvariants, sendAndAwaitTurn } from "../invariants";
import { RUN_ID, recordJourneyResult } from "./journey";
import { J_FIRST_LAUNCH_SETUP as J } from "./manifest";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SHIM = path.resolve(__dirname, "../../fixtures/tauri-shim-real.js");
const BRIDGE_PORT = 9746;
const BRIDGE = `http://127.0.0.1:${BRIDGE_PORT}`;
const PROFILE_DIR = path.join(ARTIFACTS, "journey-setup-profile");

/** Normalize a serialized SetupPhase to a lowercase kind, robust to
 *  serde's internal ({kind}), external ({Variant:…}), or bare-string
 *  tagging — so a serialization choice can't silently void the check. */
function phaseKind(phase: unknown): string | undefined {
  if (typeof phase === "string") return phase.toLowerCase();
  if (phase && typeof phase === "object") {
    const obj = phase as Record<string, unknown>;
    if (typeof obj.kind === "string") return obj.kind.toLowerCase();
    const keys = Object.keys(obj);
    if (keys.length) return keys[0].toLowerCase();
  }
  return undefined;
}

test.describe.serial("first-launch setup", () => {
  let app: DesktopInstance | undefined;

  test.beforeAll(async () => {
    if (await portInUse(BRIDGE_PORT)) {
      throw new Error(
        `:${BRIDGE_PORT} already in use — a stale setup-journey app? ` +
          `Kill it before re-running.`,
      );
    }
    app = await spawnDesktop({
      profileDir: PROFILE_DIR,
      bridgePort: BRIDGE_PORT,
      logName: "journey-setup-app.log",
      // Clean first-launch: desktop.toml-only (matches global-setup's
      // proven profile shape), routed to the wizard, no daemon config.
      profile: { setupComplete: false, cliSetupConfig: false },
    });
    // The boot guard must route to the wizard (fires before any model
    // load, so it's fast).
    await awaitSticky(BRIDGE, "setup-required", 120_000);
    // Register setup-progress so emissions during the wizard land in the
    // bridge replay ring (lazy listen_any — same contract global-setup
    // relies on for per-job channels).
    await fetch(`${BRIDGE}/listen`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ event: "setup-progress" }),
    });
  });

  test.afterAll(async () => {
    await app?.stop();
  });

  test(`[T${J.tier}] ${J.id} — ${J.title}`, async ({ page }) => {
    test.setTimeout(240_000);
    const started = Date.now();
    const notes: string[] = [];
    const pageErrors: Error[] = [];
    page.on("pageerror", (e) => pageErrors.push(e));
    let failed = false;
    try {
      // Point the shim at OUR bridge — the shared fixture is wired to
      // :9745 (test-base-real.ts). Order matters: globals before the
      // shim that reads them.
      await page.addInitScript(
        ([url]) => {
          (window as unknown as Record<string, unknown>).__SOVEREIGN_BRIDGE_URL__ = url;
          (window as unknown as Record<string, unknown>).__SOVEREIGN_SPEC_NAME__ =
            "first-launch-setup.journey";
        },
        [BRIDGE] as const,
      );
      await page.addInitScript({ path: SHIM });

      await page.goto("/");

      // Welcome → Setup Plan → Setup Flow (the consent-first onboarding
      // chain; "Set up Sovereign" is the consent to mutate).
      await expect(page.locator(".threshold"), "welcome screen must render").toBeVisible({
        timeout: 30_000,
      });
      await page.locator(".begin-btn").click();
      await page.locator(".btn-go").click();
      await expect(page.locator(".setup-flow"), "setup flow must mount").toBeVisible({
        timeout: 30_000,
      });

      // complete_setup_auto runs on mount; with models present it skips
      // downloads, bootstraps in-process, and emits backend-ready.
      await awaitSticky(BRIDGE, "backend-ready", 180_000);

      // Glassbox phase telemetry (soft — see file header).
      const progressRows = (await eventsRecent(BRIDGE)).filter(
        (r) => r.event === "setup-progress",
      );
      const kinds = progressRows
        .map((r) => phaseKind((r.payload as { phase?: unknown })?.phase))
        .filter((k): k is string => Boolean(k));
      if (kinds.length > 0) {
        expect(kinds, "setup must reach the Ready phase").toContain("ready");
        const downloaded = kinds.some((k) => k.startsWith("downloading"));
        const distinct = [...new Set(kinds)].join(" → ");
        notes.push(
          downloaded
            ? `WARNING: a download phase ran despite present models (${distinct})`
            : `phases: ${distinct} (downloads skipped — models present)`,
        );
      } else {
        notes.push(
          "no setup-progress captured on the bridge ring (telemetry gap; outcome still asserted)",
        );
      }

      // Consent gate, then chat. The gate may be skipped in some routes,
      // so accept either and click through the gate when present.
      const gate = page.locator(".gate");
      const chat = page.locator(".chat-view");
      await expect(gate.or(chat), "setup must land on consent or chat").toBeVisible({
        timeout: 30_000,
      });
      if (await gate.isVisible()) {
        await gate.locator(".choice-secondary").click(); // keep compute local
        notes.push("consent gate: kept all compute local");
      }
      await expect(chat, "must land in chat after setup/consent").toBeVisible({
        timeout: 30_000,
      });

      // The backend setup produced actually answers a turn (invariants
      // via an adapter pointed at our bridge).
      const bridge = {
        invoke: <T = unknown>(cmd: string, args?: Record<string, unknown>) =>
          bridgeInvoke<T>(BRIDGE, cmd, args),
      };
      const mid = await sendAndAwaitTurn(page, "In one sentence, what are you?");
      await assertTurnInvariants(page, bridge, mid);
      notes.push("setup produced a working chat that answered a turn");

      expect(
        pageErrors,
        `uncaught page errors during setup: ${pageErrors.map((e) => e.message).join("; ")}`,
      ).toHaveLength(0);
    } catch (e) {
      failed = true;
      throw e;
    } finally {
      recordJourneyResult({
        kind: "journey",
        runId: RUN_ID,
        id: J.id,
        tier: J.tier,
        title: J.title,
        surfaces: J.surfaces,
        status: failed ? "failed" : "passed",
        turns: 1,
        citationsResolved: 0,
        durationMs: Date.now() - started,
        notes,
        ts: Date.now(),
      });
    }
  });
});
