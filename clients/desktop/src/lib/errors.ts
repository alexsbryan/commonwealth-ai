// SPDX-License-Identifier: AGPL-3.0-or-later
// Frontend half of the structured-error contract (§2D-3). Pure — no
// Tauri import — so the guard + normaliser are unit-testable without
// mocking `invoke`. `invokeChecked` (in api.ts) and `toastError` (in
// stores/toast) both route through `normalizeError`.

import type { DesktopError, ErrorCode } from "./types";

const ERROR_CODES: readonly ErrorCode[] = [
  "not_ready",
  "invalid_request",
  "upstream",
  "internal",
];

/// Structural guard: does this rejected value carry the Rust
/// `DesktopError` wire shape (`{ code, message, suggested_action }`)?
/// Tauri rejects with the serialized `Err`, so a migrated command lands
/// here as a plain object — never an `Error` instance.
export function isDesktopError(e: unknown): e is DesktopError {
  if (typeof e !== "object" || e === null) return false;
  const o = e as Record<string, unknown>;
  return (
    typeof o.code === "string" &&
    (ERROR_CODES as readonly string[]).includes(o.code) &&
    typeof o.message === "string" &&
    typeof o.suggested_action === "string"
  );
}

/// Normalise any thrown/rejected value to a `DesktopError`. Structured
/// errors pass through unchanged; a legacy `String` rejection (an
/// unmigrated command) or a JS `Error` becomes `internal` with the text
/// preserved — so callers can treat every failure uniformly while the
/// per-handler migration is still in flight.
export function normalizeError(e: unknown): DesktopError {
  if (isDesktopError(e)) return e;
  const message =
    e instanceof Error ? e.message : typeof e === "string" ? e : String(e);
  return { code: "internal", message, suggested_action: "" };
}
