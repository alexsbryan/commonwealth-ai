import { describe, expect, it } from "vitest";
import { captureContext } from "../src/context";

const doc = [
  "line one",
  "line two",
  "fn main() {",
  "    let x = 1;",
  "    let y = x + ",
  "}",
].join("\n");

describe("captureContext", () => {
  it("splits at the offset", () => {
    const offset = doc.indexOf("x + ") + 4;
    const { prefix, suffix } = captureContext(doc, offset, 60, 20);
    expect(prefix.endsWith("let y = x + ")).toBe(true);
    expect(suffix.startsWith("\n}")).toBe(true);
  });

  it("truncates the prefix at line boundaries only", () => {
    const offset = doc.length;
    const { prefix } = captureContext(doc, offset, 3, 20);
    // 3 lines max; the oldest kept line is complete (no mid-line cut).
    const lines = prefix.split("\n");
    expect(lines).toHaveLength(3);
    expect(lines[0]).toBe("    let x = 1;");
  });

  it("caps the suffix at maxSuffixLines", () => {
    const { suffix } = captureContext(doc, 0, 60, 2);
    expect(suffix.split("\n")).toHaveLength(2);
  });

  it("keeps the partial cursor line in the prefix", () => {
    const offset = doc.indexOf("x +") + 1; // mid-identifier
    const { prefix } = captureContext(doc, offset, 60, 20);
    expect(prefix.endsWith("x")).toBe(true);
  });

  it("handles CRLF documents without corrupting boundaries", () => {
    const crlf = doc.replace(/\n/g, "\r\n");
    const offset = crlf.indexOf("x + ") + 4;
    const { prefix } = captureContext(crlf, offset, 60, 20);
    expect(prefix.endsWith("let y = x + ")).toBe(true);
  });

  it("returns empty prefix at document start", () => {
    const { prefix } = captureContext(doc, 0, 60, 20);
    expect(prefix).toBe("");
  });
});
