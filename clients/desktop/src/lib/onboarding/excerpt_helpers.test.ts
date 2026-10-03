// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  cleanExcerptBody,
  cleanExcerptTitle,
  deriveExcerptStarters,
} from "./excerpt_helpers";
import type { ExcerptChunk } from "../types";

describe("cleanExcerptTitle", () => {
  it("strips leading numeric prefixes and trailing years", () => {
    expect(cleanExcerptTitle("11. Erwin Schrodinger What is Life 1944")).toBe(
      "Erwin Schrodinger What is Life",
    );
    expect(cleanExcerptTitle("03_The Sovereignty Reader 2021")).toBe(
      "The Sovereignty Reader",
    );
    expect(cleanExcerptTitle("(2019) An Annual Report")).toBe(
      "(2019) An Annual Report",
    );
  });

  it("strips surviving file extensions", () => {
    expect(cleanExcerptTitle("What Is Life pdf")).toBe("What Is Life");
    expect(cleanExcerptTitle("Notes md")).toBe("Notes");
  });

  it("leaves clean titles alone", () => {
    expect(cleanExcerptTitle("Water Quality Report")).toBe(
      "Water Quality Report",
    );
    expect(cleanExcerptTitle("From Dictatorship to Democracy")).toBe(
      "From Dictatorship to Democracy",
    );
  });

  it("handles empty / whitespace input", () => {
    expect(cleanExcerptTitle("")).toBe("Untitled");
    expect(cleanExcerptTitle("   ")).toBe("Untitled");
  });
});

describe("cleanExcerptBody — title-prefix removal", () => {
  // The chunker prepends the title to each chunk for retrieval
  // context. Display should strip that echo.
  it("strips the raw title when it starts the chunk body", () => {
    const out = cleanExcerptBody(
      "Water Quality Report are also described. A distilled list of key findings from the overall report is also provided.",
      "Water Quality Report",
    );
    expect(out.startsWith("Water Quality Report")).toBe(false);
    expect(out).toContain("A distilled list of key findings");
  });

  it("strips the raw title when it includes a leading numeric prefix", () => {
    const out = cleanExcerptBody(
      "11. Erwin Schrodinger What is Life 1944 novel you are reading is probably nearer to your heart, certainly more intensely alive and better known to you. Yet there has been no intermediate break, no death.",
      "11. Erwin Schrodinger What is Life 1944",
    );
    // The title-prefixed portion is gone.
    expect(out.includes("1944 novel you are reading")).toBe(false);
    // The real content stays intact.
    expect(out).toContain("no intermediate break");
  });

  it("seeks forward to a sentence start when one lives in the opening window", () => {
    // "Water Quality Report" strip leaves "are also described. A
    // distilled list of key findings..." — ~20 chars before the
    // next sentence start, comfortably inside the window.
    const out = cleanExcerptBody(
      "Water Quality Report are also described. A distilled list of key findings from the overall report is also provided.",
      "Water Quality Report",
    );
    expect(out.startsWith("A distilled list")).toBe(true);
  });

  it("prepends an ellipsis when the next sentence start is too far", () => {
    // Here the next sentence is ~108 chars in after the strip —
    // past the 30% window — so we keep the fragment and flag it.
    const out = cleanExcerptBody(
      "11. Erwin Schrodinger What is Life 1944 novel you are reading is probably nearer to your heart, certainly more intensely alive and better known to you. Yet there has been no intermediate break, no death.",
      "11. Erwin Schrodinger What is Life 1944",
    );
    expect(out.startsWith("…novel you are reading")).toBe(true);
    expect(out).toContain("no intermediate break");
  });

  it("does not strip a title that is a substring of a real opening word", () => {
    // Title "Cat" is a substring of "Catastrophe"; refuse to
    // strip because the character after "Cat" isn't a boundary.
    const out = cleanExcerptBody(
      "Catastrophic flooding reshaped the valley by dawn.",
      "Cat",
    );
    expect(out.startsWith("Catastrophic flooding")).toBe(true);
  });

  it("appends an ellipsis when the chunk doesn't end on a sentence terminator", () => {
    const out = cleanExcerptBody(
      "A claim that doesn't quite end",
      null,
    );
    expect(out.endsWith("…")).toBe(true);
  });

  it("handles empty input gracefully", () => {
    expect(cleanExcerptBody("", "Title")).toBe("");
    expect(cleanExcerptBody("   ", null)).toBe("");
  });
});

describe("deriveExcerptStarters", () => {
  it("produces a cross-document question first when 2+ sources exist", () => {
    const excerpts: ExcerptChunk[] = [
      { text: "t1", source_name: "11. Alpha 2020", page_ref: null },
      { text: "t2", source_name: "Beta", page_ref: null },
    ];
    const starters = deriveExcerptStarters(excerpts, 4);
    expect(starters.length).toBeGreaterThanOrEqual(2);
    expect(starters[0].atom_id).toBe("excerpt-seed::cross");
    expect(starters[0].text.toLowerCase()).toContain("alpha");
    expect(starters[0].text.toLowerCase()).toContain("beta");
    // Titles are cleaned — no "11." leaks.
    expect(starters[0].text.includes("11.")).toBe(false);
  });

  it("falls back to per-document questions when only one source exists", () => {
    const excerpts: ExcerptChunk[] = [
      { text: "t1", source_name: "Water Quality Report", page_ref: null },
    ];
    const starters = deriveExcerptStarters(excerpts, 4);
    expect(starters.length).toBe(1);
    expect(starters[0].atom_id).not.toBe("excerpt-seed::cross");
    expect(starters[0].text).toContain("Water Quality Report");
  });

  it("respects the limit", () => {
    const excerpts: ExcerptChunk[] = Array.from({ length: 5 }, (_, i) => ({
      text: `t${i}`,
      source_name: `Doc ${i}`,
      page_ref: null,
    }));
    const starters = deriveExcerptStarters(excerpts, 3);
    expect(starters.length).toBe(3);
  });

  it("returns empty for no excerpts", () => {
    expect(deriveExcerptStarters([], 4)).toEqual([]);
  });
});
