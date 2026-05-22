// Pure ETA derivation for the Settings → Imports progress card.
//
// The daemon's `corpus-progress` stream already carries `phase` +
// `percent` per tick. There is no ETA field on the wire — but the
// Imports tab promises one ("we should keep users informed about how
// long we expect the full ingestion and enrichment to take"). This
// helper derives it client-side from the existing fields plus the
// wall-clock at start.
//
// Two phases of guidance:
//
//   1. **Pre-flight** — the Tauri command returns
//      `estimated_minutes` from a baked per-message benchmark
//      constant times the message count parsed from the export
//      header. That is presented as a `±30%` band before the user
//      starts the import.
//
//   2. **Live** — once progress is streaming, we replace the
//      pre-flight estimate with `elapsed_seconds * (100 - percent) /
//      percent` after a warmup window (60s OR percent ≥ 5%). The
//      band tightens as more progress lands.
//
// Pure function: no DOM, no fetch, no store. Easy to unit-test
// against synthetic payloads.

import type { CorpusProgressPayload } from "../types";

/** Minimum elapsed wall-clock before we trust the live derivative.
 *  60s matches the watched-folder enrichment doc-comment that calls
 *  out "~0.6s/chunk" as the dominant cost — under one minute of
 *  warmup the per-tick noise dominates. */
const WARMUP_MS = 60_000;

/** Below this percentage we don't have enough signal to extrapolate.
 *  Mirrored from `format_landscape`'s conservative posture — if the
 *  number isn't meaningful we'd rather show nothing than something
 *  wrong. */
const WARMUP_PERCENT = 5;

export interface EtaResult {
  /** Render-ready string for the progress card.
   *  Empty string means "hide the ETA chip." */
  label: string;
  /** Seconds remaining as the helper currently believes them.
   *  `null` during warmup or on terminal phases — callers should
   *  not render a chip. */
  secondsRemaining: number | null;
}

/** Terminal phases hide the ETA entirely — the card is about to
 *  switch to its "Open in Atlas" or "Retry" state. */
const TERMINAL_PHASES: ReadonlySet<string> = new Set(["complete", "failed"]);

/** Derive a live ETA from the latest progress tick.
 *
 *  - `payload`: most recent event from `corpusProgressStore`.
 *  - `startedAtMs`: `performance.now()`-style epoch the import
 *    kicked off.
 *  - `now`: callable so tests don't depend on the clock.
 */
export function deriveEta(
  payload: CorpusProgressPayload | undefined,
  startedAtMs: number,
  now: () => number = () => performance.now(),
): EtaResult {
  if (!payload) {
    return { label: "", secondsRemaining: null };
  }
  if (TERMINAL_PHASES.has(payload.phase)) {
    return { label: "", secondsRemaining: null };
  }
  const elapsedMs = Math.max(0, now() - startedAtMs);
  // Warmup gate — see comments at top of file.
  if (elapsedMs < WARMUP_MS && payload.percent < WARMUP_PERCENT) {
    return { label: "", secondsRemaining: null };
  }
  if (payload.percent <= 0 || payload.percent >= 100) {
    return { label: "", secondsRemaining: null };
  }
  const remainingFraction = (100 - payload.percent) / payload.percent;
  const remainingSeconds = Math.max(0, (elapsedMs / 1000) * remainingFraction);
  return {
    label: formatRemaining(remainingSeconds),
    secondsRemaining: remainingSeconds,
  };
}

/** Render seconds as "~12 min remaining" / "~45 sec remaining".
 *  Granularity follows the rule in the plan: minute resolution
 *  above 5 min, 10-second resolution below — finer numbers below
 *  one minute would read as fake precision. */
export function formatRemaining(seconds: number): string {
  if (seconds <= 0) return "";
  if (seconds < 60) {
    const rounded = Math.max(10, Math.round(seconds / 10) * 10);
    return `~${rounded} sec remaining`;
  }
  if (seconds < 5 * 60) {
    const mins = Math.max(1, Math.round(seconds / 60));
    return `~${mins} min remaining`;
  }
  const mins = Math.round(seconds / 60);
  return `~${mins} min remaining`;
}

/** Pre-flight band display string. Lives in the same module so the
 *  ImportsTab has a single import for both estimate surfaces. */
export function formatPreflightBand(estimatedMinutes: number): string {
  if (!Number.isFinite(estimatedMinutes) || estimatedMinutes <= 0) {
    return "";
  }
  // ±30% band per the plan. Show as a range — the half-minute floor
  // keeps short imports from rendering "0–1 min".
  const low = Math.max(0.5, estimatedMinutes * 0.7);
  const high = estimatedMinutes * 1.3;
  if (estimatedMinutes >= 5) {
    return `Estimated total time: ~${Math.round(low)}–${Math.round(high)} min`;
  }
  return `Estimated total time: ~${low.toFixed(1)}–${high.toFixed(1)} min`;
}

/** Refined total-time estimate derived from observed progress rate.
 *
 *  Once `deriveEta` has cleared its warmup gate, the live remaining
 *  is grounded in real chunks/sec rather than the baked
 *  `SECONDS_PER_MESSAGE` constant. The total is then just
 *  `elapsed + remaining` — a single number, no band, no guesses.
 *
 *  Returns `""` while still in warmup so the caller can fall back to
 *  `formatPreflightBand`. The two surfaces are mutually exclusive:
 *  during warmup the user sees the baked band; once live data
 *  arrives, the refined total takes over.
 */
export function formatRefinedTotal(
  payload: CorpusProgressPayload | undefined,
  startedAtMs: number,
  now: () => number = () => performance.now(),
): string {
  const live = deriveEta(payload, startedAtMs, now);
  if (live.secondsRemaining === null) {
    return "";
  }
  const elapsedSeconds = Math.max(0, (now() - startedAtMs) / 1000);
  const totalSeconds = elapsedSeconds + live.secondsRemaining;
  const totalMinutes = totalSeconds / 60;
  if (totalMinutes < 1) {
    return `Estimated total time: ~${Math.max(0.5, totalMinutes).toFixed(1)} min`;
  }
  return `Estimated total time: ~${Math.round(totalMinutes)} min`;
}
