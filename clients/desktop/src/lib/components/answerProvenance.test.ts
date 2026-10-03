// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, it, expect } from "vitest";
import {
  readAnswerProvenance,
  readStageAttribution,
  readTypedAbstention,
  type AnswerSegmentWire,
} from "./answerProvenance";

const seg = (
  start: number,
  end: number,
  kind: AnswerSegmentWire["kind"],
): AnswerSegmentWire => ({ text_range: { start, end }, kind });

describe("readAnswerProvenance", () => {
  it("returns null when the turn never segmented (flag off)", () => {
    // The whole flag-off contract on the desktop side: no key, no strip.
    expect(readAnswerProvenance({}, "some answer")).toBeNull();
    expect(readAnswerProvenance(undefined, "some answer")).toBeNull();
    expect(
      readAnswerProvenance({ answer_segments: null }, "some answer"),
    ).toBeNull();
  });

  it("distinguishes 'segmented and found nothing' from 'never segmented'", () => {
    const p = readAnswerProvenance({ answer_segments: [] }, "some answer");
    expect(p).not.toBeNull();
    expect(p?.total).toBe(0);
  });

  it("tiles the answer text by byte range and labels each stretch", () => {
    const answer = "The mill stands on Harbour Row. I think it burned down.";
    const cut = answer.indexOf(" I think");
    const p = readAnswerProvenance(
      {
        answer_segments: [
          seg(0, cut, {
            kind: "grounded",
            chunk_id: "2",
            span: { start: 10, end: 41 },
            address: { corpus_id: "saltgrass", chunk_id: 4102 },
          }),
          seg(cut, answer.length, { kind: "parametric" }),
        ],
      },
      answer,
    );
    expect(p?.rows.map((r) => r.text)).toEqual([
      "The mill stands on Harbour Row.",
      " I think it burned down.",
    ]);
    expect(p?.rows[0].chunkId).toBe("2");
    expect(p?.rows[0].address).toEqual({ corpusId: "saltgrass", chunkId: 4102 });
    expect(p?.rows[1].chunkId).toBeNull();
    expect(p?.grounded).toBe(1);
    expect(p?.groundedAddressed).toBe(1);
    // Labels talk about WHERE the text is, never about whether it is
    // right — the resolver certifies at 0.7429 precision, not 1.0.
    expect(p?.rows[0].label).toBe("found in your sources");
    expect(p?.rows[1].label).toBe("the model's own words");
  });

  it("slices multibyte answers on scalar boundaries, not code units", () => {
    // "Åsa" is 4 UTF-8 bytes / 3 UTF-16 units. A naive slice would cut
    // the answer one character short and show the user a truncated
    // sentence next to a provenance badge — the exact mis-highlight the
    // shared byte converter exists to prevent.
    const answer = "Åsa signed the ledger. Probably.";
    const cut = new TextEncoder().encode("Åsa signed the ledger.").length;
    const p = readAnswerProvenance(
      {
        answer_segments: [
          seg(0, cut, {
            kind: "grounded",
            chunk_id: "0",
            span: { start: 0, end: 4 },
            address: { corpus_id: "saltgrass", chunk_id: 1 },
          }),
        ],
      },
      answer,
    );
    expect(p?.rows[0].text).toBe("Åsa signed the ledger.");
  });

  it("reports an unknown segment kind instead of coercing it", () => {
    const p = readAnswerProvenance(
      {
        answer_segments: [
          { text_range: { start: 0, end: 2 }, kind: { kind: "quantum" } },
        ],
      },
      "hi",
    );
    expect(p?.rows[0].kind).toBe("unknown");
    expect(p?.rows[0].label).toContain("quantum");
    expect(p?.grounded).toBe(0);
  });

  it("counts a grounded badge with no address as NOT resolving", () => {
    // The citability bar is `groundedAddressed === grounded`. A pool
    // slot the runtime could not map to a corpus handle must show up as
    // a miss here, not be quietly counted as a resolved badge.
    const p = readAnswerProvenance(
      {
        answer_segments: [
          seg(0, 2, { kind: "grounded", chunk_id: "3", span: { start: 0, end: 2 } }),
        ],
      },
      "hi",
    );
    expect(p?.grounded).toBe(1);
    expect(p?.groundedAddressed).toBe(0);
    expect(p?.rows[0].address).toBeNull();
  });

  it("counts unverified segments — the demoted badge, never shown as grounded", () => {
    const p = readAnswerProvenance(
      {
        answer_segments: [
          seg(0, 2, { kind: "unverified" }),
          seg(2, 4, { kind: "inference" }),
        ],
      },
      "abcd",
    );
    expect(p?.unverified).toBe(1);
    expect(p?.grounded).toBe(0);
  });
});

describe("readTypedAbstention", () => {
  it("is null on released turns and on turns with no gate", () => {
    expect(readTypedAbstention({})).toBeNull();
    expect(
      readTypedAbstention({ grounding_gate: { action: "released" } }),
    ).toBeNull();
    expect(readTypedAbstention({ grounding_gate: null })).toBeNull();
  });

  it("reads every abstained_* variant from the field, not the prose", () => {
    for (const action of [
      "abstained",
      "abstained_decline",
      "abstained_unverified",
    ]) {
      expect(readTypedAbstention({ grounding_gate: { action } })?.action).toBe(
        action,
      );
    }
  });

  it("carries H1's answerability as telemetry, and null when it did not run", () => {
    expect(
      readTypedAbstention({
        grounding_gate: { action: "abstained_decline", native_answerability: 0.31 },
      })?.nativeAnswerability,
    ).toBe(0.31);
    expect(
      readTypedAbstention({ grounding_gate: { action: "abstained_decline" } })
        ?.nativeAnswerability,
    ).toBeNull();
  });
});

// ─── G4: the stack-attribution strip ─────────────────────────────────

/** The operator's 2026-08-12 turn, as it would reach the wire. Numbers
 *  are the measured ones from NATIVE_GROUNDING_ECONOMY.md §7.2. */
const LADDER_TURN = {
  stage_attribution: {
    total_ms: 150_200,
    served_by: "both_stacks",
    incumbent_ms: 119_670,
    rows: [
      { stage: "retrieval", owner: "shared", ms: 32_600, cause: "every_turn" },
      { stage: "admission", owner: "native", ms: 40, cause: "every_turn" },
      { stage: "draft", owner: "shared", ms: 25_400, cause: "every_turn", calls: 1 },
      {
        stage: "audit",
        owner: "incumbent",
        ms: 25_550,
        mechanism: "per_claim_judge",
        cause: "every_turn",
        calls: 7,
      },
      {
        stage: "rewrite",
        owner: "incumbent",
        ms: 43_220,
        mechanism: "full_resynthesis",
        cause: "audit_found_failures",
      },
      {
        stage: "re_audit",
        owner: "incumbent",
        ms: 50_850,
        mechanism: "per_claim_judge",
        cause: "rewrite_produced_new_prose",
        calls: 12,
      },
      { stage: "segments", owner: "native", ms: 60, mechanism: "deterministic" },
      { stage: "gate_unattributed", owner: "incumbent", ms: 50 },
      { stage: "turn_unattributed", owner: "shared", ms: 2_430 },
    ],
  },
};

describe("readStageAttribution", () => {
  it("is null when the turn opened no ledger — never an empty strip", () => {
    expect(readStageAttribution({})).toBeNull();
    expect(readStageAttribution(undefined)).toBeNull();
    expect(readStageAttribution({ stage_attribution: null })).toBeNull();
  });

  /** The acceptance test, in a unit test: three questions answered off
   *  the strip alone. */
  it("answers which stack served, what each stage cost, and which old-stack mechanism ran", () => {
    const a = readStageAttribution(LADDER_TURN)!;
    // (1) which system served the turn
    expect(a.servedBy).toBe("BOTH stacks");
    expect(a.oldStackRan).toBe(true);
    expect(a.oldStackSeconds).toBeCloseTo(119.67, 2);
    expect(a.totalSeconds).toBeCloseTo(150.2, 2);
    // (2) what each stage cost
    const rewrite = a.rows.find((r) => r.stage === "rewrite")!;
    expect(rewrite.seconds).toBeCloseTo(43.22, 2);
    expect(rewrite.owner).toBe("OLD STACK");
    // (3) WHICH mechanism, and why the stage existed at all
    expect(rewrite.detail).toContain("full re-synthesis (surgical fell back)");
    expect(a.rows.find((r) => r.stage === "re_audit")!.detail).toContain(
      "exists only because the rewrite ran",
    );
  });

  /** The negative case the order requires watched: surgery ENGAGING must
   *  read differently from surgery falling back, on the same stage. */
  it("names the surgical mechanism when surgery engaged", () => {
    const a = readStageAttribution({
      stage_attribution: {
        total_ms: 90_000,
        served_by: "both_stacks",
        incumbent_ms: 5_360,
        rows: [
          {
            stage: "rewrite",
            owner: "incumbent",
            ms: 5_360,
            mechanism: "surgical_rewrite",
            cause: "audit_found_failures",
          },
        ],
      },
    })!;
    expect(a.rows[0].detail).toContain("surgical span edits");
    expect(a.rows[0].detail).not.toContain("fell back");
  });

  it("reads oldStackRan from the runtime's verdict, not by counting rows", () => {
    // A turn whose ONLY incumbent-owned row is the arithmetic residual.
    // The runtime sealed it `chain_floor_only`; the UI must not overrule
    // that by spotting an `incumbent` owner in the rows (ARCH §10.6).
    const a = readStageAttribution({
      stage_attribution: {
        total_ms: 40_000,
        served_by: "chain_floor_only",
        incumbent_ms: 0,
        rows: [
          { stage: "retrieval", owner: "shared", ms: 32_000 },
          { stage: "gate_unattributed", owner: "incumbent", ms: 0 },
          { stage: "turn_unattributed", owner: "shared", ms: 8_000 },
        ],
      },
    })!;
    expect(a.oldStackRan).toBe(false);
    expect(a.servedBy).toBe("no grounding stack ran");
  });

  it("marks residual rows as residuals and says what they are", () => {
    const a = readStageAttribution(LADDER_TURN)!;
    const resid = a.rows.filter((r) => r.isResidual).map((r) => r.stage);
    expect(resid).toEqual(["gate_unattributed", "turn_unattributed"]);
    expect(a.rows.find((r) => r.stage === "turn_unattributed")!.detail).toContain(
      "time no stage row claimed",
    );
    // Rendered even at zero: "measured, found nothing" is a fact.
    expect(a.rows.find((r) => r.stage === "gate_unattributed")!.seconds).toBe(0.05);
  });

  it("reports an owner, stage, mechanism or verdict it does not recognise", () => {
    const a = readStageAttribution({
      stage_attribution: {
        total_ms: 1_000,
        served_by: "quantum_stack",
        incumbent_ms: 0,
        rows: [{ stage: "teleport", owner: "third_stack", ms: 500, mechanism: "vibes" }],
      },
    })!;
    expect(a.servedBy).toContain("unrecognised verdict (quantum_stack)");
    expect(a.rows[0].label).toContain("unrecognised stage (teleport)");
    expect(a.rows[0].owner).toContain("unrecognised owner (third_stack)");
    expect(a.rows[0].ownerKind).toBe("unknown");
    expect(a.rows[0].detail).toContain("unrecognised mechanism (vibes)");
  });

  it("computes each row's share of the turn for the bar", () => {
    const a = readStageAttribution(LADDER_TURN)!;
    expect(a.rows.find((r) => r.stage === "rewrite")!.share).toBeCloseTo(
      43_220 / 150_200,
      4,
    );
  });
});
