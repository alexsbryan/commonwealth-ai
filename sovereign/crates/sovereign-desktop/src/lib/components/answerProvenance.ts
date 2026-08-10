// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The desktop reading of `metadata.answer_segments` and the typed
// abstention that rides beside it — `NATIVE_GROUNDING.md` §6, composed
// at P1 (`sovereign/docs/specs/NATIVE_GROUNDING_PARITY_PLAN.md` §4.1).
//
// **What this renders, and what it must never imply.** A `grounded`
// segment means the sentence was found VERBATIM inside one retrieved
// passage, at a real address. It does NOT mean a judge agreed the claim
// is true: the span resolver certifies at 0.7429 precision against the
// incumbent judge (`sovereign/bench/calibration/resolver-precision/`),
// which is why every label below talks about WHERE THE TEXT IS and none
// of them talks about whether it is right. The CLI footer
// (`sovereign-cli-llm/src/chat_cmd/render.rs::answer_segments_footer`)
// carries the same four labels; they are the same vocabulary because
// they describe the same enum
// (`sovereign-contracts/src/types/grounding_verdict.rs::SegmentKind`).
//
// **Absent and empty are different, and are rendered differently.** A
// turn that never segmented (flag off, or H1 had no instrument) has no
// `answer_segments` key at all and this module returns `null` — nothing
// renders. A turn that segmented and resolved nothing returns a strip
// saying so. Collapsing the two would report a measurement that never
// ran (ARCH §18.3).

import { byteSlice } from "../utils/byteOffsets";

/** Wire shape of `SegmentKind` — a serde-tagged enum. */
export type SegmentKindWire =
  | {
      kind: "grounded";
      chunk_id: string;
      span: { start: number; end: number };
      /** `(corpus_id, chunk_id)` — what makes the badge openable. Absent
       *  when the runtime could not resolve a handle for that pool slot;
       *  the badge then renders un-openable rather than pointing at a
       *  guess. */
      address?: { corpus_id: string; chunk_id: number } | null;
    }
  | { kind: "parametric" }
  | { kind: "inference" }
  | { kind: "unverified" };

/** Wire shape of `AnswerSegment`. */
export interface AnswerSegmentWire {
  text_range: { start: number; end: number };
  kind: SegmentKindWire;
  margin?: number | null;
}

/** One row of the provenance strip, ready to render. */
export interface ProvenanceRow {
  /** The stretch of the answer this row is about, as the user read it. */
  text: string;
  /** Wire spelling of the segment kind, or `"unknown"` for a kind this
   *  build does not recognise — reported, never coerced into one of the
   *  four (ARCH §18.3). */
  kind: SegmentKindWire["kind"] | "unknown";
  /** Human label. Talks about location, never about truth. */
  label: string;
  /** Pool-slot id, on grounded rows only. Diagnostic — NOT openable. */
  chunkId: string | null;
  /** The openable address, when the runtime resolved one. `null` on a
   *  grounded row means "found in your sources, nowhere to send you" —
   *  rendered as an un-openable badge, never as a link to a guess. */
  address: { corpusId: string; chunkId: number } | null;
}

export interface AnswerProvenance {
  rows: ProvenanceRow[];
  grounded: number;
  /** Grounded rows that resolve to a real address. The P1 citability
   *  bar is `groundedAddressed === grounded`. */
  groundedAddressed: number;
  unverified: number;
  total: number;
}

/** The four labels, defined once. Wire spelling → what the user reads. */
const LABELS: Record<string, string> = {
  grounded: "found in your sources",
  parametric: "the model's own words",
  inference: "drawn from your sources, not copied from one passage",
  unverified: "not found in your sources",
};

/** Read the provenance strip out of a message's metadata.
 *
 *  `null` means "this turn never segmented" — render nothing. */
export function readAnswerProvenance(
  metadata: Record<string, unknown> | undefined,
  answerText: string,
): AnswerProvenance | null {
  const raw = metadata?.answer_segments;
  if (raw === undefined || raw === null || !Array.isArray(raw)) return null;
  const segs = raw as AnswerSegmentWire[];
  const rows: ProvenanceRow[] = [];
  for (const s of segs) {
    const start = s?.text_range?.start;
    const end = s?.text_range?.end;
    const wire = s?.kind?.kind;
    const known = typeof wire === "string" && wire in LABELS;
    const addr =
      s?.kind && "address" in s.kind ? (s.kind.address ?? null) : null;
    rows.push({
      text:
        typeof start === "number" && typeof end === "number"
          ? byteSlice(answerText, start, end)
          : "",
      kind: known ? (wire as ProvenanceRow["kind"]) : "unknown",
      label: known ? LABELS[wire] : `unrecognised segment (${wire ?? "no kind"})`,
      chunkId:
        s?.kind && "chunk_id" in s.kind && typeof s.kind.chunk_id === "string"
          ? s.kind.chunk_id
          : null,
      address:
        addr && typeof addr.corpus_id === "string" && typeof addr.chunk_id === "number"
          ? { corpusId: addr.corpus_id, chunkId: addr.chunk_id }
          : null,
    });
  }
  const grounded = rows.filter((r) => r.kind === "grounded");
  return {
    rows,
    grounded: grounded.length,
    groundedAddressed: grounded.filter((r) => r.address !== null).length,
    unverified: rows.filter((r) => r.kind === "unverified").length,
    total: rows.length,
  };
}

/** The turn's typed abstention, when it abstained.
 *
 *  `action` is the gate's own vocabulary — every consumer in the runtime
 *  tests `startsWith("abstained")`, and so does this one, so the desktop
 *  and the runtime cannot drift about what an abstention is. The prose is
 *  NOT consulted: a bubble that says "I found nothing" while the gate
 *  released an answer is the model's wording, not a verdict, and the
 *  typed field is what this renders. */
export interface TypedAbstention {
  /** The gate action, verbatim, for the disclosure line. */
  action: string;
  /** H1's answerability score for the turn, when the native path ran.
   *  TELEMETRY: it did not decide this abstention and the copy must not
   *  say it did (parity plan §4.1 — admission is never enforced at P1). */
  nativeAnswerability: number | null;
}

export function readTypedAbstention(
  metadata: Record<string, unknown> | undefined,
): TypedAbstention | null {
  const gg = metadata?.grounding_gate as
    | { action?: unknown; native_answerability?: unknown }
    | null
    | undefined;
  const action = gg?.action;
  if (typeof action !== "string" || !action.startsWith("abstained")) return null;
  return {
    action,
    nativeAnswerability:
      typeof gg?.native_answerability === "number"
        ? gg.native_answerability
        : null,
  };
}
