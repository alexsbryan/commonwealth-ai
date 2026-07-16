// SPDX-License-Identifier: AGPL-3.0-or-later
// Pure-function tests for the peer-assist copy/format helpers. These strings
// are the glassbox contract shown to the user (why a peer can't help, what the
// local re-check found); a drift here silently mis-describes the mesh's
// behaviour, so each branch is pinned.

import { describe, it, expect } from "vitest";
import type { AssistVerification } from "../../types";
import {
  ineligibleReasonCopy,
  verificationSummary,
  verificationOk,
  peerCountLabel,
  assistFraction,
} from "./assistFormat";

describe("ineligibleReasonCopy", () => {
  it("maps each backend reason token to human copy", () => {
    expect(ineligibleReasonCopy("offline")).toBe("offline right now");
    expect(ineligibleReasonCopy("no_embed_model")).toBe(
      "no matching embedding model",
    );
    expect(ineligibleReasonCopy("embed_model_mismatch")).toBe(
      "different embedding model — results wouldn't match",
    );
  });

  it("returns empty copy for the eligible ('ok') token", () => {
    expect(ineligibleReasonCopy("ok")).toBe("");
  });

  it("falls back to a generic reason for an unknown token", () => {
    // Cast through unknown: a future backend token must never crash the picker.
    expect(
      ineligibleReasonCopy("some_new_reason" as unknown as "offline"),
    ).toBe("can't help with this one");
  });
});

describe("verificationSummary", () => {
  it("reports an all-matched re-check", () => {
    const v: AssistVerification = {
      sampled: 24,
      passed: 24,
      min_cosine: 0.9999,
      failures: [],
    };
    expect(verificationSummary(v)).toBe(
      "Re-checked 24 chunks on this machine — all matched.",
    );
  });

  it("reports how many chunks were recomputed on a mismatch", () => {
    const v: AssistVerification = {
      sampled: 24,
      passed: 22,
      min_cosine: 0.4,
      failures: [
        [3, 0.4],
        [7, 0.6],
      ],
    };
    expect(verificationSummary(v)).toBe(
      "2 of 24 chunks didn't match and were recomputed here.",
    );
  });

  it("handles the empty-sample case", () => {
    const v: AssistVerification = {
      sampled: 0,
      passed: 0,
      min_cosine: 1,
      failures: [],
    };
    expect(verificationSummary(v)).toBe("Nothing to re-check.");
  });
});

describe("verificationOk", () => {
  it("is true only when every sampled chunk passed", () => {
    expect(
      verificationOk({ sampled: 10, passed: 10, min_cosine: 1, failures: [] }),
    ).toBe(true);
    expect(
      verificationOk({
        sampled: 10,
        passed: 9,
        min_cosine: 0.5,
        failures: [[1, 0.5]],
      }),
    ).toBe(false);
  });

  it("treats a zero-sample report as ok (nothing failed)", () => {
    expect(
      verificationOk({ sampled: 0, passed: 0, min_cosine: 1, failures: [] }),
    ).toBe(true);
  });
});

describe("peerCountLabel", () => {
  it("singularizes one peer", () => {
    expect(peerCountLabel(1)).toBe("1 peer");
  });
  it("pluralizes for zero and many", () => {
    expect(peerCountLabel(0)).toBe("0 peers");
    expect(peerCountLabel(3)).toBe("3 peers");
  });
});

describe("assistFraction", () => {
  it("computes complete/total", () => {
    expect(assistFraction(5, 10)).toBe(0.5);
  });
  it("guards divide-by-zero", () => {
    expect(assistFraction(0, 0)).toBe(0);
  });
  it("clamps to [0,1] against bad inputs", () => {
    expect(assistFraction(15, 10)).toBe(1);
    expect(assistFraction(-3, 10)).toBe(0);
  });
});
