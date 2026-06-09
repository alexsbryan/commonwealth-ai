// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, it, expect } from "vitest";
import {
  withRelay,
  relayLabel,
  formatBytes,
  formatTokens,
  formatGb,
  statusDot,
} from "./meshFormat";

describe("withRelay", () => {
  const link = "sovereign://join/abc";

  it("appends relay with `?` when the link has no query", () => {
    expect(withRelay(link, "1.2.3.4")).toBe(`${link}?relay=1.2.3.4`);
  });

  it("strips relay when toggled off (null)", () => {
    expect(withRelay(`${link}?relay=9.9.9.9`, null)).toBe(link);
  });

  it("is idempotent — replaces an existing relay rather than stacking", () => {
    expect(withRelay(`${link}?relay=old`, "new")).toBe(`${link}?relay=new`);
  });

  it("preserves unrelated params and appends with `&`", () => {
    expect(withRelay(`${link}?foo=bar`, "1.2.3.4")).toBe(
      `${link}?foo=bar&relay=1.2.3.4`,
    );
  });

  it("preserves unrelated params while replacing the relay", () => {
    expect(withRelay(`${link}?foo=bar&relay=old`, "new")).toBe(
      `${link}?foo=bar&relay=new`,
    );
  });

  it("drops only the relay param when toggled off, keeping the rest", () => {
    expect(withRelay(`${link}?foo=bar&relay=old`, null)).toBe(`${link}?foo=bar`);
  });

  it("does not percent-encode `:` in the relay value (the wire-format invariant)", () => {
    expect(withRelay(link, "100.64.0.2:9742")).toBe(
      `${link}?relay=100.64.0.2:9742`,
    );
  });
});

describe("relayLabel", () => {
  it("maps known kinds and echoes unknown ones", () => {
    expect(relayLabel("tailscale")).toBe("Tailscale (works across networks)");
    expect(relayLabel("lan")).toBe("Local network only");
    expect(relayLabel("ipv6")).toBe("IPv6 (sometimes routable)");
    expect(relayLabel("carrier-pigeon")).toBe("carrier-pigeon");
  });
});

describe("formatBytes", () => {
  it("guards non-finite / non-positive to 0 B", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(-5)).toBe("0 B");
    expect(formatBytes(NaN)).toBe("0 B");
  });

  it("rolls up 1024-based units with the <10 → 1dp rule", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(10 * 1024)).toBe("10 KB");
    expect(formatBytes(1024 * 1024)).toBe("1.0 MB");
  });
});

describe("formatTokens", () => {
  it("formats by magnitude (raw / k / M), guarding ≤0", () => {
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(1500)).toBe("1.5k");
    expect(formatTokens(1_500_000)).toBe("1.50M");
  });
});

describe("formatGb", () => {
  it("renders sub-1 GB as MB and guards ≤0", () => {
    expect(formatGb(0)).toBe("0 GB");
    expect(formatGb(0.5)).toBe("512 MB");
    expect(formatGb(2.5)).toBe("2.5 GB");
    expect(formatGb(16)).toBe("16 GB");
  });
});

describe("statusDot", () => {
  it("passes known presence states through and defaults to offline", () => {
    expect(statusDot("online")).toBe("online");
    expect(statusDot("busy")).toBe("busy");
    expect(statusDot("away")).toBe("away");
    expect(statusDot("zombie")).toBe("offline");
  });
});
