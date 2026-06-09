import { describe, it, expect } from "vitest";
import { modelTier, formatSize } from "./modelDisplay";

describe("modelTier", () => {
  it("classifies by minimum RAM (GB) at the 10/20 boundaries", () => {
    expect(modelTier(8)).toBe("basic");
    expect(modelTier(10)).toBe("basic");
    expect(modelTier(11)).toBe("standard");
    expect(modelTier(20)).toBe("standard");
    expect(modelTier(21)).toBe("premium");
    expect(modelTier(64)).toBe("premium");
  });
});

describe("formatSize", () => {
  it("formats by magnitude (GB 1dp / MB / KB, decimal units)", () => {
    expect(formatSize(2_000_000_000)).toBe("2.0 GB");
    expect(formatSize(1_500_000_000)).toBe("1.5 GB");
    expect(formatSize(5_000_000)).toBe("5 MB");
    expect(formatSize(3_000)).toBe("3 KB");
  });
});
