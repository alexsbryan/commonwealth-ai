// Pure starter-question pool logic, extracted from ChatView.svelte
// (§3.3 component decomposition). These three functions are the
// logic-heavy, drift-prone parts of the suggestion-chip feature —
// round-robin interleave, the modular visible window, and the cursor
// advance with wrap-detection. Kept pure (no runes, no I/O) so they
// unit-test without mounting the component or Tauri. ChatView holds the
// reactive `$state`/`$derived` and delegates the computation here.

import type { StarterQuestion } from "../types";

/** A starter question tagged with the corpus it came from. */
export type StarterWithCorpus = StarterQuestion & { corpus_id: string };

/**
 * Round-robin interleave per-corpus starter lists into one pool, capped
 * at `target`, keeping cycle order corpus-fair (so a single enriched
 * corpus doesn't dominate the first pair). Stops as soon as `target` is
 * reached or every list is exhausted.
 */
export function interleaveStarters(
  perCorpus: StarterWithCorpus[][],
  target: number,
): StarterWithCorpus[] {
  const interleaved: StarterWithCorpus[] = [];
  let idx = 0;
  while (
    interleaved.length < target &&
    perCorpus.some((p) => p.length > idx)
  ) {
    for (const row of perCorpus) {
      if (idx < row.length && interleaved.length < target) {
        interleaved.push(row[idx]);
      }
    }
    idx += 1;
  }
  return interleaved;
}

/**
 * The `count`-item window of `pool` starting at `cursor`, wrapping
 * modulo the pool length. Empty when the pool is empty.
 */
export function visibleStarters(
  pool: StarterWithCorpus[],
  cursor: number,
  count: number,
): StarterWithCorpus[] {
  if (pool.length === 0) return [];
  const out: StarterWithCorpus[] = [];
  for (let i = 0; i < count; i++) {
    out.push(pool[(cursor + i) % pool.length]);
  }
  return out;
}

/**
 * Advance the cursor by `step`. If the next window would run past the
 * end of the pool, reset to 0 and signal a background refresh so the
 * next loop pulls fresh questions rather than recycling the same ones.
 */
export function advanceStarterCursor(
  cursor: number,
  poolLen: number,
  step: number,
): { cursor: number; shouldRefresh: boolean } {
  const next = cursor + step;
  if (next >= poolLen) {
    return { cursor: 0, shouldRefresh: true };
  }
  return { cursor: next, shouldRefresh: false };
}
