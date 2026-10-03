import { describe, expect, it } from "vitest";
import { SseParser } from "../src/sse";

describe("SseParser", () => {
  it("parses a complete single-chunk stream", () => {
    const p = new SseParser();
    const events = p.feed('data: {"a":1}\n\ndata: [DONE]\n\n');
    expect(events).toEqual([{ data: '{"a":1}' }, { data: "[DONE]" }]);
  });

  it("reassembles lines split across chunks", () => {
    const p = new SseParser();
    expect(p.feed('data: {"a":')).toEqual([]);
    expect(p.feed("1}\n")).toEqual([]);
    expect(p.feed("\n")).toEqual([{ data: '{"a":1}' }]);
  });

  it("joins multi-line data continuations", () => {
    const p = new SseParser();
    const events = p.feed("data: line one\ndata: line two\n\n");
    expect(events).toEqual([{ data: "line one\nline two" }]);
  });

  it("drops comment keep-alives", () => {
    const p = new SseParser();
    const events = p.feed(": keep-alive\n\ndata: x\n\n");
    expect(events).toEqual([{ data: "x" }]);
  });

  it("tolerates CRLF", () => {
    const p = new SseParser();
    const events = p.feed("data: x\r\n\r\n");
    expect(events).toEqual([{ data: "x" }]);
  });

  it("flushes a trailing unterminated event on end()", () => {
    const p = new SseParser();
    p.feed("data: trailing");
    expect(p.end()).toEqual([{ data: "trailing" }]);
  });

  it("handles the FIM terminal sequence", () => {
    const p = new SseParser();
    const wire =
      'data: {"choices":[{"text":"x = 1"}]}\n\n' +
      'data: {"choices":[],"sovereign_debug":{"stop_rule":"newline"}}\n\n' +
      'data: {"choices":[{"text":"","finish_reason":"stop"}]}\n\n' +
      "data: [DONE]\n\n";
    const events = p.feed(wire);
    expect(events).toHaveLength(4);
    expect(events[1].data).toContain("sovereign_debug");
    expect(events[3].data).toBe("[DONE]");
  });
});
