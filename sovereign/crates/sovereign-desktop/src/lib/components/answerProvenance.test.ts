// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, it, expect } from "vitest";
import {
  readAnswerProvenance,
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
