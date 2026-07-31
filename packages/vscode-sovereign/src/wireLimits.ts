// The daemon's /v1/edit_predictions request contract, mirrored
// client-side (NEXT_EDIT.md §3). Pure and dependency-free so it can
// be exercised directly by the unit tests.
//
// The caps are the daemon's, in the daemon's units. That last part is
// load-bearing: the daemon measures unit fields in UTF-8 BYTES and
// rejects the WHOLE request with a 400 when one is over. Measuring in
// UTF-16 units on this side (`String.length`) would let a unit of 683
// CJK characters through a "2048" check that the daemon reads as 2049
// bytes — and because the offending unit stays in the history window,
// it would poison every following request until it aged out. Two
// different rulers on one contract is the bug; this module is the one
// ruler.

/** Largest document the route will search. */
export const MAX_TEXT_BYTES = 512 * 1024;
/** Longest edit-unit history the induction window accepts. */
export const MAX_HISTORY = 32;
/** Per-field cap on one history unit — BYTES, not characters. */
export const MAX_UNIT_BYTES = 2048;

/** UTF-8 length — the unit the daemon's caps are written in. */
export function bytes(s: string): number {
  return Buffer.byteLength(s, "utf8");
}

const isHigh = (c: number) => c >= 0xd800 && c <= 0xdbff;
const isLow = (c: number) => c >= 0xdc00 && c <= 0xdfff;

/**
 * `slice` that never splits a surrogate pair.
 *
 * Context is captured by fixed character offsets either side of an
 * edit, so a boundary can land inside an emoji. A lone surrogate
 * survives `JSON.stringify` as a `\udXXX` escape, which `serde_json`
 * rejects outright — so one emoji within the captured window would
 * otherwise 400 every request until the unit rolled out of history.
 * Boundaries move inward, never outward: context is an optimisation,
 * and half a character is worth less than a working lane.
 */
export function sliceWhole(s: string, start: number, end: number): string {
  let a = Math.max(0, Math.min(start, s.length));
  let b = Math.max(a, Math.min(end, s.length));
  if (a > 0 && isLow(s.charCodeAt(a)) && isHigh(s.charCodeAt(a - 1))) a += 1;
  if (b > 0 && b < s.length && isLow(s.charCodeAt(b)) && isHigh(s.charCodeAt(b - 1))) b -= 1;
  return s.slice(a, Math.max(a, b));
}

/** True when a captured unit is small enough for the daemon to accept. */
export function unitFitsWire(...fields: string[]): boolean {
  return fields.every((f) => bytes(f) <= MAX_UNIT_BYTES);
}
