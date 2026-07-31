import { describe, expect, it } from "vitest";
import { buildQueue, shiftAfterApply } from "../src/editQueue";

describe("buildQueue", () => {
  it("captures each edit's expected old text from the request text", () => {
    const text = "console.log(1);\nconsole.log(2);\n";
    const q = buildQueue(text, [
      { start: 16, end: 28, new_text: "console.debug(" },
      { start: 0, end: 12, new_text: "console.debug(" },
    ]);
    expect(q[0].oldText).toBe("console.log(");
    expect(q[1].oldText).toBe("console.log(");
  });
});

describe("shiftAfterApply", () => {
  const applied = { start: 16, end: 28, newText: "console.debug(", oldText: "console.log(" };

  it("shifts edits after the applied site by the length delta", () => {
    const q = [{ start: 32, end: 44, newText: "console.debug(", oldText: "console.log(" }];
    expect(shiftAfterApply(q, applied)[0]).toMatchObject({ start: 34, end: 46 });
  });

  it("leaves wrapped-around edits before the applied site alone", () => {
    const q = [{ start: 0, end: 12, newText: "console.debug(", oldText: "console.log(" }];
    expect(shiftAfterApply(q, applied)[0]).toMatchObject({ start: 0, end: 12 });
  });
});
