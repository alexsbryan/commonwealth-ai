import { describe, it, expect } from "vitest";
import {
  formatRelativeAgo,
  catalogTier,
  formatDate,
  phaseLabel,
} from "./knowledgeStatusFormat";

describe("formatRelativeAgo", () => {
  const now = 1_000_000;

  it("buckets into s / m / h / d ago", () => {
    expect(formatRelativeAgo(now - 30, now)).toBe("30s ago");
    expect(formatRelativeAgo(now - 120, now)).toBe("2m ago");
    expect(formatRelativeAgo(now - 7200, now)).toBe("2h ago");
    expect(formatRelativeAgo(now - 2 * 86400, now)).toBe("2d ago");
  });

  it("clamps a future timestamp to 0s ago", () => {
    expect(formatRelativeAgo(now + 500, now)).toBe("0s ago");
  });
});

describe("catalogTier", () => {
  it("passes featured/hidden through and defaults everything else to preview", () => {
    expect(catalogTier("featured")).toBe("featured");
    expect(catalogTier("hidden")).toBe("hidden");
    expect(catalogTier("preview")).toBe("preview");
    expect(catalogTier(null)).toBe("preview");
    expect(catalogTier(undefined)).toBe("preview");
    expect(catalogTier("brand-new-recipe")).toBe("preview");
  });
});

describe("formatDate", () => {
  it("renders a numeric year (locale-robust)", () => {
    const ts = Math.floor(Date.UTC(2026, 5, 15, 12) / 1000);
    expect(formatDate(ts)).toMatch(/2026/);
  });
});

describe("phaseLabel", () => {
  it("maps known phases and echoes unknown ones", () => {
    expect(phaseLabel("downloading")).toBe("Downloading…");
    expect(phaseLabel("embedding")).toBe("Embedding…");
    expect(phaseLabel("complete")).toBe("Complete");
    expect(phaseLabel("failed")).toBe("Failed");
    expect(phaseLabel("some_future_phase")).toBe("some_future_phase");
  });
});
