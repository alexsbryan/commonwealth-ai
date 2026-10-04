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
import { CAPTION, CAPTION_EL_ID, captionChipCss } from "./reel-style.mjs";

export { expect } from "@playwright/test";
export { demoClick, demoType, glideTo, glideToLocator, parkCursor } from "./cursor";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
export const DEMO_DIR = path.join(CRATE_ROOT, "test-artifacts/demo");
export const DEMO_LEDGER = path.join(DEMO_DIR, "ledger.jsonl");
/** Where hand-recorded takes and their caption sheets live. */
export const RAW_DIR = path.join(DEMO_DIR, "raw");

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

/** One scripted lower-third for a hand-recorded take. The beat owns the
 *  WORDS (they are part of the claim); the operator owns the TIMES,
 *  which can only be known against the actual recording — so they are
 *  filled in afterwards in the caption sheet, not here. */
export interface CaptionLine {
  text: string;
  holdMs?: number;
}

/**
 * A beat whose footage cannot be produced by Playwright.
 *
 * Two exist, for two different reasons, and neither is a shortcut:
 *
 *   B3  the mesh-app bundle only gets real data inside a window labelled
 *       `meshapp-<id>` (meshapp.rs `authorize` is fail-closed on that
 *       label, and the test command bridge always invokes as `main`).
 *       That window is a native Tauri webview; Playwright's screencast
 *       is per-page and cannot see it.
 *   B7  the agent is coding on a Raspberry Pi across the room.
 *
 * The danger of "just drop a .mov in" is that the clip ships with no
 * proof its claim was ever true — which breaks the one contract that
 * makes this a test suite and not a screen recorder. So a raw beat is
 * still a TEST: it runs a gate against the live daemon and the live app,
 * and `demo-export.mjs` refuses to encode the take unless that gate
 * passed in the same run. The human attests the pixels; the machine
 * attests the claim behind them.
 */
export interface RawDemoBeat extends DemoBeat {
  capture: "raw";
  /** What the operator must do to produce the take. Printed on a pass
   *  and written into the manifest — a beat that gates something nobody
   *  knows how to film is a beat that never ships. */
  recordingGuide: string[];
  /** The lower-thirds this clip carries, in order. Burned in by the
   *  exporter using the same chip the live beats draw. */
  script?: CaptionLine[];
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
   *  capture so the reel needs no separate subtitle pass.
   *
   *  The chip's styling comes from `reel-style.mjs`, which the exporter
   *  also rasterizes when it burns captions into a hand-recorded take.
   *  One definition, so a screencast beat and a raw beat cannot end up
   *  wearing different type. */
  async caption(text: string, holdMs = CAPTION.holdMs): Promise<void> {
    await this.page.evaluate(
      ([msg, hold, elId, css, cssVisible, fadeOutMs]) => {
        const prev = document.getElementById(elId as string);
        prev?.remove();
        const el = document.createElement("div");
        el.id = elId as string;
        el.textContent = msg as string;
        el.style.cssText = css as string;
        document.documentElement.appendChild(el);
        // Two frames, not one: the element must have been laid out with
        // the hidden style before the visible one can transition from it.
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            el.style.cssText = cssVisible as string;
          }),
        );
        setTimeout(() => {
          el.style.opacity = "0";
          el.style.transform = "translateX(-50%) translateY(8px)";
          setTimeout(() => el.remove(), fadeOutMs as number);
        }, hold as number);
      },
      [
        text,
        holdMs,
        CAPTION_EL_ID,
        captionChipCss(),
        captionChipCss({ visible: true }),
        CAPTION.fadeOutMs,
      ] as const,
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
      capture: "screencast",
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

/** Where the operator's take and its caption sheet are expected. */
export const rawTakePrefix = (id: string): string => path.join(RAW_DIR, id);

/** Seed `raw/<id>.captions.json` with the beat's scripted lines and no
 *  times. NEVER overwrites: once the operator has placed the cues
 *  against their recording, a later gate run must not wipe them. */
function seedCaptionSheet(meta: RawDemoBeat): { path: string; created: boolean } {
  const file = `${rawTakePrefix(meta.id)}.captions.json`;
  if (fs.existsSync(file)) return { path: file, created: false };
  fs.mkdirSync(RAW_DIR, { recursive: true });
  fs.writeFileSync(
    file,
    JSON.stringify(
      {
        beat: meta.id,
        _how: [
          "Times are in SECONDS from the start of the raw file, and only you can",
          "know them — set `at` on each line once the take exists. A line with",
          "`at: null` is skipped and reported in MANIFEST.md, never guessed.",
          "`trimInSec`/`trimOutSec` cut the handles off the take; null keeps all of it.",
        ],
        trimInSec: null,
        trimOutSec: null,
        captions: (meta.script ?? []).map((c) => ({
          at: null,
          text: c.text,
          holdMs: c.holdMs ?? CAPTION.holdMs,
        })),
      },
      null,
      2,
    ) + "\n",
  );
  return { path: file, created: true };
}

/**
 * Register a HAND-RECORDED beat: a gate that runs like any other test,
 * and footage the operator supplies.
 *
 * The body asserts everything the clip claims, against the live daemon
 * and the live app. Passing it does two things: it authorizes the
 * exporter to encode `raw/<id>.<ext>` (and nothing else authorizes it),
 * and it seeds the caption sheet so the take is scripted before it is
 * shot rather than subtitled after.
 */
export function rawBeatTest(
  meta: RawDemoBeat,
  gate: (ctx: {
    page: Page;
    bridge: { invoke<T = unknown>(c: string, a?: Record<string, unknown>): Promise<T> };
    run: BeatRun;
  }) => Promise<void>,
): void {
  test(`${meta.id} — ${meta.title} [raw gate]`, async ({ demoPage, bridge }) => {
    const run = new BeatRun(meta, demoPage, bridge);
    let status: "passed" | "failed" | "skipped" = "passed";
    let skipReason: string | undefined;
    let error: string | undefined;
    try {
      await gate({ page: demoPage, bridge, run });
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
      let sheet: { path: string; created: boolean } | null = null;
      if (status === "passed") {
        sheet = seedCaptionSheet(meta);
        // eslint-disable-next-line no-console
        console.log(
          `[demo:${meta.id}] GATE PASSED — the take is authorized. To film it:\n` +
            meta.recordingGuide.map((s) => `    · ${s}`).join("\n") +
            `\n    · save it as ${path.relative(CRATE_ROOT, rawTakePrefix(meta.id))}.mov` +
            `\n    · cue sheet ${sheet.created ? "seeded at" : "already at"} ` +
            `${path.relative(CRATE_ROOT, sheet.path)}`,
        );
      }
      run.finish(status, {
        // A raw beat's screencast is of the app sitting still while the
        // gate ran. Recording null keeps the exporter from ever mistaking
        // it for the beat's footage.
        capture: "raw",
        video: null,
        outputDir: null,
        recordingGuide: meta.recordingGuide,
        script: meta.script ?? [],
        captionSheet: sheet?.path ?? null,
        skipReason,
        error,
      });
      if (status === "skipped") {
        // eslint-disable-next-line no-console
        console.log(`[demo:${meta.id}] SKIPPED — ${skipReason}`);
        test.skip(true, skipReason);
      }
    }
  });
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
