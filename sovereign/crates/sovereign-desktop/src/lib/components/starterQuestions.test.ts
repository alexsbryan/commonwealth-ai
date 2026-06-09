// Pure-logic tests for the starter-question pool helpers (extracted
// from ChatView). The reactive wiring is covered by the e2e specs; this
// pins the interleave/window/cursor math that has no compiler signal.

import { describe, it, expect } from "vitest";
import {
  interleaveStarters,
  visibleStarters,
  advanceStarterCursor,
  type StarterWithCorpus,
} from "./starterQuestions";

function q(text: string, corpus: string): StarterWithCorpus {
  return {
    text,
    atom_id: text,
    source_section: null,
    question_type: "thematic",
    corpus_id: corpus,
  };
}

describe("interleaveStarters", () => {
  it("round-robins across corpora (corpus-fair order)", () => {
    const a = [q("a1", "a"), q("a2", "a")];
    const b = [q("b1", "b"), q("b2", "b")];
    const out = interleaveStarters([a, b], 10);
    expect(out.map((x) => x.text)).toEqual(["a1", "b1", "a2", "b2"]);
  });

  it("caps the pool at `target`", () => {
    const a = [q("a1", "a"), q("a2", "a"), q("a3", "a")];
    const b = [q("b1", "b"), q("b2", "b"), q("b3", "b")];
    expect(interleaveStarters([a, b], 3).map((x) => x.text)).toEqual([
      "a1",
      "b1",
      "a2",
    ]);
  });

  it("drains a longer list when the other is exhausted", () => {
    const a = [q("a1", "a"), q("a2", "a"), q("a3", "a")];
    const b = [q("b1", "b")];
    expect(interleaveStarters([a, b], 10).map((x) => x.text)).toEqual([
      "a1",
      "b1",
      "a2",
      "a3",
    ]);
  });

  it("returns empty when there are no corpora", () => {
    expect(interleaveStarters([], 10)).toEqual([]);
    expect(interleaveStarters([[], []], 10)).toEqual([]);
  });
});

describe("visibleStarters", () => {
  const pool = [q("0", "a"), q("1", "a"), q("2", "a")];

  it("returns the window starting at the cursor", () => {
    expect(visibleStarters(pool, 0, 2).map((x) => x.text)).toEqual(["0", "1"]);
    expect(visibleStarters(pool, 1, 2).map((x) => x.text)).toEqual(["1", "2"]);
  });

  it("wraps modulo the pool length", () => {
    expect(visibleStarters(pool, 2, 2).map((x) => x.text)).toEqual(["2", "0"]);
  });

  it("is empty for an empty pool (no divide-by-zero)", () => {
    expect(visibleStarters([], 0, 2)).toEqual([]);
  });
});

describe("advanceStarterCursor", () => {
  it("advances by `step` while inside the pool", () => {
    expect(advanceStarterCursor(0, 12, 2)).toEqual({
      cursor: 2,
      shouldRefresh: false,
    });
  });

  it("resets to 0 and signals refresh when the next window would overrun", () => {
    // pool of 4, step 2: cursor 2 -> next 4 >= 4 -> wrap + refresh
    expect(advanceStarterCursor(2, 4, 2)).toEqual({
      cursor: 0,
      shouldRefresh: true,
    });
  });
});
