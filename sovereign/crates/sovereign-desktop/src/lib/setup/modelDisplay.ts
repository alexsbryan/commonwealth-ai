// SPDX-License-Identifier: AGPL-3.0-or-later
// Pure display helpers for ModelSelector (§3.3 component decomposition):
// the hardware-tier classifier and the byte formatter. No runes, no IO —
// unit-tested; the component imports them.

export type ModelTier = "basic" | "standard" | "premium";

/** Hardware tier from a model's minimum RAM in GB: ≤10 → basic,
 *  ≤20 → standard, otherwise premium. */
export function modelTier(minRamGb: number): ModelTier {
  if (minRamGb <= 10) return "basic";
  if (minRamGb <= 20) return "standard";
  return "premium";
}

/** Human-readable file size: GB (1 decimal) at ≥ 1e9 bytes, MB at
 *  ≥ 1e6, otherwise KB. Uses decimal (1000-based) units to match the
 *  download-progress wire numbers. */
export function formatSize(bytes: number): string {
  if (bytes >= 1_000_000_000) {
    return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
  }
  if (bytes >= 1_000_000) {
    return `${(bytes / 1_000_000).toFixed(0)} MB`;
  }
  return `${(bytes / 1_000).toFixed(0)} KB`;
}
