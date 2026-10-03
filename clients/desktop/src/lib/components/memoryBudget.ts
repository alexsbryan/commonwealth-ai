// SPDX-License-Identifier: AGPL-3.0-or-later
// Memory-budget math for the Settings → Models tab, extracted from
// SettingsPanel.svelte (§3.3 component decomposition). Pure functions
// (no runes, no IO) so the footgun-avoidance logic is unit-tested;
// SettingsPanel holds the reactive `$state`/`$derived` and feeds these
// the live slot sizes + detected hardware.

import type { HardwareInfo } from "../types";

const GIB = 1024 ** 3;

/** Runtime-overhead factor: KV cache + activation workspace + chat-
 *  template scratch are ~15% of the file size at 8192 ctx for typical
 *  Q4–Q6 GGUFs. */
export const RUNTIME_OVERHEAD = 1.15;

/** Baseline reserve for the OS + svrnmesh's own working set. */
export const BASELINE_BYTES = 2 * 1024 ** 3;

/** Per-slot GGUF file sizes in bytes; `null` when the slot is unset. */
export interface SlotSizes {
  fast: number | null;
  primary: number | null;
  embed: number | null;
  code: number | null;
}

export type BudgetState = "ok" | "warn" | "crit";

/** The device's effective memory for model loading: unified RAM on
 *  Apple Silicon, VRAM on a discrete GPU, otherwise system RAM. */
export function effectiveMemoryBytes(hw: HardwareInfo): number {
  const gb = hw.is_unified_memory
    ? hw.system_ram_gb
    : hw.gpu_available && hw.gpu_memory_gb != null
      ? hw.gpu_memory_gb
      : hw.system_ram_gb;
  return gb * GIB;
}

export function memorySourceLabel(hw: HardwareInfo): string {
  if (hw.is_unified_memory) return "unified RAM";
  if (hw.gpu_available && hw.gpu_memory_gb != null) return "GPU VRAM";
  return "system RAM";
}

/** Peak resident bytes. `fast` + `embed` are always loaded; `primary`
 *  and `code` share one lazy slot, so the peak adds `max(primary, code)`
 *  — then the runtime overhead, plus the OS baseline. Picking a large
 *  model in every slot is a footgun (daemon OOM at load / memory
 *  pressure mid-chat); the Models-tab budget meter is computed from this. */
export function peakMemoryBytes(slots: SlotSizes): number {
  const fast = slots.fast ?? 0;
  const embed = slots.embed ?? 0;
  const primary = slots.primary ?? 0;
  const code = slots.code ?? 0;
  const lazy = Math.max(primary, code);
  return (fast + embed + lazy) * RUNTIME_OVERHEAD + BASELINE_BYTES;
}

/** Budget-meter state from the peak/effective ratio: `warn` at ≥80%,
 *  `crit` at ≥95%. */
export function budgetStateFor(ratio: number): BudgetState {
  return ratio >= 0.95 ? "crit" : ratio >= 0.8 ? "warn" : "ok";
}

export function fmtGiB(bytes: number): string {
  return `${(bytes / GIB).toFixed(1)} GiB`;
}
