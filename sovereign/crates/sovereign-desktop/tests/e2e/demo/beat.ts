// SPDX-License-Identifier: AGPL-3.0-or-later
// The beat wrapper — the demo analogue of `journeys/journey.ts`.
//
// Same posture as the journey shim, and deliberately the same shape: it
// is NOT a framework (ARCH_PRINCIPLES §10.3 — helper over framework). It
// composes the real-mode fixture, the invariant pack, and the cursor
// primitives, then appends one glassbox record per beat that
// `demo-export.mjs` reads to cut clips.
//
// The contract that makes this a test suite and not a screen recorder:
//   a beat that fails its assertions produces NO exportable clip.
// The ledger record carries `status`, and the exporter refuses anything
// that isn't `passed`. There is no flag to override that — a demo we
// can't verify is a demo we don't ship.
//
// Skips are equally load-bearing: an unmet precondition (peer offline,
// corpus missing) records `status: "skipped"` WITH a reason and prints
// it. It is never silently downgraded to a mocked stand-in, and the
// exporter never resurrects a clip from a previous run to fill the hole.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect as pwExpect, type Page } from "@playwright/test";
import {
  assertTurnInvariants,
  type TurnFacts,
  type TurnInvariantOptions,
} from "../real/invariants";
import { test } from "./demo-base";
import { demoClick, demoType, parkCursor } from "./cursor";

export { expect } from "@playwright/test";
export { demoClick, demoType, glideTo, glideToLocator, parkCursor } from "./cursor";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
export const DEMO_DIR = path.join(CRATE_ROOT, "test-artifacts/demo");
export const DEMO_LEDGER = path.join(DEMO_DIR, "ledger.jsonl");

/** One id per `playwright test` invocation, so the exporter can isolate
 *  the latest run instead of Frankensteining clips across takes.
 *
 *  Stamped once in global-setup and read from the environment here. It is
 *  NOT `Date.now()` at module load, despite workers:1: Playwright starts a
 *  FRESH WORKER after a test failure, which reloads this module and would
 *  mint a second id mid-run. The exporter takes only `max(runId)`, so a
 *  single failing beat silently split one take in two and dropped every
 *  beat before the failure from the export — observed 2026-07-24, where a
 *  9-beat run exported b9 alone and quietly discarded b1/b2/b5.
 *  Worker processes inherit the env global-setup mutated, restarts
 *  included, so this survives exactly the case that broke it. */
export const RUN_ID = Number(process.env.SOVEREIGN_DEMO_RUN_ID) || Date.now();

function appendLedger(rec: Record<string, unknown>): void {
  fs.mkdirSync(DEMO_DIR, { recursive: true });
  fs.appendFileSync(DEMO_LEDGER, JSON.stringify(rec) + "\n");
}

/** Beat metadata — the spec entry from DEMO_BEATS.md, in code. */
export interface DemoBeat {
  /** Stable slug; becomes the exported filename stem (`b1-determinism`). */
  id: string;
  /** Human title for the run summary / manifest. */
  title: string;
  /** The one sentence a viewer should walk away believing. */
  claim: string;
  /** Seconds of runway the exporter leaves before the first mark and
   *  after the last when cutting the short-form GIF. */
  gifPadSec?: number;
  /** Which mark the short-form GIF is cut on. Lives here rather than in
   *  the exporter so the beat and its highlight can't drift apart — the
   *  spec that emits the mark is the spec that names it. Falls back to
   *  the last recorded mark when absent or unmatched. */
  gifMark?: string;
}

/** Thrown by requireOrSkip so beatTest can record the skip before
 *  Playwright's own skip machinery unwinds the test. */
class BeatSkip extends Error {
  constructor(readonly reason: string) {
    super(reason);
  }
}

export class BeatRun {
  readonly marks: { name: string; atMs: number }[] = [];
  readonly notes: string[] = [];
  turns = 0;
  citationsResolved = 0;
  readonly startedAt = Date.now();

  constructor(
    readonly meta: DemoBeat,
    private readonly page: Page,
    private readonly bridge: { invoke<T = unknown>(c: string, a?: Record<string, unknown>): Promise<T> },
  ) {}

  /** Timestamp a moment inside the beat, as an offset from beat start.
   *  The exporter cuts short-form GIFs on these — `mark("witness-appears")`
   *  is what turns a 90s beat into an 8s loop without a human scrubbing
   *  a timeline. */
  mark(name: string): void {
    this.marks.push({ name, atMs: Date.now() - this.startedAt });
  }

  /** Surface something in the manifest — use it to make a best-effort
   *  step that didn't happen VISIBLE rather than silent. */
  note(message: string): void {
    this.notes.push(message);
    // eslint-disable-next-line no-console
    console.log(`[demo:${this.meta.id}] ${message}`);
  }

  /** Hard precondition. Records the skip (with the reason, in the
   *  ledger and on stdout) and aborts the beat. */
  requireOrSkip(ok: boolean, reason: string): void {
    if (!ok) throw new BeatSkip(reason);
  }

  /** Hold on the current frame. Named for what it is: a demo needs
   *  stillness where a test wants speed. */
  async dwell(ms: number): Promise<void> {
    await this.page.waitForTimeout(ms);
  }

  /** Send one turn through the real UI at human cadence and assert the
   *  invariant pack on it — the same pack the journeys run, so stream
   *  integrity, glassbox provenance and citation resolution hold for
   *  every turn that appears on camera.
   *
   *  Diverges from journeys' sendAndAwaitTurn only in HOW the text gets
   *  into the box (typed, not filled) — the terminal-event wait and the
   *  assertions are identical. */
  async turn(
    text: string,
    opts: TurnInvariantOptions & { timeoutMs?: number; charDelayMs?: number } = {},
  ): Promise<TurnFacts> {
    const { timeoutMs, charDelayMs, ...invariantOpts } = opts;
    const page = this.page;

    // demoType presses Enter at paragraph breaks, and Enter in the chat
    // composer SENDS. A multi-paragraph prompt would fire half a turn.
    if (/\n/.test(text)) {
      throw new Error(
        `BeatRun.turn() prompt must be a single paragraph (Enter sends in the composer). ` +
          `Got:\n${text}`,
      );
    }

    const before = await page.evaluate(
      () =>
        window.__sovereign_real__.captured.filter((r) => r.event === "message-complete")
          .length,
    );

    await demoType(page, page.locator(".input-area textarea"), text, {
      charDelayMs: charDelayMs ?? 30,
    });
    await this.dwell(420); // beat of stillness before send — reads as intent
    await demoClick(page, page.locator(".send-btn"));
    this.mark("turn-sent");

    await pwExpect
      .poll(
        () =>
          page.evaluate(
            () =>
              window.__sovereign_real__.captured.filter(
                (r) => r.event === "message-complete",
              ).length,
          ),
        { timeout: timeoutMs ?? 240_000, intervals: [500, 1000, 2000] },
      )
      .toBeGreaterThan(before);

    const mid = await page.evaluate(() => {
      const completes = window.__sovereign_real__.captured.filter(
        (r) => r.event === "message-complete",
      );
      return (completes[completes.length - 1].payload as { message_id: string })
        .message_id;
    });
    this.mark("turn-complete");

    const facts = await assertTurnInvariants(page, this.bridge, mid, invariantOpts);
    this.turns += 1;
    this.citationsResolved += facts.citations.length;
    return facts;
  }

  /** Render a lower-third caption over the app. Presentation only — it
   *  is `pointer-events: none` and lives above everything, so it can
   *  never intercept a click or change what's asserted. Burned into the
   *  capture so the reel needs no separate subtitle pass. */
  async caption(text: string, holdMs = 2600): Promise<void> {
    await this.page.evaluate(
      ([msg, hold]) => {
        const prev = document.getElementById("__sovereign_demo_caption__");
        prev?.remove();
        const el = document.createElement("div");
        el.id = "__sovereign_demo_caption__";
        el.textContent = msg as string;
        el.style.cssText = [
          "position:fixed",
          "left:50%",
          "bottom:44px",
          "transform:translateX(-50%) translateY(8px)",
          "max-width:76%",
          "padding:12px 22px",
          "border-radius:12px",
          "background:rgba(14,14,18,.82)",
          "backdrop-filter:blur(14px)",
          "color:#f4f4f6",
          "font:500 20px/1.4 'IBM Plex Sans', system-ui, sans-serif",
          "letter-spacing:.005em",
          "text-align:center",
          "pointer-events:none",
          "z-index:2147483645",
          "opacity:0",
          "transition:opacity 320ms ease, transform 320ms ease",
          "box-shadow:0 8px 30px rgba(0,0,0,.35)",
        ].join(";");
        document.documentElement.appendChild(el);
        requestAnimationFrame(() => {
          el.style.opacity = "1";
          el.style.transform = "translateX(-50%) translateY(0)";
        });
        setTimeout(() => {
          el.style.opacity = "0";
          el.style.transform = "translateX(-50%) translateY(8px)";
          setTimeout(() => el.remove(), 400);
        }, hold as number);
      },
      [text, holdMs] as const,
    );
  }

  /** Park the pointer out of the way for a long read beat. */
  async park(): Promise<void> {
    await parkCursor(this.page);
  }

  finish(status: "passed" | "failed" | "skipped", extra: Record<string, unknown> = {}): void {
    appendLedger({
      kind: "beat",
      runId: RUN_ID,
      id: this.meta.id,
      title: this.meta.title,
      claim: this.meta.claim,
      status,
      gifPadSec: this.meta.gifPadSec ?? 1.2,
      gifMark: this.meta.gifMark ?? null,
      marks: this.marks,
      turns: this.turns,
      citationsResolved: this.citationsResolved,
      durationMs: Date.now() - this.startedAt,
      notes: this.notes,
      ts: Date.now(),
      ...extra,
    });
  }
}

/** Register a beat as a real-mode capture test. */
export function beatTest(
  meta: DemoBeat,
  body: (ctx: {
    page: Page;
    bridge: { invoke<T = unknown>(c: string, a?: Record<string, unknown>): Promise<T> };
    run: BeatRun;
  }) => Promise<void>,
): void {
  test(`${meta.id} — ${meta.title}`, async ({ demoPage, bridge }, testInfo) => {
    const run = new BeatRun(meta, demoPage, bridge);
    let status: "passed" | "failed" | "skipped" = "passed";
    let skipReason: string | undefined;
    let error: string | undefined;
    try {
      await body({ page: demoPage, bridge, run });
      // A short tail so the last frame isn't the instant the assertion
      // returned — the exporter's lead-out has something to cut into.
      await run.dwell(900);
    } catch (e) {
      if (e instanceof BeatSkip) {
        status = "skipped";
        skipReason = e.reason;
      } else {
        status = "failed";
        error = e instanceof Error ? (e.stack ?? e.message) : String(e);
        throw e;
      }
    } finally {
      // page.video() is null when video capture is off (e.g. a --grep
      // debug run with video disabled) — record null rather than crash.
      let video: string | null = null;
      try {
        video = (await demoPage.video()?.path()) ?? null;
      } catch {
        video = null;
      }
      run.finish(status, { video, skipReason, error, outputDir: testInfo.outputDir });
      if (status === "skipped") {
        // eslint-disable-next-line no-console
        console.log(`[demo:${meta.id}] SKIPPED — ${skipReason}`);
        test.skip(true, skipReason);
      }
    }
  });
}
