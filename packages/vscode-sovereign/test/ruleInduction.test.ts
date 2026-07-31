import { describe, expect, it } from "vitest";
import { findGuardedSites } from "../src/nextEditSpikeCore";
import { expandRule, induce, shouldFire } from "../src/ruleInduction";

describe("expandRule", () => {
  it("absorbs member-access context and the call paren", () => {
    const doc = '  console.debug("a");\n';
    // unit: "log" → "debug" at offset 10
    const r = expandRule(doc, 10, "log", "debug");
    expect(r).toMatchObject({ find: "console.log(", replace: "console.debug(" });
    expect(r?.guardLeft).toBe(true); // 'c' — don't match inside identifiers
    expect(r?.guardRight).toBe(false); // '(' needs no guard
  });

  it("guards both ends of a pure identifier rename", () => {
    const doc = "let countNext = countNext + 1;\n";
    // unit: inserted "Next" after "count" at offset 9
    const r = expandRule(doc, 9, "", "Next");
    expect(r).toMatchObject({ find: "count", replace: "countNext" });
    expect(r?.guardLeft).toBe(true);
    expect(r?.guardRight).toBe(true);
    // the guards prevent the rule re-matching its own output
    expect(findGuardedSites(doc, "count", 0, true, true)).toEqual([]);
  });

  it("declines no-ops and multi-line units", () => {
    expect(expandRule("abc", 1, "b", "b")).toBeNull();
    expect(expandRule("a\nc", 1, "x", "\n")).toBeNull();
  });
});

describe("induce", () => {
  const rule = { find: "console.log(", replace: "console.debug(", guardLeft: true, guardRight: false };
  const other = { find: "foo", replace: "bar", guardLeft: true, guardRight: true };

  it("anchors on the most recent unit and counts window support", () => {
    expect(induce([rule, other, rule])).toEqual({ rule, support: 2 });
  });

  it("survives uninducible units between supports", () => {
    expect(induce([rule, null, rule])?.support).toBe(2);
  });

  it("returns null with no inducible history", () => {
    expect(induce([])).toBeNull();
    expect(induce([null, null])).toBeNull();
  });
});

describe("shouldFire — the interrupt-policy table", () => {
  const specific = { find: "console.log(", replace: "console.debug(", guardLeft: true, guardRight: false };
  const short = { find: "ab", replace: "ax", guardLeft: true, guardRight: true };

  it("never fires without a remaining site", () => {
    expect(shouldFire(specific, 5, 0)).toBe(false);
  });

  it("fires a specific rule at 2 supports", () => {
    expect(shouldFire(specific, 2, 1)).toBe(true);
  });

  it("holds a short rule until 3 supports", () => {
    expect(shouldFire(short, 2, 10)).toBe(false);
    expect(shouldFire(short, 3, 10)).toBe(true);
  });

  it("never fires on a single edit", () => {
    expect(shouldFire(specific, 1, 10)).toBe(false);
  });
});
