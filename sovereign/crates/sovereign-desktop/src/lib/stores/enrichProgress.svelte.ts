// SPDX-License-Identifier: AGPL-3.0-or-later
// Runed singleton tracking `enrich build` runs. Unlike
// `corpusProgress.svelte.ts`, which subscribes to a single static
// Tauri event, each enrichment build publishes on its own
// `enrich://progress/{job_id}` channel — we attach a listener per
// job when the build starts and tear it down when the job
// terminates.
//
// Why per-job state (not per-corpus)? A single corpus can have
// multiple builds attempted back-to-back (first run, retry,
// re-build after a fix). Keying on `job_id` means the UI can show
// "still streaming job A" separately from "just started job B for
// the same corpus" without conflating them. When the UI wants an
// at-a-glance view, `byCorpus(corpusId)` filters to jobs for one
// corpus; `active` drops terminated jobs.
//
// Terminal events (`complete`, `aborted`) trigger a short
// observation window so a UI can flash the final state, then the
// job entry is pruned and its Tauri listener detached.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { produce } from "immer";
import type {
  EnrichBuildHandle,
  EnrichBuildStep,
  EnrichProgress,
} from "../types";

/// How long to keep a terminal job visible before pruning, in
/// milliseconds. Matches the `corpusProgress` store so
/// "just finished" transitions feel consistent across panels.
const TERMINAL_FLASH_MS = 800;

/// One chapter-level failure captured during Phase 1 of a build.
/// The aggregator CLI surfaces the same record grouped by kind;
/// this is the per-job slice used by the live progress UI before
/// a final aggregation.
export interface ChapterFailureEntry {
  chapter_id: string;
  failure_kind: string;
  reason: string;
}

/// Per-job state. Updated in place via `immer.produce()` so the
/// reactive `$state` picks up writes through a new top-level ref.
export interface EnrichJobState {
  job_id: string;
  corpus_id: string;
  channel: string;
  /// Populated by the first `build_start` event. Empty until then.
  pipeline_id: string;
  plannedSteps: EnrichBuildStep[];
  autoSkipped: EnrichBuildStep[];
  /// The step currently running, with its 1-based position in the
  /// enabled-steps sequence. `null` between steps or before the
  /// first `step_start`.
  currentStep:
    | { step: EnrichBuildStep; ordinal: number; total: number }
    | null;
  stepsCompleted: EnrichBuildStep[];
  /// Last per-chapter event from Phase 1 extract, for the live
  /// "1/19 sec_0001…" caption. Cleared when the extract step
  /// finishes.
  chapterProgress:
    | {
        chapter_id: string;
        index: number;
        total: number;
        question_count: number | null;
      }
    | null;
  /// Per-chapter failures surfaced by the parser during the
  /// extract step. Kept on the job so the progress panel can
  /// render them inline without re-reading the run file.
  chapterFailures: ChapterFailureEntry[];
  /// Terminal state:
  ///   - `complete`     — every enabled step passed
  ///   - `aborted`      — a step failed mid-build
  ///   - `spawn_failed` — the CLI subprocess couldn't even start
  ///   - `cancelled`    — user cancelled the build mid-flight
  terminal: "complete" | "aborted" | "spawn_failed" | "cancelled" | null;
  /// The step that stopped an aborted build, OR the step that was
  /// running when a cancellation fired. `null` otherwise.
  failedStep: EnrichBuildStep | null;
  exitCode: number | null;
  /// Human-readable reason for a `spawn_failed` terminal. `null`
  /// when the job is running or terminated for any other reason.
  spawnErrorMessage: string | null;
  startedAt: number;
  /// When the terminal event arrived (ms since epoch). The
  /// flash-and-prune timer uses this to decide when to remove the
  /// job from `_byJobId`. `null` while still running.
  terminatedAt: number | null;
}

let _byJobId: Record<string, EnrichJobState> = $state({});
const _unlistenByJobId: Record<string, UnlistenFn> = {};

function makeInitialState(handle: EnrichBuildHandle): EnrichJobState {
  return {
    job_id: handle.job_id,
    corpus_id: handle.corpus_id,
    channel: handle.channel,
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
    startedAt: Date.now(),
    terminatedAt: null,
  };
}

/// Apply one event to the job state. Split out of the listener
/// closure so it can be unit-tested in isolation — the store's
/// reducer logic is the part most likely to drift.
export function applyEvent(
  state: EnrichJobState,
  evt: EnrichProgress,
): EnrichJobState {
  return produce(state, (draft) => {
    switch (evt.kind) {
      case "build_start":
        draft.pipeline_id = evt.pipeline_id;
        draft.plannedSteps = evt.steps;
        draft.autoSkipped = evt.auto_skipped;
        break;
      case "step_start":
        draft.currentStep = {
          step: evt.step,
          ordinal: evt.ordinal,
          total: evt.total,
        };
        draft.chapterProgress = null;
        break;
      case "chapter_progress":
        draft.chapterProgress = {
          chapter_id: evt.chapter_id,
          index: evt.index,
          total: evt.total,
          question_count: evt.question_count,
        };
        break;
      case "chapter_failed":
        draft.chapterFailures.push({
          chapter_id: evt.chapter_id,
          failure_kind: evt.failure_kind,
          reason: evt.reason,
        });
        break;
      case "step_done":
        // Preserve ordering. The `stepsCompleted` list is the
        // source of truth for "how much of the planned sequence
        // has finished" — the UI derives a progress bar from it.
        if (!draft.stepsCompleted.includes(evt.step)) {
          draft.stepsCompleted.push(evt.step);
        }
        // Only clear `currentStep` if this event corresponds to
        // it — a late step_done for a previous step shouldn't
        // drop an already-running next step.
        if (draft.currentStep?.step === evt.step) {
          draft.currentStep = null;
        }
        break;
      case "step_failed":
        draft.failedStep = evt.step;
        draft.exitCode = evt.exit_code;
        break;
      case "complete":
        draft.terminal = "complete";
        draft.terminatedAt = Date.now();
        draft.currentStep = null;
        break;
      case "aborted":
        draft.terminal = "aborted";
        draft.terminatedAt = Date.now();
        draft.failedStep = evt.failed_step;
        draft.exitCode = evt.exit_code;
        draft.currentStep = null;
        break;
      case "spawn_failed":
        draft.terminal = "spawn_failed";
        draft.terminatedAt = Date.now();
        draft.spawnErrorMessage = evt.message;
        draft.currentStep = null;
        break;
      case "cancelled":
        draft.terminal = "cancelled";
        draft.terminatedAt = Date.now();
        // Reuse `failedStep` for the step that was running at
        // kill time. Semantically it's "the step that didn't
        // complete", which covers both aborted and cancelled.
        if (evt.at_step) draft.failedStep = evt.at_step;
        draft.currentStep = null;
        break;
    }
  });
}

function schedulePrune(
  jobId: string,
  terminal: "complete" | "aborted" | "spawn_failed" | "cancelled",
) {
  setTimeout(() => {
    // Only prune if the job is still in the same terminal state
    // we scheduled on — a re-track on the same job id (unlikely
    // but possible) shouldn't drop a fresh run.
    const current = _byJobId[jobId];
    if (!current || current.terminal !== terminal) return;
    _byJobId = produce(_byJobId, (draft) => {
      delete draft[jobId];
    });
    const unlisten = _unlistenByJobId[jobId];
    if (unlisten) {
      unlisten();
      delete _unlistenByJobId[jobId];
    }
  }, TERMINAL_FLASH_MS);
}

export const enrichProgressStore = {
  /** Reactive record: job_id → latest state. Readers in
   *  .svelte components rerender on any write. */
  get byJobId() {
    return _byJobId;
  },

  /** Non-terminal jobs currently in flight. Newest first so a
   *  single-entry "what's running" caption picks the right row. */
  get active(): EnrichJobState[] {
    return Object.values(_byJobId)
      .filter((j) => j.terminal === null)
      .sort((a, b) => b.startedAt - a.startedAt);
  },

  /** True iff any build is currently streaming. */
  get anyActive(): boolean {
    return this.active.length > 0;
  },

  /** Lookup by job_id. `undefined` when the job isn't tracked
   *  (never started, already pruned). */
  get(jobId: string): EnrichJobState | undefined {
    return _byJobId[jobId];
  },

  /** Every job for one corpus, newest first. Used by the
   *  EnrichmentPanel's per-corpus row. */
  byCorpus(corpusId: string): EnrichJobState[] {
    return Object.values(_byJobId)
      .filter((j) => j.corpus_id === corpusId)
      .sort((a, b) => b.startedAt - a.startedAt);
  },

  /** Attach a listener to a freshly-kicked-off build. Call this
   *  immediately after `enrichBuildAsync` returns — the Rust side
   *  starts emitting events as soon as the subprocess spawns, so
   *  a late subscription risks missing `build_start`.
   *
   *  Idempotent: if `handle.job_id` is already tracked, this is a
   *  no-op (the existing listener keeps the state current). */
  async track(handle: EnrichBuildHandle): Promise<void> {
    if (_byJobId[handle.job_id]) return;
    _byJobId = produce(_byJobId, (draft) => {
      draft[handle.job_id] = makeInitialState(handle);
    });
    const unlisten = await listen<EnrichProgress>(
      handle.channel,
      (event) => {
        const current = _byJobId[handle.job_id];
        if (!current) return; // pruned; ignore straggler events
        _byJobId = produce(_byJobId, (draft) => {
          draft[handle.job_id] = applyEvent(current, event.payload);
        });
        if (event.payload.kind === "complete") {
          schedulePrune(handle.job_id, "complete");
        } else if (event.payload.kind === "aborted") {
          schedulePrune(handle.job_id, "aborted");
        } else if (event.payload.kind === "spawn_failed") {
          schedulePrune(handle.job_id, "spawn_failed");
        } else if (event.payload.kind === "cancelled") {
          schedulePrune(handle.job_id, "cancelled");
        }
      },
    );
    _unlistenByJobId[handle.job_id] = unlisten;
  },

  /** Drop a job from the store and detach its listener
   *  immediately. Intended for explicit UI "dismiss" actions on
   *  terminal jobs, not as a substitute for the auto-prune. */
  dismiss(jobId: string): void {
    _byJobId = produce(_byJobId, (draft) => {
      delete draft[jobId];
    });
    const unlisten = _unlistenByJobId[jobId];
    if (unlisten) {
      unlisten();
      delete _unlistenByJobId[jobId];
    }
  },
};
