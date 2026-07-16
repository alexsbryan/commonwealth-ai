// SPDX-License-Identifier: AGPL-3.0-or-later
// Tests for the glassbox ETA helpers. The ETA is derived from the backend's
// real embed throughput (chunks_per_sec + chunks_total forwarded from
// IngestProgress::Embedding), NOT a fabricated client guess — so the honest
// contract is: no rate or no total ⇒ no estimate (null ⇒ "—"), never a made-up
// number.
import { describe, it, expect } from "vitest";
import { etaSecondsFor, formatEta } from "./corpusProgress.svelte";
import type { CorpusProgressPayload } from "../types";

function payload(over: Partial<CorpusProgressPayload> = {}): CorpusProgressPayload {
  return {
    corpus_id: "obsidian-vault-abc",
    phase: "embedding",
    percent: 50,
    chunks_processed: 1000,
    chunks_total: 2000,
    chunks_per_sec: 50,
    ...over,
  };
}

describe("etaSecondsFor", () => {
  it("computes remaining/rate from backend throughput", () => {
    // (2000 - 1000) / 50 = 20s
    expect(etaSecondsFor(payload())).toBe(20);
  });

  it("returns null when there is no live rate (can't honestly estimate)", () => {
    expect(etaSecondsFor(payload({ chunks_per_sec: 0 }))).toBeNull();
    expect(etaSecondsFor(payload({ chunks_per_sec: undefined }))).toBeNull();
  });

  it("returns null when the total is unknown", () => {
    expect(etaSecondsFor(payload({ chunks_total: 0 }))).toBeNull();
    expect(etaSecondsFor(payload({ chunks_total: undefined }))).toBeNull();
  });

  it("returns 0 when already past the total (no negative ETA)", () => {
    expect(etaSecondsFor(payload({ chunks_processed: 2500 }))).toBe(0);
  });

  it("returns null for an absent payload", () => {
    expect(etaSecondsFor(undefined)).toBeNull();
  });
});

describe("formatEta", () => {
  it("renders null as an em-dash (no estimate)", () => {
    expect(formatEta(null)).toBe("—");
  });
  it("renders sub-90s as seconds", () => {
    expect(formatEta(20)).toBe("~20s");
    expect(formatEta(89)).toBe("~89s");
  });
  it("renders minutes for the mid range", () => {
    expect(formatEta(240)).toBe("~4 min");
    expect(formatEta(90)).toBe("~2 min");
  });
  it("renders hours past 90 minutes", () => {
    expect(formatEta(3600 * 2)).toBe("~2.0 h");
    expect(formatEta(3600 * 12)).toBe("~12 h");
  });
  it("renders a near-zero ETA as 'almost done'", () => {
    expect(formatEta(0)).toBe("almost done");
  });
  it("always marks the estimate as approximate with a ~", () => {
    expect(formatEta(45).startsWith("~")).toBe(true);
    expect(formatEta(300).startsWith("~")).toBe(true);
  });
});
