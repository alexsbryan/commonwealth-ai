// SPDX-License-Identifier: AGPL-3.0-or-later
// Shared motion primitives (elegance phase, Move 3).
//
// One crossfade pair for the notebook card ↔ detail shared-element morph,
// plus a reduced-motion-aware duration helper. Matches the app's existing
// fly/cubicOut idiom (ReadingSurface, AtomPanel) and honours
// `prefers-reduced-motion` everywhere (every duration collapses to 0).

import { crossfade } from "svelte/transition";
import { cubicOut } from "svelte/easing";

/** Live `prefers-reduced-motion` preference. */
export function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/** `duration: reducedMotion ? 0 : ms` — for inline fly/fade configs. */
export const motionDur = (ms: number): number =>
  prefersReducedMotion() ? 0 : ms;

// The notebook card ↔ detail-header morph. Tag the shelf card AND the
// detail header with the same `key` (the notebook id) using both
// `in:cardReceive` and `out:cardSend`; on navigation the matched pair
// flies between positions while the rest of the shelf falls back to a
// quick fade.
export const [cardSend, cardReceive] = crossfade({
  duration: (d) =>
    prefersReducedMotion() ? 0 : Math.min(300, 80 + Math.sqrt(d) * 22),
  easing: cubicOut,
  fallback() {
    return {
      duration: prefersReducedMotion() ? 0 : 160,
      easing: cubicOut,
      css: (t) => `opacity: ${t}`,
    };
  },
});
