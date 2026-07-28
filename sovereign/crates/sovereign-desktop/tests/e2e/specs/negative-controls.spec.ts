// SPDX-License-Identifier: AGPL-3.0-or-later
// NEGATIVE CONTROLS FOR THE REAL-MODE INVARIANT PACK
//
// # The question this answers
//
// Every other test in this repo asks "does the product behave?" This one asks
// "would we notice if it stopped?" — the only question whose answer a passing
// suite cannot supply about itself. A green suite is consistent with two very
// different worlds: the product works, or the assertions are vacuous. Coverage
// numbers do not separate them (an unreachable assertion still counts as
// reached), and neither does adding more specs.
//
// The separator is a negative control: feed the instrument a sample that is
// KNOWN BAD and require it to say so. The repo already runs this discipline on
// its judges — `sovereign-eval`'s mechanism-fidelity harness holds a blindfolded
// agent that must score at chance, and CHAOS_QA_METHODOLOGY's judge-calibration
// gate holds sensitivity/specificity floors against a labelled bank. This file
// extends the same standard to the assertion pack that guards real-mode turns:
// an instrument nobody has tried to fool is not calibrated, it is merely quiet.
//
// # Why here, and not in the real-mode suite
//
// `invariants.ts` runs only under `playwright.real.config.ts`, which needs
// multi-GB GGUFs and a live desktop and therefore runs NOWHERE in CI (ci.yml
// says so out loud). Its potency would be unguarded on exactly the merges that
// could weaken it. These controls need no app — they stage captured rows
// directly — so they live in the synthetic suite, which CI does run. CI thereby
// protects assertions it never executes.
//
// # Fidelity: the real shim, not a double
//
// The staged turn is pushed through `fixtures/tauri-shim-real.js` itself, with
// only `EventSource` stubbed. `captured`, `chunksFor`, `lagged` and the
// `__lagged__` synthesis are the production implementations. A hand-rolled
// double would let the shim's capture contract drift out from under the
// invariant pack while every control still passed — which is the precise
// failure mode this file exists to make impossible.
//
// # What a control must prove
//
// Three things, or it proves nothing:
//   1. The BASELINE passes. A negative control on an already-red baseline is
//      indistinguishable from a broken harness (`positive control` below).
//   2. The mutated turn FAILS — exactly one field perturbed, so the blame is
//      unambiguous.
//   3. It fails for the DECLARED REASON. Matching any error at all would let a
//      typo in the staging code masquerade as a working control; every case
//      pins the message the invariant is supposed to emit.
//
// Cases marked `tolerates` are the mirror image — deliberately odd turns the
// pack must NOT reject (web citations, attach-mode corpora, `length` finishes).
// They fence in over-strictness, which is how an instrument goes deaf next:
// not by asserting too little, but by asserting so much that the suite gets
// loosened wholesale to shut it up.
import { test, expect, type Page } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  assertTurnInvariants,
  type TurnInvariantOptions,
} from "../real/invariants";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REAL_SHIM = path.resolve(__dirname, "../fixtures/tauri-shim-real.js");
const INVARIANTS_SRC = path.resolve(__dirname, "../real/invariants.ts");

const MID = "msg-negative-control";
const FIXTURE_CORPUS = "e2e-fixture-corpus";

/** One captured SSE row, in the shape the bridge streams. */
interface Row {
  seq: number;
  event: string;
  payload: Record<string, unknown>;
}

/** A well-formed two-chunk grounded turn: the sample every control mutates.
 *  Deep-cloned per case so mutations cannot leak between tests. */
function baselineRows(): Row[] {
  return [
    { seq: 1, event: "message-chunk", payload: { message_id: MID, chunk: "Ada Lovelace " } },
    { seq: 2, event: "message-chunk", payload: { message_id: MID, chunk: "wrote the first algorithm." } },
    {
      seq: 3,
      event: "message-complete",
      payload: {
        message_id: MID,
        full_text: "Ada Lovelace wrote the first algorithm.",
        metadata: {
          provenance: {
            intent: "referential",
            total_latency_ms: 1240,
            finish_reason: "stop",
            self_assessment: "all figures traceable to retrieved chunks",
            sources: [{ origin: "knowledge", count: 1 }],
          },
          retrieved_chunks: [
            { title: "Notes on the Analytical Engine", corpus_id: FIXTURE_CORPUS, chunk_id: 7 },
          ],
        },
      },
    },
  ];
}

/** The `metadata` object of the staged complete row, for terse mutations. */
function meta(rows: Row[]): Record<string, unknown> {
  return rows[2].payload.metadata as Record<string, unknown>;
}

/** The `provenance` object of the staged complete row. */
function prov(rows: Row[]): Record<string, unknown> {
  return meta(rows).provenance as Record<string, unknown>;
}

/** The staged citation list. */
function citations(rows: Row[]): Array<Record<string, unknown>> {
  return meta(rows).retrieved_chunks as Array<Record<string, unknown>>;
}

interface BridgeStub {
  /** Corpus ids this app instance can resolve. Default: the fixture corpus. */
  localIds?: string[];
  /** What `read_get_chunk` returns. Default: a non-empty chunk. */
  chunk?: { content: string } | null;
}

/** A `BridgeLike` standing in for the command bridge. Only the three commands
 *  the invariant pack issues are answered; anything else throws, so a pack that
 *  grows a new dependency fails loudly here instead of silently no-op'ing. */
function stubBridge(opts: BridgeStub = {}) {
  const localIds = opts.localIds ?? [FIXTURE_CORPUS];
  const chunk = opts.chunk === undefined ? { content: "…the Analytical Engine…" } : opts.chunk;
  return {
    invoke: async <T = unknown>(cmd: string): Promise<T> => {
      if (cmd === "lc_list") return localIds.map((corpus_id) => ({ corpus_id })) as T;
      if (cmd === "list_corpora") return [] as T;
      if (cmd === "read_get_chunk") return chunk as T;
      throw new Error(
        `negative-control bridge stub: unexpected command ${JSON.stringify(cmd)}. ` +
          `The invariant pack issues a command this stub does not model — teach the ` +
          `stub, and add a control for whatever the new command is guarding.`,
      );
    },
  };
}

/** Load the PRODUCTION shim into a blank page with `EventSource` stubbed, then
 *  push the staged rows through its real `onmessage` handler. */
async function stageTurn(page: Page, rows: Row[]): Promise<void> {
  await page.addInitScript(() => {
    // The shim opens exactly one EventSource at load and holds it in a
    // closure. Capturing the instance is the only way to reach its
    // `onmessage` — which is the point: rows enter through the same door
    // the bridge's stream uses.
    class StubEventSource {
      onmessage: ((e: { data: string }) => void) | null = null;
      onerror: (() => void) | null = null;
      constructor(public url: string) {
        (window as unknown as Record<string, unknown>).__negctl_es__ = this;
      }
      close(): void {}
    }
    (window as unknown as Record<string, unknown>).EventSource = StubEventSource;
    (window as unknown as Record<string, unknown>).__negctl_push__ = (row: unknown) => {
      const es = (window as unknown as { __negctl_es__: StubEventSource }).__negctl_es__;
      if (!es?.onmessage) throw new Error("shim never opened its EventSource");
      es.onmessage({ data: JSON.stringify(row) });
    };
  });
  await page.addInitScript({ path: REAL_SHIM });
  // A real document on a real origin — `addInitScript` needs a navigation to
  // apply, and the shim must be the one that populates `__sovereign_real__`.
  await page.goto("about:blank");
  await page.evaluate((staged) => {
    const push = (window as unknown as { __negctl_push__: (r: unknown) => void })
      .__negctl_push__;
    for (const row of staged as unknown[]) push(row);
  }, rows as unknown);
}

/** Run the pack against a staged turn, returning the thrown error (or null). */
async function runPack(
  page: Page,
  rows: Row[],
  opts: TurnInvariantOptions = {},
  bridge = stubBridge(),
): Promise<Error | null> {
  await stageTurn(page, rows);
  try {
    await assertTurnInvariants(page, bridge, MID, opts);
    return null;
  } catch (e) {
    return e as Error;
  }
}

// ─────────────────────────────────────────────────────────────────────
// The control bank
// ─────────────────────────────────────────────────────────────────────

interface Control {
  /** Stable id, quoted in the failure and in NEGATIVE_CONTROLS.md. */
  id: string;
  /** The invariant this proves is live, in the pack's own numbering. */
  invariant: string;
  /** Exactly one perturbation of the baseline. */
  mutate: (rows: Row[]) => void;
  /** Options the pack must be called with for this invariant to apply. */
  opts?: TurnInvariantOptions;
  /** Bridge behaviour, when the mutation lives on the resolution side. */
  bridge?: BridgeStub;
  /** The pack must fail with a message matching this. Broad-matching would
   *  let a staging typo pass as a working control. */
  expect: RegExp;
}

const CONTROLS: Control[] = [
  {
    id: "sse-lag-ignored",
    invariant: "1. stream integrity — a lagged consumer invalidates the turn",
    mutate: (rows) => {
      rows.splice(1, 0, { seq: -1, event: "__lagged__", payload: { lagged: 4 } });
    },
    expect: /SSE consumer lagged/,
  },
  {
    id: "duplicate-complete",
    invariant: "1. stream integrity — exactly one message-complete per id",
    mutate: (rows) => {
      rows.push({ ...rows[2], seq: 4 });
    },
    expect: /expected exactly one message-complete/,
  },
  {
    id: "chunks-do-not-reconstruct",
    invariant: "1. stream integrity — concat(chunks) === full_text",
    mutate: (rows) => {
      rows[2].payload.full_text = "Ada Lovelace wrote the second algorithm.";
    },
    expect: /byte-for-byte/,
  },
  {
    id: "dropped-chunk",
    invariant: "1. stream integrity — a silently dropped chunk breaks the concat",
    mutate: (rows) => {
      rows.splice(1, 1);
    },
    expect: /byte-for-byte/,
  },
  {
    id: "metadata-absent",
    invariant: "2. glassbox — the turn carries metadata at all",
    mutate: (rows) => {
      rows[2].payload.metadata = null;
    },
    expect: /metadata must be present/,
  },
  {
    id: "intent-absent",
    invariant: "2. glassbox — provenance.intent OR metadata.intent, non-empty",
    mutate: (rows) => {
      delete prov(rows).intent;
      delete meta(rows).intent;
    },
    expect: /carries no intent/,
  },
  {
    id: "intent-empty-string",
    invariant: "2. glassbox — an empty intent is not an intent",
    mutate: (rows) => {
      prov(rows).intent = "";
    },
    expect: /carries no intent/,
  },
  {
    id: "finish-reason-mismatch",
    invariant: "2. glassbox — an exact finish_reason demand is enforced",
    mutate: (rows) => {
      prov(rows).finish_reason = "length";
    },
    opts: { expectFinish: "stop" },
    expect: /finish_reason should be/,
  },
  {
    id: "finish-reason-nonsense",
    invariant: "2. glassbox — an unrecognised finish_reason is rejected",
    mutate: (rows) => {
      prov(rows).finish_reason = "exploded";
    },
    expect: /unexpected finish_reason/,
  },
  {
    id: "grounded-turn-without-citations",
    invariant: "3. citations — a knowledge turn must cite",
    mutate: (rows) => {
      meta(rows).retrieved_chunks = [];
    },
    opts: { requireCitations: true },
    expect: /must carry retrieved_chunks/,
  },
  {
    id: "citation-without-corpus",
    invariant: "3. citations — every citation carries a corpus_id",
    mutate: (rows) => {
      delete citations(rows)[0].corpus_id;
    },
    expect: /citation missing corpus_id/,
  },
  {
    id: "citation-without-chunk-id",
    invariant: "3. citations — every citation carries a numeric chunk_id",
    mutate: (rows) => {
      citations(rows)[0].chunk_id = null;
    },
    expect: /citation missing chunk_id/,
  },
  {
    id: "dangling-citation",
    invariant: "3. citations — a local citation dereferences (the 79fdd04c bug)",
    mutate: () => {
      /* the turn is well-formed; the RESOLUTION is what fails */
    },
    bridge: { chunk: null },
    expect: /dangling citation/,
  },
  {
    id: "citation-resolves-to-nothing",
    invariant: "3. citations — a resolved chunk has content",
    mutate: () => {
      /* resolution succeeds but yields an empty body */
    },
    bridge: { chunk: { content: "" } },
    expect: /resolved to an empty chunk/,
  },
  {
    id: "untraceable-figures",
    invariant: "4. numeric honesty — the runtime's own audit is believed",
    mutate: (rows) => {
      prov(rows).self_assessment = "the 1876 figure is not traceable to any chunk";
    },
    expect: /numeric audit failed/,
  },
];

/** Turns the pack must NOT reject. Over-strictness is the other way an
 *  instrument dies: the suite gets loosened wholesale to silence it. */
interface Tolerance {
  id: string;
  why: string;
  mutate: (rows: Row[]) => void;
  opts?: TurnInvariantOptions;
  bridge?: BridgeStub;
}

const TOLERANCES: Tolerance[] = [
  {
    id: "web-citation-has-no-chunk-handle",
    why: "web results cite by URL; demanding a chunk handle would fail every web turn",
    mutate: (rows) => {
      citations(rows)[0] = {
        title: "Ada Lovelace",
        corpus_id: "",
        chunk_id: null,
        url: "https://example.org/ada",
        provenance_tier: "web",
      };
    },
    bridge: { chunk: null },
  },
  {
    id: "attach-mode-external-corpus",
    why: "in Attach mode the daemon's corpora are not readable through this instance",
    mutate: (rows) => {
      citations(rows)[0].corpus_id = "a-corpus-owned-by-the-external-daemon";
    },
    bridge: { localIds: [], chunk: null },
  },
  {
    id: "length-finish-under-a-token-budget",
    why: "truncation is a legitimate outcome the desktop renders as a chip, not a defect",
    mutate: (rows) => {
      prov(rows).finish_reason = "length";
    },
  },
  {
    id: "finish-check-waived",
    why: "expectFinish:null must genuinely skip the check, as cancel specs rely on",
    mutate: (rows) => {
      prov(rows).finish_reason = "cancelled-midstream";
    },
    opts: { expectFinish: null },
  },
  {
    id: "speech-act-turn-without-provenance",
    why: "conation/expressive handlers attach a top-level intent and no provenance block",
    mutate: (rows) => {
      delete meta(rows).provenance;
      meta(rows).intent = "conation";
    },
  },
];

// ─────────────────────────────────────────────────────────────────────
// The suite
// ─────────────────────────────────────────────────────────────────────

test.describe("negative controls — the real-mode invariant pack can fail", () => {
  // Every case navigates one blank page and does no network I/O.
  test.describe.configure({ mode: "parallel" });

  test("positive control: the unmutated baseline passes", async ({ page }) => {
    const err = await runPack(page, baselineRows(), { requireCitations: true });
    expect(
      err,
      "the baseline turn must satisfy the pack — every negative control below " +
        "is meaningless if the sample they mutate is already red, because they " +
        "would all 'catch' a failure that has nothing to do with their mutation",
    ).toBeNull();
  });

  for (const c of CONTROLS) {
    test(`negative control: ${c.id} — ${c.invariant}`, async ({ page }) => {
      const rows = baselineRows();
      c.mutate(rows);
      const err = await runPack(page, rows, c.opts ?? {}, stubBridge(c.bridge ?? {}));

      expect(
        err,
        `SURVIVED: the invariant pack accepted a turn that violates "${c.invariant}". ` +
          `The assertion guarding it is vacuous — real-mode specs would stay green ` +
          `through this regression in production. Restore the assertion in ` +
          `tests/e2e/real/invariants.ts, do not weaken this control.`,
      ).not.toBeNull();

      expect(
        err?.message ?? "",
        `control "${c.id}" failed for the WRONG reason. It must fail on ` +
          `${c.expect} — matching any error at all would let a mistake in the ` +
          `staging code pass as a working control.`,
      ).toMatch(c.expect);
    });
  }

  for (const t of TOLERANCES) {
    test(`tolerance: ${t.id}`, async ({ page }) => {
      const rows = baselineRows();
      t.mutate(rows);
      const err = await runPack(page, rows, t.opts ?? {}, stubBridge(t.bridge ?? {}));
      expect(
        err,
        `OVER-STRICT: the pack rejected a turn it must accept — ${t.why}. ` +
          `Left standing, this fails legitimate production turns, and the usual ` +
          `repair is to loosen the pack wholesale, which costs every invariant above.`,
      ).toBeNull();
    });
  }

  // ── The tripwire ──
  //
  // Controls guard the assertions that exist TODAY. Nothing so far notices an
  // assertion added tomorrow with no control behind it — the bank would still
  // be green while a fresh invariant sat unproven. Counting is crude and that
  // is the point: it cannot be satisfied by accident, and the failure names the
  // exact obligation.
  const CONTROLLED_ASSERTIONS = 13;

  test("tripwire: every assertion in the pack is accounted for", async () => {
    const src = fs.readFileSync(INVARIANTS_SRC, "utf8");
    const found = (src.match(/\bexpect\(/g) ?? []).length;
    expect(
      found,
      `tests/e2e/real/invariants.ts holds ${found} assertions; this bank was ` +
        `written against ${CONTROLLED_ASSERTIONS}.\n` +
        `  MORE  → an invariant was added. Add a control proving it can fail, ` +
        `then raise CONTROLLED_ASSERTIONS.\n` +
        `  FEWER → an invariant was deleted. Confirm that was intended (a ` +
        `deleted assertion is a silently un-tested production behaviour), ` +
        `remove its control, then lower CONTROLLED_ASSERTIONS.\n` +
        `Adjusting this number without doing one of those is how the bank goes ` +
        `stale while staying green.`,
    ).toBe(CONTROLLED_ASSERTIONS);
  });
});
