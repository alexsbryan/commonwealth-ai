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

// ─── G4: the stack-attribution strip ─────────────────────────────────
//
// `metadata.stage_attribution` — `NATIVE_GROUNDING_ECONOMY.md` §3.4 (G4,
// the function no stage owned) and §9 Phase 1. The Rust side is
// `sovereign-contracts/src/types/stage_attribution.rs::TurnStageLedger`
// and carries the design rationale; this is the reading half.
//
// **This module computes NO attribution.** Which system owns a stage,
// which stacks served the turn, and how much of the turn the old stack
// took are all decided by the runtime and read here (ARCH §10.6 — if both
// could compute it, the UI reads, so a desktop strip and a CLI footer
// cannot disagree). The only thing this file decides is wording.
//
// **Absent and empty are different.** A turn that opened no ledger has no
// `stage_attribution` key and this returns `null` — nothing renders. There
// is no such thing as an empty ledger on the wire.

/** Wire spelling of `StackOwner`. Closed set in Rust. */
export type StackOwnerWire = "native" | "incumbent" | "shared";

/** Wire shape of one `StageRow`. */
export interface StageRowWire {
  stage: string;
  owner: string;
  ms: number;
  mechanism?: string | null;
  cause?: string | null;
  calls?: number | null;
}

/** Wire shape of `TurnStageLedger`. */
export interface StageAttributionWire {
  total_ms: number;
  rows: StageRowWire[];
  served_by: string;
  incumbent_ms: number;
}

/** One row of the strip, ready to render. */
export interface StageAttributionRow {
  /** Stage name the reader sees. */
  label: string;
  /** Wire spelling, for `data-` hooks and tests. */
  stage: string;
  /** Owner badge text — "new" / "OLD STACK" / "shared". */
  owner: string;
  /** Wire owner, or `"unknown"` for an owner this build does not know.
   *  Reported, never coerced into one of the three (ARCH §18.3). */
  ownerKind: StackOwnerWire | "unknown";
  seconds: number;
  /** Fraction of the turn, 0..1 — for the bar width. */
  share: number;
  /** Mechanism + cause + call count, joined. Empty when the row has none. */
  detail: string;
  /** Residual rows are arithmetic, not observed stages, and are rendered
   *  differently. Read from the stage id, not guessed from the label. */
  isResidual: boolean;
}

export interface StageAttribution {
  /** Turn wall time in seconds. */
  totalSeconds: number;
  /** The headline: which stack(s) served the turn. */
  servedBy: string;
  /** True when any incumbent-owned stage executed — the one thing the
   *  operator asked to be able to see at a glance. */
  oldStackRan: boolean;
  /** Seconds the old stack took. `0` when it did not run. */
  oldStackSeconds: number;
  rows: StageAttributionRow[];
}

/** Stage id → what the reader sees. Mirrors `StageId::label` in Rust; the
 *  wire spellings are pinned by a test on both sides. */
const STAGE_LABELS: Record<string, string> = {
  retrieval: "retrieval",
  admission: "admission",
  draft: "draft",
  audit: "audit",
  rewrite: "rewrite",
  re_audit: "re-audit",
  retry: "retry",
  verify: "verify",
  citation: "citation",
  segments: "segments",
  gate_unattributed: "gate — unattributed",
  turn_unattributed: "turn — unattributed",
};

/** Owner → badge text. Mirrors `StackOwner::label` in Rust. */
const OWNER_LABELS: Record<string, string> = {
  native: "new",
  incumbent: "OLD STACK",
  shared: "shared",
};

/** Mechanism → phrase. Mirrors `StageMechanism::label` in Rust. */
const MECHANISM_LABELS: Record<string, string> = {
  surgical_rewrite: "surgical span edits",
  full_resynthesis: "full re-synthesis (surgical fell back)",
  per_claim_judge: "per-claim generative judge",
  deterministic: "deterministic containment",
};

/** Cause → phrase. Mirrors `StageCause::label` in Rust. */
const CAUSE_LABELS: Record<string, string> = {
  every_turn: "runs on every turn",
  audit_found_failures: "the audit found unsupported claims",
  rewrite_produced_new_prose: "exists only because the rewrite ran",
  violation_over_threshold: "violation probability crossed the threshold",
};

/** Served-by → headline phrase. Mirrors `ServedBy::label` in Rust. */
const SERVED_BY_LABELS: Record<string, string> = {
  native_only: "the new stack only",
  incumbent_only: "the OLD stack only",
  both_stacks: "BOTH stacks",
  chain_floor_only: "no grounding stack ran",
};

const RESIDUAL_STAGES = new Set(["gate_unattributed", "turn_unattributed"]);

/** Read the stack-attribution strip out of a message's metadata.
 *
 *  `null` means "this turn opened no ledger" — render nothing. */
export function readStageAttribution(
  metadata: Record<string, unknown> | undefined,
): StageAttribution | null {
  const raw = metadata?.stage_attribution as StageAttributionWire | null | undefined;
  if (!raw || typeof raw !== "object" || !Array.isArray(raw.rows)) return null;
  const totalMs = typeof raw.total_ms === "number" ? raw.total_ms : 0;

  const rows: StageAttributionRow[] = raw.rows.map((r) => {
    const stage = typeof r?.stage === "string" ? r.stage : "";
    const owner = typeof r?.owner === "string" ? r.owner : "";
    const ms = typeof r?.ms === "number" ? r.ms : 0;
    const known = owner in OWNER_LABELS;
    const bits: string[] = [];
    const mech = typeof r?.mechanism === "string" ? r.mechanism : null;
    if (mech) bits.push(MECHANISM_LABELS[mech] ?? `unrecognised mechanism (${mech})`);
    const cause = typeof r?.cause === "string" ? r.cause : null;
    if (cause) bits.push(CAUSE_LABELS[cause] ?? `unrecognised cause (${cause})`);
    if (typeof r?.calls === "number") {
      bits.push(`${r.calls} model call${r.calls === 1 ? "" : "s"}`);
    }
    if (RESIDUAL_STAGES.has(stage)) bits.push("time no stage row claimed");
    return {
      label: STAGE_LABELS[stage] ?? `unrecognised stage (${stage || "none"})`,
      stage,
      // An unknown owner is SAID, not silently rendered as shared: a build
      // that meets a fourth owner must not quietly file it under "neither
      // stack" (ARCH §18.3).
      owner: known ? OWNER_LABELS[owner] : `unrecognised owner (${owner || "none"})`,
      ownerKind: known ? (owner as StackOwnerWire) : "unknown",
      seconds: ms / 1000,
      share: totalMs > 0 ? Math.min(1, ms / totalMs) : 0,
      detail: bits.join(" · "),
      isResidual: RESIDUAL_STAGES.has(stage),
    };
  });

  const servedByWire = typeof raw.served_by === "string" ? raw.served_by : "";
  const incumbentMs = typeof raw.incumbent_ms === "number" ? raw.incumbent_ms : 0;
  return {
    totalSeconds: totalMs / 1000,
    servedBy:
      SERVED_BY_LABELS[servedByWire] ??
      `unrecognised verdict (${servedByWire || "none"})`,
    // Read from the runtime's own derivation, NOT re-derived by counting
    // rows here: one producer, one name.
    oldStackRan: servedByWire === "incumbent_only" || servedByWire === "both_stacks",
    oldStackSeconds: incumbentMs / 1000,
    rows,
  };
}
