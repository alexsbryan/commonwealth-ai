// SPDX-License-Identifier: AGPL-3.0-or-later
// Pure-reducer tests for the assistProgress store.
//
// The poll side effect (`meshAssistStatus` on an interval) isn't exercised
// here — we test `applyStatus`, `phaseLabel`, and `isTerminalPhase`, the parts
// most likely to drift. A missed field in the reducer would silently freeze
// the glassbox progress panel with no compiler signal; a bad phase parse would
// mislabel a still-running job as done (or never terminate it).

import { describe, it, expect } from "vitest";
import type { CollaborateStatus } from "../types";
import {
  applyStatus,
  phaseLabel,
  isTerminalPhase,
  type AssistJobState,
} from "./assistProgress.svelte";

function initialState(): AssistJobState {
  return {
    corpus_id: "vault",
    handoff_id: [1, 2, 3],
    phase: "Open",
    unitsTotal: 0,
    complete: 0,
    failed: 0,
    leased: 0,
    queued: 0,
    perPeer: [],
    grantExpiresAtMs: 5_000,
    verification: null,
    terminal: null,
    lastError: null,
    startedAt: 1000,
    terminatedAt: null,
  };
}

function snapshot(over: Partial<CollaborateStatus> = {}): CollaborateStatus {
  return {
    handoff_id: "h1",
    corpus_id: "vault",
    phase: "Open",
    total_units: 0,
    complete: 0,
    failed: 0,
    leased: 0,
    queued: 0,
    per_peer: [],
    ephemeral: true,
    grant: null,
    verification: null,
    ...over,
  };
}

describe("phaseLabel", () => {
  it("passes through a plain string phase", () => {
    expect(phaseLabel("Draining")).toBe("Draining");
  });
  it("takes the first key of a tagged-object phase", () => {
    expect(phaseLabel({ Failed: { reason: "boom" } })).toBe("Failed");
  });
  it("returns Unknown for null / unexpected shapes", () => {
    expect(phaseLabel(null)).toBe("Unknown");
    expect(phaseLabel(42)).toBe("Unknown");
    expect(phaseLabel({})).toBe("Unknown");
  });
});

describe("isTerminalPhase", () => {
  it("is true for Complete and Failed", () => {
    expect(isTerminalPhase("Complete")).toBe(true);
    expect(isTerminalPhase({ Failed: { reason: "x" } })).toBe(true);
  });
  it("is false for in-flight phases", () => {
    expect(isTerminalPhase("Open")).toBe(false);
    expect(isTerminalPhase("Draining")).toBe(false);
  });
});

describe("applyStatus", () => {
  it("projects counts + per-peer tallies from a live snapshot", () => {
    const next = applyStatus(
      initialState(),
      snapshot({
        phase: "Draining",
        total_units: 12,
        complete: 4,
        failed: 1,
        leased: 2,
        queued: 5,
        per_peer: [
          { node_id: "peerA", leased: 1, completed: 3, failed: 0 },
          { node_id: "peerB", leased: 1, completed: 1, failed: 1 },
        ],
      }),
    );
    expect(next.phase).toBe("Draining");
    expect(next.unitsTotal).toBe(12);
    expect(next.complete).toBe(4);
    expect(next.failed).toBe(1);
    expect(next.leased).toBe(2);
    expect(next.queued).toBe(5);
    expect(next.perPeer).toHaveLength(2);
    expect(next.perPeer[0].node_id).toBe("peerA");
    expect(next.terminal).toBeNull();
  });

  it("updates the grant expiry from the snapshot's grant window", () => {
    const next = applyStatus(
      initialState(),
      snapshot({
        grant: { expires_at_ms: 99_000, revoked: false, allowed_peers: ["a"] },
      }),
    );
    expect(next.grantExpiresAtMs).toBe(99_000);
  });

  it("terminates on a Complete phase", () => {
    const next = applyStatus(
      initialState(),
      snapshot({ phase: "Complete", total_units: 3, complete: 3 }),
    );
    expect(next.terminal).toBe("complete");
    expect(next.terminatedAt).not.toBeNull();
  });

  it("terminates on a Failed tagged phase", () => {
    const next = applyStatus(
      initialState(),
      snapshot({ phase: { Failed: { reason: "merge error" } } as unknown as string }),
    );
    expect(next.phase).toBe("Failed");
    expect(next.terminal).toBe("complete");
  });

  it("treats a null snapshot (queue gone) as terminal complete", () => {
    const next = applyStatus(initialState(), null);
    expect(next.terminal).toBe("complete");
    expect(next.terminatedAt).not.toBeNull();
  });

  it("does not resurrect a revoked terminal when the queue later disappears", () => {
    const revoked: AssistJobState = {
      ...initialState(),
      terminal: "revoked",
      terminatedAt: 2000,
    };
    const next = applyStatus(revoked, null);
    // Revoked stays revoked — a trailing null must not overwrite it to complete.
    expect(next.terminal).toBe("revoked");
    expect(next.terminatedAt).toBe(2000);
  });

  it("captures verification once and never unsets it on a later snapshot", () => {
    const withVerify = applyStatus(
      initialState(),
      snapshot({
        phase: "Complete",
        verification: { sampled: 24, passed: 24, min_cosine: 0.999, failures: [] },
      }),
    );
    expect(withVerify.verification?.sampled).toBe(24);
    // A subsequent snapshot with no verification must not wipe the report.
    const later = applyStatus(withVerify, snapshot({ verification: null }));
    expect(later.verification?.sampled).toBe(24);
  });

  it("only sets the first terminal transition (keeps original terminatedAt)", () => {
    const first = applyStatus(
      initialState(),
      snapshot({ phase: "Complete", complete: 3, total_units: 3 }),
    );
    const stamp = first.terminatedAt;
    const second = applyStatus(first, snapshot({ phase: "Complete" }));
    expect(second.terminal).toBe("complete");
    expect(second.terminatedAt).toBe(stamp);
  });
});
