// SPDX-License-Identifier: AGPL-3.0-or-later
// Pure copy/format helpers for the peer-assisted-ingest UI. Kept out of the
// components so the copy logic (which must stay in lockstep with the backend
// reason tokens) is unit-tested in isolation.

import type { AssistIneligibleReason, AssistVerification } from "../../types";

/** Human copy for why a peer can't help. Mirrors the backend candidate-filter
 *  reason tokens so the picker never silently omits a peer. */
export function ineligibleReasonCopy(reason: AssistIneligibleReason): string {
  switch (reason) {
    case "offline":
      return "offline right now";
    case "no_embed_model":
      return "no matching embedding model";
    case "embed_model_mismatch":
      return "different embedding model — results wouldn't match";
    case "ok":
      return "";
    default:
      return "can't help with this one";
  }
}

/** The verification line shown on completion. */
export function verificationSummary(v: AssistVerification): string {
  if (v.sampled === 0) return "Nothing to re-check.";
  const mismatched = v.sampled - v.passed;
  if (mismatched === 0) {
    return `Re-checked ${v.sampled} chunks on this machine — all matched.`;
  }
  return `${mismatched} of ${v.sampled} chunks didn't match and were recomputed here.`;
}

/** True when every sampled chunk matched the local re-embed. */
export function verificationOk(v: AssistVerification): boolean {
  return v.sampled === v.passed;
}

/** "3 peers", "1 peer" — for the offer/confirm button. */
export function peerCountLabel(n: number): string {
  return `${n} ${n === 1 ? "peer" : "peers"}`;
}

/** Overall completion fraction 0..1 from unit tallies. */
export function assistFraction(complete: number, total: number): number {
  if (total <= 0) return 0;
  return Math.min(1, Math.max(0, complete / total));
}
