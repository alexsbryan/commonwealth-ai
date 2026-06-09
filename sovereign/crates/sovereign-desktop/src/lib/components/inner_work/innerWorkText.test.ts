import { describe, it, expect } from "vitest";
import {
  humanizeWitnessError,
  tokenize,
  formatRelativeDate,
  formatDateline,
} from "./innerWorkText";

describe("humanizeWitnessError", () => {
  it("gives the context-overflow case a tailored 'new entry' hint", () => {
    expect(humanizeWitnessError("Error: Prompt too long (8200 tokens)")).toContain(
      "start a new entry",
    );
  });

  it("collapses everything else to a generic non-blaming line", () => {
    const msg = humanizeWitnessError("ECONNREFUSED 127.0.0.1:9741");
    expect(msg).toBe("The witness couldn't respond. Try again, or keep writing.");
  });
});

describe("tokenize", () => {
  it("keeps only words longer than 3 chars, lowercased", () => {
    expect([...tokenize("The quick brown fox")]).toEqual(["quick", "brown"]);
  });

  it("splits on non-word chars and dedupes", () => {
    expect(tokenize("Hello, hello... HELLO!")).toEqual(new Set(["hello"]));
    expect(tokenize("test test testing")).toEqual(new Set(["test", "testing"]));
  });

  it("returns an empty set for blank / short-only input", () => {
    expect(tokenize("").size).toBe(0);
    expect(tokenize("a an the for").size).toBe(0);
  });
});

describe("formatRelativeDate", () => {
  // Fixed reference instant so each branch is deterministic.
  const now = new Date(Date.UTC(2026, 5, 9, 12, 0, 0));
  const nowSec = now.getTime() / 1000;

  it("returns 'earlier today' for the same day (incl. future)", () => {
    expect(formatRelativeDate(nowSec - 3600, now)).toBe("earlier today");
    expect(formatRelativeDate(nowSec + 3600, now)).toBe("earlier today");
  });

  it("returns 'yesterday' at one day's distance", () => {
    expect(formatRelativeDate(nowSec - 25 * 3600, now)).toBe("yesterday");
  });

  it("returns 'N days ago' for 2–6 days", () => {
    expect(formatRelativeDate(nowSec - (3 * 86400 + 3600), now)).toBe("3 days ago");
  });

  it("falls back to an absolute date at 7+ days", () => {
    const out = formatRelativeDate(nowSec - 10 * 86400, now);
    expect(out).not.toMatch(/ago|yesterday|today/);
    expect(out).toMatch(/\d/);
  });
});

describe("formatDateline", () => {
  it("renders a non-empty dateline carrying the day number", () => {
    const out = formatDateline("2026-06-09");
    expect(out.length).toBeGreaterThan(0);
    expect(out).toMatch(/9/);
  });

  it("differs across distinct dates", () => {
    expect(formatDateline("2026-06-09")).not.toBe(formatDateline("2026-01-01"));
  });
});
