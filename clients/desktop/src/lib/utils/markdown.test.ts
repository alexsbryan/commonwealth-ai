// SPDX-License-Identifier: AGPL-3.0-or-later
// Citation-shape rendering tests. `renderMarkdown` is the single
// point that turns model output into clickable chips; both shapes
// must round-trip predictably so AssistantMessage's click handler
// has data to resolve against.
import { describe, it, expect } from "vitest";
import { renderMarkdown } from "./markdown";

describe("renderMarkdown — citations", () => {
  it("renders [Source: title] as a clickable chip with data-source", () => {
    const html = renderMarkdown("Hume argued X [Source: Joan Robinson].");
    expect(html).toContain('class="source-citation"');
    expect(html).toContain('data-source="Joan Robinson"');
    expect(html).toContain(">Joan Robinson<");
  });

  it("renders numeric [N] refs as fallback chips with data-citation-index", () => {
    const html = renderMarkdown("Compatibilism is well-defended [2].");
    expect(html).toContain('class="source-citation citation-numeric"');
    expect(html).toContain('data-citation-index="2"');
    expect(html).toContain(">[2]<");
  });

  it("renders multiple numeric refs independently", () => {
    const html = renderMarkdown("As in [1] and [3] but not [Source: X].");
    expect(html).toMatch(/data-citation-index="1"/);
    expect(html).toMatch(/data-citation-index="3"/);
    expect(html).toMatch(/data-source="X"/);
  });

  it("does NOT chip array indexing inside text-like prose (foo[1])", () => {
    // The lookbehind requires a non-alphanumeric char (or BOL)
    // before the `[`. `arr[1]` should pass through unmodified.
    const html = renderMarkdown("Access via `arr[1]` returns the head.");
    expect(html).not.toContain("data-citation-index");
  });

  it("does NOT chip 3+ digit brackets ([100])", () => {
    const html = renderMarkdown("Page [100] of the book.");
    expect(html).not.toContain("data-citation-index");
  });

  it("chips a numeric ref at the start of a line", () => {
    const html = renderMarkdown("[5] is the answer.");
    expect(html).toContain('data-citation-index="5"');
  });
});
