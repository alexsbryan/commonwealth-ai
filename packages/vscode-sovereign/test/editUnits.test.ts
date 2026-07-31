import { describe, expect, it } from "vitest";
import {
  RawChange,
  UnitCoalescer,
  unitsFromMultiChange,
} from "../src/editUnits";

// Simulates a document + change stream: applies each change to the
// running text (as VSCode would) while feeding the coalescer with
// the pre-change snapshot, exactly like the controller does.
function drive(
  initial: string,
  changes: RawChange[],
): { closed: ReturnType<UnitCoalescer["feed"]>[]; text: string; c: UnitCoalescer } {
  const c = new UnitCoalescer();
  let text = initial;
  const closed = [];
  for (const ch of changes) {
    closed.push(c.feed(ch, text));
    text = text.slice(0, ch.offset) + ch.inserted + text.slice(ch.offset + ch.deleted.length);
  }
  return { closed, text, c };
}

describe("UnitCoalescer", () => {
  const doc = 'console.log("a");\nconsole.log("b");\n';

  it("coalesces select-and-type into one before/after unit", () => {
    // select "log" at offset 8, type d-e-b-u-g
    const { closed, text, c } = drive(doc, [
      { offset: 8, deleted: "log", inserted: "d" },
      { offset: 9, deleted: "", inserted: "e" },
      { offset: 10, deleted: "", inserted: "b" },
      { offset: 11, deleted: "", inserted: "u" },
      { offset: 12, deleted: "", inserted: "g" },
    ]);
    expect(closed).toEqual([null, null, null, null, null]);
    expect(c.settle(text)).toEqual({ start: 8, before: "log", after: "debug" });
  });

  it("coalesces backspace-then-retype into one unit", () => {
    // cursor after "log" (offset 11): backspace ×3, then type debug
    const { closed, text, c } = drive(doc, [
      { offset: 10, deleted: "g", inserted: "" },
      { offset: 9, deleted: "o", inserted: "" },
      { offset: 8, deleted: "l", inserted: "" },
      { offset: 8, deleted: "", inserted: "d" },
      { offset: 9, deleted: "", inserted: "e" },
      { offset: 10, deleted: "", inserted: "b" },
      { offset: 11, deleted: "", inserted: "u" },
      { offset: 12, deleted: "", inserted: "g" },
    ]);
    expect(closed.every((u) => u === null)).toBe(true);
    expect(c.settle(text)).toEqual({ start: 8, before: "log", after: "debug" });
  });

  it("closes the open unit when an edit lands far away", () => {
    const { closed } = drive(doc, [
      { offset: 8, deleted: "log", inserted: "debug" },
      // second site, well past the first unit's span
      { offset: 28, deleted: "log", inserted: "debug" },
    ]);
    expect(closed[0]).toBeNull();
    expect(closed[1]).toEqual({ start: 8, before: "log", after: "debug" });
  });

  it("discards a burst that undoes itself", () => {
    const { text, c } = drive(doc, [
      { offset: 8, deleted: "log", inserted: "x" },
      { offset: 8, deleted: "x", inserted: "log" },
    ]);
    expect(c.settle(text)).toBeNull();
  });

  it("settle on an idle coalescer returns null", () => {
    expect(new UnitCoalescer().settle(doc)).toBeNull();
  });
});

describe("unitsFromMultiChange", () => {
  it("maps each cursor's change to post-event coordinates", () => {
    // two cursors replacing "log" (3 chars) with "debug" (5 chars)
    const units = unitsFromMultiChange([
      { offset: 28, deleted: "log", inserted: "debug" },
      { offset: 8, deleted: "log", inserted: "debug" },
    ]);
    expect(units).toEqual([
      { start: 8, before: "log", after: "debug" },
      { start: 30, before: "log", after: "debug" }, // 28 + (5-3)
    ]);
  });

  it("drops no-op changes", () => {
    expect(unitsFromMultiChange([{ offset: 3, deleted: "a", inserted: "a" }])).toEqual([]);
  });
});
