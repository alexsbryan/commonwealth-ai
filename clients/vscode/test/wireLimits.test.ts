import { describe, expect, it } from "vitest";

import {
  MAX_UNIT_BYTES,
  bytes,
  sliceWhole,
  unitFitsWire,
} from "../src/wireLimits";

// These guard one shared failure mode: a request the daemon refuses is
// not a dropped suggestion, it is a 400 for the WHOLE request — and the
// unit that caused it stays in the history window, so it kills every
// later prediction until it ages out. Both bugs below shipped that way.

describe("byte accounting matches the daemon's ruler", () => {
  it("measures UTF-8 bytes, not UTF-16 units", () => {
    expect(bytes("abc")).toBe(3);
    expect(bytes("世界")).toBe(6); // 2 UTF-16 units, 6 bytes
    expect(bytes("💡")).toBe(4); // 2 UTF-16 units, 4 bytes
  });

  it("refuses a unit that is small in chars but oversized in bytes", () => {
    // 700 CJK chars: 700 UTF-16 units (would pass a 2048-CHAR check)
    // but 2100 bytes — the daemon rejects the whole request.
    const cjk = "世".repeat(700);
    expect(cjk.length).toBeLessThan(MAX_UNIT_BYTES);
    expect(bytes(cjk)).toBeGreaterThan(MAX_UNIT_BYTES);
    expect(unitFitsWire(cjk)).toBe(false);
    expect(unitFitsWire("ok", "also ok")).toBe(true);
  });
});

describe("context slicing never emits a lone surrogate", () => {
  // A lone surrogate survives JSON.stringify as a \udXXX escape that
  // serde_json rejects outright.
  const lone = (s: string) =>
    [...s].some((c) => {
      const n = c.charCodeAt(0);
      return c.length === 1 && n >= 0xd800 && n <= 0xdfff;
    });

  it("moves boundaries inward rather than splitting a pair", () => {
    const doc = `const a = "💡";`;
    // Every possible window over a document containing an emoji.
    for (let i = 0; i <= doc.length; i++) {
      for (let j = i; j <= doc.length; j++) {
        const out = sliceWhole(doc, i, j);
        expect(lone(out), `split at ${i}..${j}: ${JSON.stringify(out)}`).toBe(false);
        expect(doc.includes(out)).toBe(true);
      }
    }
  });

  it("round-trips through JSON for astral text at any boundary", () => {
    const doc = "🎉".repeat(20);
    for (let i = 0; i <= doc.length; i++) {
      const out = sliceWhole(doc, i, i + 7);
      expect(() => JSON.parse(JSON.stringify({ left: out }))).not.toThrow();
      expect(lone(out)).toBe(false);
    }
  });

  it("still returns the whole slice when no pair is straddled", () => {
    expect(sliceWhole("hello world", 0, 5)).toBe("hello");
    expect(sliceWhole("hello", 3, 99)).toBe("lo");
    expect(sliceWhole("hello", 4, 2)).toBe("");
    expect(sliceWhole("", 0, 10)).toBe("");
  });
});
