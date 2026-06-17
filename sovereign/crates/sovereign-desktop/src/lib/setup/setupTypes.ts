// SPDX-License-Identifier: AGPL-3.0-or-later
// Shared shape for the setup-progress view. Mirrors the `SetupProgress`
// payload that `src-tauri/src/setup_flow.rs` emits over the
// `setup-progress` event. Kept standalone so both the live SetupFlow
// container and the no-backend SetupScreen view (used by the dev screen
// gallery) speak the same type.

export type SetupPhase =
  | { kind: "detecting_hardware" }
  | { kind: "preparing_data_dir" }
  | { kind: "downloading_primary"; mb_total: number | null }
  | { kind: "downloading_fast" }
  | { kind: "downloading_embed" }
  | { kind: "opening_database" }
  | { kind: "loading_model" }
  | { kind: "smoke_testing" }
  | { kind: "ready" }
  | { kind: "failed"; recoverable: boolean };

export type Progress = {
  phase: SetupPhase;
  message: string;
  fraction: number | null;
  eta_seconds: number | null;
  indeterminate: boolean;
};

/// Provenance for one model slot — what `SetupScreen` needs to show where a
/// download comes from and lands. The `setup-progress` event carries only a
/// generic message, so `SetupFlow` fetches this (read-only) and the screen
/// joins it to the current phase to make the ledger legible.
export type SlotProvenance = {
  name: string;
  quant: string;
  size_gb: number;
  repo: string;
};

export type Provenance = {
  modelsDir: string;
  primary: SlotProvenance | null;
  fast: SlotProvenance | null;
  embed: SlotProvenance | null;
};
