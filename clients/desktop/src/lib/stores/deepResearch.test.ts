// SPDX-License-Identifier: AGPL-3.0-or-later
// The pure deciders behind the deep-research live surface.
//
// Both of the things tested here are places where the old code answered a
// three-or-four-valued question with two values and got it wrong:
//
//   • `livenessOf` — "running / not running" cannot express the difference
//     between a round that is thinking (silent, healthy) and a backend that
//     has stopped talking (silent, wedged). A surface that redraws only on
//     change shows the same frozen panel for both.
//   • `runStateLabel` — the shelf defaulted an absent manifest to
//     "interrupted", so a run that was actively turning read as dead and was
//     offered a Resume button that would have re-entered its run dir.
import { describe, it, expect } from "vitest";
import {
  QUIET_NOTABLE_SECS,
  SIGNAL_STALE_SECS,
  formatElapsed,
  livenessOf,
  runStateLabel,
  type DrActiveState,
} from "./deepResearch.svelte";

const T0 = 1_700_000_000_000;

function activeState(over: Partial<DrActiveState> = {}): DrActiveState {
  return {
    jobId: "dr-100",
    channel: "deep-research://progress/dr-100",
    question: "q",
    runId: "dr-100",
    round: 1,
    maxRounds: 3,
    stage: "rounding",
    gaps: [],
    budget: { spent: {}, remaining: {} },
    consent: null,
    trail: [],
    elapsedSecs: 60,
    quietSecs: 2,
    lastBeatMs: T0,
    stopRequested: false,
    ...over,
  };
}

describe("livenessOf", () => {
  it("is null when nothing is running", () => {
    expect(livenessOf(null, T0)).toBeNull();
  });

  it("is `starting` until the first heartbeat lands", () => {
    // No beat yet means no honest claim about health either way.
    expect(livenessOf(activeState({ lastBeatMs: null }), T0)).toBe("starting");
  });

  it("is `working` while beats arrive and the run dir keeps moving", () => {
    expect(livenessOf(activeState({ quietSecs: 3 }), T0 + 1_000)).toBe("working");
  });

  it("is `quiet` when the run is beating but nothing has changed for a while", () => {
    // Healthy, and worth naming: this is the state a change-only view
    // renders as a frozen panel.
    const s = activeState({ quietSecs: QUIET_NOTABLE_SECS });
    expect(livenessOf(s, T0 + 1_000)).toBe("quiet");
    expect(livenessOf(activeState({ quietSecs: QUIET_NOTABLE_SECS - 1 }), T0 + 1_000)).toBe(
      "working",
    );
  });

  it("is `no-signal` once the beats stop, whatever the run last said", () => {
    // The distinguishing measurement is LOCAL — elapsed since the last beat
    // reached us — precisely because a backend that has gone away cannot
    // report that it has gone away.
    const s = activeState({ quietSecs: 1 });
    expect(livenessOf(s, T0 + (SIGNAL_STALE_SECS - 1) * 1000)).toBe("working");
    expect(livenessOf(s, T0 + SIGNAL_STALE_SECS * 1000)).toBe("no-signal");
  });

  it("reports lost signal even when the backend's own numbers looked healthy", () => {
    // quiet_secs = 0 means the run dir had just changed. It still counts as
    // no-signal, because the last thing we heard is old.
    const s = activeState({ quietSecs: 0 });
    expect(livenessOf(s, T0 + 60_000)).toBe("no-signal");
  });
});

describe("runStateLabel", () => {
  it("calls a live run running, whatever the manifest says", () => {
    expect(runStateLabel({ live: true, terminal_state: null })).toBe("running");
    // `live` outranks a stale manifest from a previous leg too.
    expect(runStateLabel({ live: true, terminal_state: "interrupted" })).toBe("running");
  });

  it("reports the manifest's own state for a run nobody is driving", () => {
    expect(runStateLabel({ live: false, terminal_state: "completed" })).toBe("completed");
    expect(runStateLabel({ live: false, terminal_state: "truncated" })).toBe("truncated");
  });

  it("calls an unmanifested run nobody is driving interrupted", () => {
    // This is the ONE case where "interrupted" is the truth rather than a
    // default: no manifest, and no process driving it.
    expect(runStateLabel({ live: false, terminal_state: null })).toBe("interrupted");
  });
});

describe("formatElapsed", () => {
  it("stays exact — this is a number people check against their own wait", () => {
    expect(formatElapsed(0)).toBe("0s");
    expect(formatElapsed(9)).toBe("9s");
    expect(formatElapsed(59)).toBe("59s");
    expect(formatElapsed(60)).toBe("1m 00s");
    expect(formatElapsed(254)).toBe("4m 14s");
    expect(formatElapsed(3599)).toBe("59m 59s");
    expect(formatElapsed(3600)).toBe("1h 00m");
    expect(formatElapsed(3660)).toBe("1h 01m");
  });

  it("never renders a negative clock", () => {
    expect(formatElapsed(-5)).toBe("0s");
  });
});
