// SPDX-License-Identifier: AGPL-3.0-or-later
// AssistProgressPanel template tests. Pure view over one AssistJobState. The
// contract: a running job shows the overall bar + per-peer tallies + a "Stop
// using peers" button; a terminal job swaps to the completion/verification and
// "reverted to local-only" confirmations and drops the stop button.

import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import AssistProgressPanel from "./AssistProgressPanel.svelte";
import type { AssistJobState } from "../../stores/assistProgress.svelte";

function job(over: Partial<AssistJobState> = {}): AssistJobState {
  return {
    corpus_id: "vault",
    handoff_id: "h1",
    phase: "Draining",
    unitsTotal: 10,
    complete: 4,
    failed: 0,
    leased: 2,
    queued: 4,
    perPeer: [{ node_id: "aaaa1111bbbb", leased: 2, completed: 4, failed: 0 }],
    grantExpiresAtMs: null,
    verification: null,
    terminal: null,
    lastError: null,
    startedAt: 0,
    terminatedAt: null,
    ...over,
  };
}

describe("AssistProgressPanel", () => {
  it("renders running header, units, and per-peer tallies", () => {
    render(AssistProgressPanel, { props: { job: job(), onRevoke: vi.fn() } });
    expect(screen.getByText(/1 peer helping/i)).toBeInTheDocument();
    expect(screen.getByText(/4\/10 units/)).toBeInTheDocument();
    // Per-peer row shows the truncated node id + tallies. The tally string
    // is split across text nodes by the `{#if}` interpolations, so assert on
    // the row's combined textContent rather than a single text node.
    expect(screen.getByText("aaaa1111")).toBeInTheDocument();
    const row = screen.getByText("aaaa1111").closest("li");
    expect(row?.textContent).toMatch(/4 done/);
    expect(row?.textContent).toMatch(/2 in flight/);
  });

  it("shows the Stop button only while running and forwards the corpus id", async () => {
    const onRevoke = vi.fn();
    render(AssistProgressPanel, { props: { job: job(), onRevoke } });
    const stop = screen.getByRole("button", { name: /stop using peers/i });
    await fireEvent.click(stop);
    expect(onRevoke).toHaveBeenCalledWith("vault");
  });

  it("renders the merging note during the Merging phase", () => {
    render(AssistProgressPanel, {
      props: { job: job({ phase: "Merging" }), onRevoke: vi.fn() },
    });
    expect(screen.getByText(/merging shards on this machine/i)).toBeInTheDocument();
  });

  it("on complete: shows completion header, verification, revert line, no Stop", () => {
    render(AssistProgressPanel, {
      props: {
        job: job({
          terminal: "complete",
          phase: "Complete",
          complete: 10,
          verification: {
            sampled: 24,
            passed: 24,
            min_cosine: 0.999,
            failures: [],
          },
        }),
        onRevoke: vi.fn(),
      },
    });
    expect(screen.getByText(/mesh help complete/i)).toBeInTheDocument();
    expect(
      screen.getByText(/re-checked 24 chunks on this machine — all matched/i),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/reverted to local-only\. nothing retained by peers/i),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /stop using peers/i }),
    ).not.toBeInTheDocument();
  });

  it("on a verification mismatch: renders the recomputed-here summary", () => {
    render(AssistProgressPanel, {
      props: {
        job: job({
          terminal: "complete",
          phase: "Complete",
          verification: {
            sampled: 24,
            passed: 22,
            min_cosine: 0.3,
            failures: [
              [1, 0.3],
              [2, 0.4],
            ],
          },
        }),
        onRevoke: vi.fn(),
      },
    });
    expect(
      screen.getByText(/2 of 24 chunks didn't match and were recomputed here/i),
    ).toBeInTheDocument();
  });

  it("on revoke: shows the stopped header and revert confirmation", () => {
    render(AssistProgressPanel, {
      props: { job: job({ terminal: "revoked" }), onRevoke: vi.fn() },
    });
    expect(screen.getByText(/stopped peer help/i)).toBeInTheDocument();
    expect(
      screen.getByText(/reverted to local-only/i),
    ).toBeInTheDocument();
  });

  it("surfaces a progress-check error when present", () => {
    render(AssistProgressPanel, {
      props: {
        job: job({ lastError: "daemon unreachable" }),
        onRevoke: vi.fn(),
      },
    });
    expect(
      screen.getByText(/progress check failed: daemon unreachable/i),
    ).toBeInTheDocument();
  });
});
