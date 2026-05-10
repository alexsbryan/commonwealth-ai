// Pure-reducer tests for the enrichProgress store.
//
// The Tauri `listen` side effect isn't exercised here — we test
// `applyEvent`, which is the part most likely to drift (a missed
// variant in the switch statement would silently skip state
// updates and break the progress UI without any compiler signal).
// The listener-attach path is integration-tested when the whole
// app mounts under Tauri.

import { describe, it, expect } from "vitest";
import type { EnrichProgress } from "../types";
import { applyEvent, type EnrichJobState } from "./enrichProgress.svelte";

function initialState(): EnrichJobState {
  return {
    job_id: "j1",
    corpus_id: "bk",
    channel: "enrich://progress/j1",
    pipeline_id: "",
    plannedSteps: [],
    autoSkipped: [],
    currentStep: null,
    stepsCompleted: [],
    chapterProgress: null,
    chapterFailures: [],
    terminal: null,
    failedStep: null,
    exitCode: null,
    spawnErrorMessage: null,
    startedAt: 1000,
    terminatedAt: null,
  };
}

describe("enrichProgress applyEvent", () => {
  it("build_start populates pipeline id + planned steps", () => {
    const evt: EnrichProgress = {
      kind: "build_start",
      corpus_id: "bk",
      pipeline_id: "literary_atlas",
      steps: ["seed", "extract", "cluster"],
      auto_skipped: ["configure"],
    };
    const next = applyEvent(initialState(), evt);
    expect(next.pipeline_id).toBe("literary_atlas");
    expect(next.plannedSteps).toEqual(["seed", "extract", "cluster"]);
    expect(next.autoSkipped).toEqual(["configure"]);
  });

  it("step_start sets currentStep and clears chapterProgress", () => {
    const base = applyEvent(initialState(), {
      kind: "chapter_progress",
      corpus_id: "bk",
      chapter_id: "sec_0001",
      index: 1,
      total: 3,
      question_count: 2,
    });
    expect(base.chapterProgress?.chapter_id).toBe("sec_0001");
    const next = applyEvent(base, {
      kind: "step_start",
      corpus_id: "bk",
      step: "cluster",
      ordinal: 3,
      total: 9,
    });
    expect(next.currentStep).toEqual({ step: "cluster", ordinal: 3, total: 9 });
    // Chapter progress belongs to extract; entering cluster clears
    // it so a late chapter line doesn't linger on the cluster step.
    expect(next.chapterProgress).toBeNull();
  });

  it("chapter_progress updates only the chapter slot, not currentStep", () => {
    const base = applyEvent(initialState(), {
      kind: "step_start",
      corpus_id: "bk",
      step: "extract",
      ordinal: 2,
      total: 9,
    });
    const next = applyEvent(base, {
      kind: "chapter_progress",
      corpus_id: "bk",
      chapter_id: "sec_0007",
      index: 4,
      total: 19,
      question_count: 5,
    });
    expect(next.chapterProgress).toEqual({
      chapter_id: "sec_0007",
      index: 4,
      total: 19,
      question_count: 5,
    });
    expect(next.currentStep).toEqual({ step: "extract", ordinal: 2, total: 9 });
  });

  it("chapter_failed appends without dropping earlier failures", () => {
    const s1 = applyEvent(initialState(), {
      kind: "chapter_failed",
      corpus_id: "bk",
      chapter_id: "sec_0001",
      failure_kind: "parse_drift",
      reason: "EOF mid-json",
    });
    const s2 = applyEvent(s1, {
      kind: "chapter_failed",
      corpus_id: "bk",
      chapter_id: "sec_0005",
      failure_kind: "think_truncated",
      reason: "<think> unclosed",
    });
    expect(s2.chapterFailures).toHaveLength(2);
    expect(s2.chapterFailures[0].chapter_id).toBe("sec_0001");
    expect(s2.chapterFailures[1].failure_kind).toBe("think_truncated");
  });

  it("step_done appends to stepsCompleted and clears matching currentStep", () => {
    const base = applyEvent(initialState(), {
      kind: "step_start",
      corpus_id: "bk",
      step: "extract",
      ordinal: 2,
      total: 9,
    });
    const next = applyEvent(base, {
      kind: "step_done",
      corpus_id: "bk",
      step: "extract",
      summary: "done",
    });
    expect(next.stepsCompleted).toEqual(["extract"]);
    expect(next.currentStep).toBeNull();
  });

  it("late step_done for a prior step doesn't drop an active next step", () => {
    // The CLI prints step_done AFTER the next step_start would be
    // impossible given the synchronous step loop, but the store
    // contract should still be robust to reordering.
    const s1 = applyEvent(initialState(), {
      kind: "step_start",
      corpus_id: "bk",
      step: "extract",
      ordinal: 2,
      total: 9,
    });
    const s2 = applyEvent(s1, {
      kind: "step_start",
      corpus_id: "bk",
      step: "cluster",
      ordinal: 3,
      total: 9,
    });
    const s3 = applyEvent(s2, {
      kind: "step_done",
      corpus_id: "bk",
      step: "extract",
      summary: "done",
    });
    // cluster should still be current — extract's step_done arrived
    // out of order.
    expect(s3.currentStep?.step).toBe("cluster");
    expect(s3.stepsCompleted).toContain("extract");
  });

  it("complete sets terminal and clears currentStep", () => {
    const base = applyEvent(initialState(), {
      kind: "step_start",
      corpus_id: "bk",
      step: "report",
      ordinal: 9,
      total: 9,
    });
    const next = applyEvent(base, {
      kind: "complete",
      corpus_id: "bk",
      steps_completed: 9,
    });
    expect(next.terminal).toBe("complete");
    expect(next.currentStep).toBeNull();
    expect(next.terminatedAt).not.toBeNull();
  });

  it("aborted sets terminal + failedStep + exitCode", () => {
    const base = applyEvent(initialState(), {
      kind: "step_failed",
      corpus_id: "bk",
      step: "extract",
      message: "parse error",
      exit_code: 1,
    });
    expect(base.failedStep).toBe("extract");
    const next = applyEvent(base, {
      kind: "aborted",
      corpus_id: "bk",
      failed_step: "extract",
      exit_code: 1,
    });
    expect(next.terminal).toBe("aborted");
    expect(next.failedStep).toBe("extract");
    expect(next.exitCode).toBe(1);
  });

  it("cancelled sets its own terminal state and records the interrupted step", () => {
    // Distinct from spawn_failed: a cancellation happens
    // mid-build after at least one step has started. The store
    // carries the interrupted step so the UI can render
    // "Cancelled mid-extract" rather than just "Cancelled".
    const running = applyEvent(initialState(), {
      kind: "step_start",
      corpus_id: "bk",
      step: "extract",
      ordinal: 2,
      total: 9,
    });
    const next = applyEvent(running, {
      kind: "cancelled",
      corpus_id: "bk",
      at_step: "extract",
    });
    expect(next.terminal).toBe("cancelled");
    expect(next.failedStep).toBe("extract");
    expect(next.currentStep).toBeNull();
    expect(next.terminatedAt).not.toBeNull();
  });

  it("cancelled with null at_step is safe (early cancel)", () => {
    // When a cancel fires before any step started, `at_step` is
    // null. The reducer must not stomp existing failedStep (if
    // set by an earlier event) with null — here we assert the
    // clean-cancel path doesn't crash and terminal lands.
    const next = applyEvent(initialState(), {
      kind: "cancelled",
      corpus_id: "bk",
      at_step: null,
    });
    expect(next.terminal).toBe("cancelled");
    expect(next.failedStep).toBeNull();
  });

  it("spawn_failed sets its own terminal state without pretending a step ran", () => {
    // When the CLI binary can't even spawn, the store must NOT
    // attribute the failure to Seed (the old behaviour). The UI's
    // "failed at Seed" label would misdirect the operator into
    // debugging prompts that never ran.
    const evt: EnrichProgress = {
      kind: "spawn_failed",
      corpus_id: "bk",
      message: "could not spawn sovereign-cli: No such file or directory",
    };
    const next = applyEvent(initialState(), evt);
    expect(next.terminal).toBe("spawn_failed");
    expect(next.failedStep).toBeNull();
    expect(next.spawnErrorMessage).toContain("No such file");
    expect(next.terminatedAt).not.toBeNull();
  });

  it("stepsCompleted ignores duplicate step_done events", () => {
    // Defensive — the CLI shouldn't emit duplicates but the store
    // shouldn't render a double bump on the progress bar if it ever
    // did.
    const s1 = applyEvent(initialState(), {
      kind: "step_done",
      corpus_id: "bk",
      step: "seed",
      summary: "",
    });
    const s2 = applyEvent(s1, {
      kind: "step_done",
      corpus_id: "bk",
      step: "seed",
      summary: "",
    });
    expect(s2.stepsCompleted).toEqual(["seed"]);
  });
});
