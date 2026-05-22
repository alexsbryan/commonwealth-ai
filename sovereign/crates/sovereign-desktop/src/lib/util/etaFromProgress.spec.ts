import { describe, expect, test } from "vitest";
import {
  deriveEta,
  formatPreflightBand,
  formatRefinedTotal,
  formatRemaining,
} from "./etaFromProgress";
import type { CorpusProgressPayload } from "../types";

function payload(
  phase: CorpusProgressPayload["phase"],
  percent: number,
): CorpusProgressPayload {
  return {
    corpus_id: "conversations-anthropic",
    phase,
    percent,
    chunks_processed: 0,
  };
}

describe("deriveEta — warmup suppression", () => {
  test("returns empty label before warmup window", () => {
    const r = deriveEta(payload("extracting", 1), 0, () => 1_000);
    expect(r.label).toBe("");
    expect(r.secondsRemaining).toBeNull();
  });

  test("returns empty label when percent is below warmup threshold and warmup not elapsed", () => {
    const r = deriveEta(payload("extracting", 3), 0, () => 30_000);
    expect(r.label).toBe("");
  });

  test("activates after warmup window even at low percent", () => {
    // 70s elapsed, 2% done → projects ~3430s ≈ ~57 min remaining.
    const r = deriveEta(payload("extracting", 2), 0, () => 70_000);
    expect(r.secondsRemaining).not.toBeNull();
    expect(r.label).toMatch(/min remaining/);
  });

  test("activates once percent crosses 5% even within warmup", () => {
    const r = deriveEta(payload("extracting", 10), 0, () => 30_000);
    expect(r.secondsRemaining).not.toBeNull();
  });
});

describe("deriveEta — terminal phases", () => {
  test("complete suppresses ETA", () => {
    const r = deriveEta(payload("complete", 100), 0, () => 1_000_000);
    expect(r.label).toBe("");
    expect(r.secondsRemaining).toBeNull();
  });

  test("failed suppresses ETA", () => {
    const r = deriveEta(payload("failed", 50), 0, () => 1_000_000);
    expect(r.label).toBe("");
    expect(r.secondsRemaining).toBeNull();
  });
});

describe("deriveEta — monotonic shape", () => {
  test("ETA decreases as percent rises (same wall clock)", () => {
    const early = deriveEta(payload("extracting", 10), 0, () => 120_000);
    const later = deriveEta(payload("extracting", 60), 0, () => 120_000);
    expect(early.secondsRemaining).not.toBeNull();
    expect(later.secondsRemaining).not.toBeNull();
    expect(later.secondsRemaining! < early.secondsRemaining!).toBe(true);
  });

  test("zero / 100 percent suppresses ETA", () => {
    expect(deriveEta(payload("extracting", 0), 0, () => 120_000).label).toBe("");
    expect(deriveEta(payload("extracting", 100), 0, () => 120_000).label).toBe("");
  });
});

describe("formatRemaining — granularity", () => {
  test("under one minute rounds to 10-second granularity", () => {
    expect(formatRemaining(12)).toBe("~10 sec remaining");
    expect(formatRemaining(45)).toBe("~50 sec remaining");
  });

  test("one to five minutes shows minute granularity", () => {
    expect(formatRemaining(90)).toBe("~2 min remaining");
    expect(formatRemaining(270)).toBe("~5 min remaining");
  });

  test("over five minutes rounds to nearest minute", () => {
    expect(formatRemaining(8 * 60 + 20)).toBe("~8 min remaining");
    expect(formatRemaining(34 * 60)).toBe("~34 min remaining");
  });

  test("zero or negative produces empty string", () => {
    expect(formatRemaining(0)).toBe("");
    expect(formatRemaining(-1)).toBe("");
  });
});

describe("formatRefinedTotal", () => {
  test("returns empty during warmup so caller falls back to band", () => {
    const out = formatRefinedTotal(payload("extracting", 1), 0, () => 1_000);
    expect(out).toBe("");
  });

  test("renders integer-minute total once live ETA is active", () => {
    // 120s elapsed, 20% done → remaining = 120 * (80/20) = 480s.
    // total = 120 + 480 = 600s = 10 min.
    const out = formatRefinedTotal(payload("extracting", 20), 0, () => 120_000);
    expect(out).toContain("10 min");
    expect(out).toContain("Estimated total time");
  });

  test("refined total shrinks as observed rate improves", () => {
    // Two snapshots with the same wall clock; the second saw more
    // percent → its projected total is lower.
    const early = formatRefinedTotal(payload("extracting", 5), 0, () => 60_000);
    const later = formatRefinedTotal(payload("extracting", 50), 0, () => 60_000);
    const earlyMin = parseInt(early.match(/(\d+)/)?.[1] ?? "0", 10);
    const laterMin = parseInt(later.match(/(\d+)/)?.[1] ?? "0", 10);
    expect(laterMin).toBeLessThan(earlyMin);
  });

  test("hides on terminal phases", () => {
    expect(formatRefinedTotal(payload("complete", 100), 0, () => 1_000_000)).toBe("");
    expect(formatRefinedTotal(payload("failed", 50), 0, () => 1_000_000)).toBe("");
  });
});

describe("formatPreflightBand", () => {
  test("renders a +/- 30% band as integer-minute range when over 5 min", () => {
    const out = formatPreflightBand(10);
    expect(out).toContain("min");
    // 7-13 minute band.
    expect(out).toContain("7");
    expect(out).toContain("13");
  });

  test("clamps low end at 0.5 min so tiny imports don't read as zero", () => {
    const out = formatPreflightBand(0.4);
    expect(out).toContain("0.5");
  });

  test("returns empty for non-finite / zero", () => {
    expect(formatPreflightBand(0)).toBe("");
    expect(formatPreflightBand(NaN)).toBe("");
  });
});
