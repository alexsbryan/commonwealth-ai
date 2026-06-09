// SPDX-License-Identifier: AGPL-3.0-or-later
// Pure-logic tests for the Models-tab memory-budget math (extracted
// from SettingsPanel). The reactive wiring is covered by e2e; this pins
// the device-memory tiering, the peak formula, and the thresholds.

import { describe, it, expect } from "vitest";
import type { HardwareInfo } from "../types";
import {
  effectiveMemoryBytes,
  memorySourceLabel,
  peakMemoryBytes,
  budgetStateFor,
  fmtGiB,
  RUNTIME_OVERHEAD,
  BASELINE_BYTES,
  type SlotSizes,
} from "./memoryBudget";

const GIB = 1024 ** 3;

function hw(over: Partial<HardwareInfo>): HardwareInfo {
  return {
    system_ram_gb: 16,
    gpu_available: false,
    gpu_name: null,
    gpu_memory_gb: null,
    is_unified_memory: false,
    ...over,
  };
}

describe("effectiveMemoryBytes / memorySourceLabel", () => {
  it("uses system RAM on unified memory (Apple Silicon), even with a GPU present", () => {
    const h = hw({
      is_unified_memory: true,
      system_ram_gb: 64,
      gpu_available: true,
      gpu_memory_gb: 8,
    });
    expect(effectiveMemoryBytes(h)).toBe(64 * GIB);
    expect(memorySourceLabel(h)).toBe("unified RAM");
  });

  it("uses GPU VRAM on a discrete GPU", () => {
    const h = hw({ gpu_available: true, gpu_memory_gb: 24, system_ram_gb: 64 });
    expect(effectiveMemoryBytes(h)).toBe(24 * GIB);
    expect(memorySourceLabel(h)).toBe("GPU VRAM");
  });

  it("falls back to system RAM with no usable GPU / unknown VRAM", () => {
    expect(effectiveMemoryBytes(hw({ system_ram_gb: 32 }))).toBe(32 * GIB);
    expect(memorySourceLabel(hw({}))).toBe("system RAM");
    // GPU present but VRAM unknown → system RAM
    const h = hw({ gpu_available: true, gpu_memory_gb: null, system_ram_gb: 32 });
    expect(effectiveMemoryBytes(h)).toBe(32 * GIB);
    expect(memorySourceLabel(h)).toBe("system RAM");
  });
});

describe("peakMemoryBytes", () => {
  it("adds always-loaded slots + the larger lazy slot, × overhead + baseline", () => {
    const slots: SlotSizes = {
      fast: 1 * GIB,
      embed: 1 * GIB,
      primary: 10 * GIB,
      code: 4 * GIB,
    };
    // fast+embed+max(primary,code) = 1+1+10 = 12 GiB
    expect(peakMemoryBytes(slots)).toBeCloseTo(
      12 * GIB * RUNTIME_OVERHEAD + BASELINE_BYTES,
      0,
    );
  });

  it("treats null slots as 0 and picks max(primary, code)", () => {
    const slots: SlotSizes = {
      fast: null,
      embed: null,
      primary: 2 * GIB,
      code: 5 * GIB,
    };
    expect(peakMemoryBytes(slots)).toBeCloseTo(
      5 * GIB * RUNTIME_OVERHEAD + BASELINE_BYTES,
      0,
    );
  });
});

describe("budgetStateFor", () => {
  it("thresholds at 0.80 (warn) and 0.95 (crit)", () => {
    expect(budgetStateFor(0.5)).toBe("ok");
    expect(budgetStateFor(0.79)).toBe("ok");
    expect(budgetStateFor(0.8)).toBe("warn");
    expect(budgetStateFor(0.94)).toBe("warn");
    expect(budgetStateFor(0.95)).toBe("crit");
    expect(budgetStateFor(1.5)).toBe("crit");
  });
});

describe("fmtGiB", () => {
  it("formats bytes as GiB with one decimal", () => {
    expect(fmtGiB(2 * GIB)).toBe("2.0 GiB");
    expect(fmtGiB(1.5 * GIB)).toBe("1.5 GiB");
  });
});
