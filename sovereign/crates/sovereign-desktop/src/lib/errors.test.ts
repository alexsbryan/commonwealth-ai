import { describe, it, expect } from "vitest";
import { isDesktopError, normalizeError } from "./errors";

describe("isDesktopError", () => {
  it("accepts the exact wire shape", () => {
    expect(
      isDesktopError({ code: "not_ready", message: "m", suggested_action: "" }),
    ).toBe(true);
  });

  it("rejects partial, wrong-typed, unknown-code, and non-object values", () => {
    expect(isDesktopError({ code: "not_ready", message: "m" })).toBe(false);
    expect(
      isDesktopError({ code: "bogus", message: "m", suggested_action: "" }),
    ).toBe(false);
    expect(isDesktopError({ code: 7, message: "m", suggested_action: "" })).toBe(
      false,
    );
    expect(isDesktopError("a bare string")).toBe(false);
    expect(isDesktopError(null)).toBe(false);
    expect(isDesktopError(new Error("x"))).toBe(false);
  });
});

describe("normalizeError", () => {
  it("passes a structured DesktopError through unchanged", () => {
    const e = {
      code: "upstream" as const,
      message: "peer down",
      suggested_action: "retry",
    };
    expect(normalizeError(e)).toEqual(e);
  });

  it("coerces a string rejection (unmigrated command) to internal", () => {
    expect(normalizeError("Backend is still loading.")).toEqual({
      code: "internal",
      message: "Backend is still loading.",
      suggested_action: "",
    });
  });

  it("coerces a JS Error to internal with its message preserved", () => {
    expect(normalizeError(new Error("boom"))).toEqual({
      code: "internal",
      message: "boom",
      suggested_action: "",
    });
  });
});
