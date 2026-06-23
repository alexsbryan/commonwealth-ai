// SPDX-License-Identifier: AGPL-3.0-or-later
// The journey wrapper: a thin shim over the real-mode test fixture that
// (a) tags each acceptance test with its user-impact tier, (b) routes
// every chat turn through the existing invariant pack so stream
// integrity / provenance / citation resolution are enforced for free,
// and (c) appends a glassbox result record the journey report reads.
//
// It is deliberately NOT a test framework (ARCH_PRINCIPLES §10.3 —
// helper over framework). It composes sendAndAwaitTurn +
// assertTurnInvariants; it does not reimplement them.
//
// J5 (first-launch setup) owns its own app lifecycle on a separate
// bridge port and so cannot use the :9745-wired `test` fixture — it
// drives a raw page and records its result via the exported
// recordJourneyResult() with the same record shape.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Page } from "@playwright/test";
import {
  assertTurnInvariants,
  sendAndAwaitTurn,
  type TurnFacts,
  type TurnInvariantOptions,
} from "../invariants";
import { test } from "../test-base-real";
import type { Journey } from "./manifest";

export { expect, realBootToChat } from "../test-base-real";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../../..");
const ARTIFACTS = path.join(CRATE_ROOT, "test-artifacts");
export const JOURNEY_RESULTS = path.join(ARTIFACTS, "journey-results.jsonl");

/** One run id per `playwright test` invocation. workers:1 (the real
 *  config) means this module loads once, so every result this run is
 *  tagged with the same id and the report can isolate the latest run —
 *  mirrors soak-report's soak_start/seed filter so results never
 *  Frankenstein across runs. */
export const RUN_ID = Date.now();

/** Append one JSONL record. mkdir is idempotent and cheap; it guards
 *  the case where a record is written before global-setup created the
 *  artifacts dir. */
export function recordJourneyResult(rec: Record<string, unknown>): void {
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  fs.appendFileSync(JOURNEY_RESULTS, JSON.stringify(rec) + "\n");
}

// Stamp the start of this run (read by journey-report.mjs).
recordJourneyResult({ kind: "run_start", runId: RUN_ID, ts: RUN_ID });

/** Minimal bridge surface the invariant pack needs. Structurally
 *  satisfied by the real-mode `bridge` fixture and by J5's own
 *  bridgeInvoke adapter. */
export interface JourneyBridge {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

/** Per-journey bookkeeping: counts the turns a journey took, the
 *  citations resolved across them, and any glassbox notes (e.g. a
 *  best-effort step that was skipped). Emitted as the result record. */
export class JourneyRun {
  turns = 0;
  citationsResolved = 0;
  readonly notes: string[] = [];
  private readonly startedAt = Date.now();

  constructor(
    private readonly meta: Journey,
    private readonly page: Page,
    private readonly bridge: JourneyBridge,
  ) {}

  /** Send one turn through the real UI and assert the invariant pack on
   *  it. Every turn a journey makes goes through here, so the glassbox
   *  floor (stream integrity, provenance, citation resolution, numeric
   *  honesty) holds regardless of what else the journey checks. */
  async turn(
    text: string,
    opts: TurnInvariantOptions & { timeoutMs?: number } = {},
  ): Promise<TurnFacts> {
    const { timeoutMs, ...invariantOpts } = opts;
    const mid = await sendAndAwaitTurn(this.page, text, { timeoutMs });
    const facts = await assertTurnInvariants(this.page, this.bridge, mid, invariantOpts);
    this.turns += 1;
    this.citationsResolved += facts.citations.length;
    return facts;
  }

  /** Record a note surfaced in the journey report — use it to make a
   *  skipped best-effort step visible rather than silent. */
  note(message: string): void {
    this.notes.push(message);
  }

  /** Write the result record. `failed` is threaded from the
   *  try/catch/finally in journeyTest so it reflects a thrown
   *  assertion, not testInfo.status (which isn't final inside the body). */
  finish(failed: boolean): void {
    recordJourneyResult({
      kind: "journey",
      runId: RUN_ID,
      id: this.meta.id,
      tier: this.meta.tier,
      title: this.meta.title,
      surfaces: this.meta.surfaces,
      status: failed ? "failed" : "passed",
      turns: this.turns,
      citationsResolved: this.citationsResolved,
      durationMs: Date.now() - this.startedAt,
      notes: this.notes,
      ts: Date.now(),
    });
  }
}

/** Register a journey as a real-mode acceptance test. The title is
 *  tier-prefixed (`[T1] chat-citation — …`) so a glance at the runner
 *  output reads priority-first. */
export function journeyTest(
  meta: Journey,
  body: (ctx: { page: Page; bridge: JourneyBridge; run: JourneyRun }) => Promise<void>,
): void {
  test(`[T${meta.tier}] ${meta.id} — ${meta.title}`, async ({ sovereignPage, bridge }) => {
    const run = new JourneyRun(meta, sovereignPage, bridge);
    let failed = false;
    try {
      await body({ page: sovereignPage, bridge, run });
    } catch (e) {
      failed = true;
      throw e;
    } finally {
      run.finish(failed);
    }
  });
}
