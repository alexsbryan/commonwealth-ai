// SPDX-License-Identifier: AGPL-3.0-or-later
// The live deep-research run, held OUTSIDE the view that renders it.
//
// Why this module exists. `DeepResearchView` used to hold the whole run in
// component `$state` and attach its own `listen()` in `onMount`. The view is
// mounted under `{#if view === "deep_research"}`, so pressing "Back to chat"
// unmounted it, tore the listener down and dropped every fact about the run —
// while the backend kept driving it. Driven with Playwright, that reproduced
// as: leave mid-run, come back, land on an EMPTY COMPOSER; and a report that
// finished while you were away fired into a dead listener and was never shown
// at all. A deep-research run is the longest-lived thing in this app, and it
// was the only one whose progress could not survive a click.
//
// So the run lives here, at module scope, for the lifetime of the app —
// the same shape `corpusProgress.svelte.ts` uses for long installs. The view
// became a renderer of this store; the store owns the subscription.
//
// The other half is LIVENESS. `Live` events are emitted only when the run dir
// changes, and a round spends minutes inside a single model call with nothing
// on disk moving — so silence is the normal case, and a surface that shows
// only change events cannot distinguish a healthy run from a wedged one. The
// backend now ticks a `heartbeat` every second carrying elapsed and quiet
// time; this store turns the pair into a NAMED verdict (see `liveness`)
// rather than leaving the user to infer one from a static panel.
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { drActiveRuns, drQuitAnyway } from "../api";
import type {
  DeepResearchRunProgress,
  DrBudget,
  DrConsent,
  DrGap,
  DrReport,
  DrRunSummary,
} from "../types";

/** One round the run has passed through, kept oldest-first so the live view
 *  can show the TRAIL rather than only the latest snapshot. "How is it
 *  working" is a history question; a single current-state panel cannot
 *  answer it. */
export interface DrTrailEntry {
  round: number;
  /** The gaps the gate named at this round — what the run went looking for. */
  gaps: DrGap[];
  /** Wall-clock seconds into the run when this round first appeared. */
  atSecs: number;
}

/** Everything known about the run in flight. Survives view unmount. */
export interface DrActiveState {
  jobId: string;
  channel: string;
  /** The operator's question, echoed back so any surface can name the run. */
  question: string;
  /** The verb's own run id, once it has named its run dir. */
  runId: string | null;
  round: number | null;
  maxRounds: number | null;
  stage: string;
  gaps: DrGap[];
  budget: DrBudget;
  consent: DrConsent | null;
  trail: DrTrailEntry[];
  /** Seconds since this leg started, as measured by the BACKEND. Not a
   *  local clock: a local clock keeps counting confidently after the
   *  backend has stopped talking, which is the exact lie this store
   *  exists to prevent. */
  elapsedSecs: number;
  /** Backend-measured seconds since anything in the run dir last changed. */
  quietSecs: number;
  /** `Date.now()` of the last heartbeat, for the signal-age check. */
  lastBeatMs: number | null;
  /** The user asked it to stop and the backend has not landed yet. */
  stopRequested: boolean;
}

/** A run that reached a terminal state. Held until the user acknowledges it,
 *  so work that finished while they were on another surface is never lost —
 *  it used to be, because the terminal event arrived at a listener that had
 *  already been torn down. */
export interface DrFinishedRun {
  runId: string;
  question: string;
  /** Present on success. */
  report: DrReport | null;
  /** Present on failure. Absence is reported, never defaulted. */
  error: string | null;
  /** Did the user ask for this stop? A truncated report they requested is
   *  a success, not a failure, and must not be reported as one. */
  stopRequested: boolean;
  /** Has this been surfaced to the user yet? */
  seen: boolean;
}

/** How long without a heartbeat before we stop claiming the run is healthy.
 *  The backend ticks at 1 s; 6 s is comfortably past normal scheduling
 *  jitter while still being noticed by a watching human. */
export const SIGNAL_STALE_SECS = 6;

/** Backend-measured quiet time past which the live view volunteers that a
 *  long silence is expected here. Below it, silence is unremarkable and
 *  saying anything would be noise. */
export const QUIET_NOTABLE_SECS = 45;

let _active: DrActiveState | null = $state(null);
let _finished: DrFinishedRun | null = $state(null);
/** Ticks once a second so `signalAgeSecs` re-derives. Only runs while a run
 *  is active — an idle app schedules nothing. */
let _nowMs = $state(Date.now());
let _ticker: ReturnType<typeof setInterval> | null = null;
let _unlisten: UnlistenFn | null = null;
let _recovering: Promise<void> | null = null;
/** The window refused to close because a run is in flight, and the operator
 *  has not yet said what to do about it. */
let _quitBlocked = $state(false);
let _quitUnlisten: UnlistenFn | null = null;
let _quitGuardStarting: Promise<void> | null = null;

/** Emitted by the window's `CloseRequested` handler when it declines to
 *  quit with research running. Mirrors `QUIT_BLOCKED_EVENT`. */
export const QUIT_BLOCKED_EVENT = "deep-research://quit-blocked";

function startTicker(): void {
  if (_ticker !== null) return;
  _nowMs = Date.now();
  _ticker = setInterval(() => {
    _nowMs = Date.now();
  }, 1000);
}

function stopTicker(): void {
  if (_ticker === null) return;
  clearInterval(_ticker);
  _ticker = null;
}

/** The four verdicts this surface can honestly reach. `starting` and
 *  `working` are healthy; `quiet` is healthy but worth naming; `no-signal`
 *  is the one that must never be dressed up as either. Two verdicts
 *  (running / not running) is what let a wedged run look like a busy one. */
export type DrLiveness = "starting" | "working" | "quiet" | "no-signal";

/** Seconds since the last heartbeat reached us, or `null` before the first
 *  one. This is a LOCAL measurement on purpose: it is the only quantity
 *  that can detect the backend having gone away, precisely because it does
 *  not come from the backend. */
function signalAgeSecs(a: DrActiveState | null, nowMs: number): number | null {
  if (!a || a.lastBeatMs === null) return null;
  return Math.max(0, Math.floor((nowMs - a.lastBeatMs) / 1000));
}

export function livenessOf(
  a: DrActiveState | null,
  nowMs: number,
): DrLiveness | null {
  if (!a) return null;
  const age = signalAgeSecs(a, nowMs);
  if (age === null) return "starting";
  if (age >= SIGNAL_STALE_SECS) return "no-signal";
  if (a.quietSecs >= QUIET_NOTABLE_SECS) return "quiet";
  return "working";
}

/** "4m 12s" / "48s" / "1h 06m". Elapsed is a fact the user checks against
 *  their own sense of how long they have been waiting, so it stays exact
 *  rather than rounding to "a few minutes". */
export function formatElapsed(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const rem = s % 60;
  if (m < 60) return `${m}m ${String(rem).padStart(2, "0")}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${String(m % 60).padStart(2, "0")}m`;
}

/** The shelf's state label. THE reason this is a function and not a field:
 *  a live run has no manifest yet, and defaulting that absence to
 *  "interrupted" is what made a run that was actively turning read as dead
 *  — with a Resume button beside it. `live` outranks the manifest; an
 *  absent manifest on a run nobody is driving is genuinely interrupted. */
export function runStateLabel(r: Pick<DrRunSummary, "live" | "terminal_state">): string {
  if (r.live) return "running";
  return r.terminal_state ?? "interrupted";
}

function apply(event: DeepResearchRunProgress): void {
  const a = _active;
  if (!a) return;
  switch (event.kind) {
    case "started":
      _active = { ...a, runId: event.run_id };
      break;
    case "live": {
      // The trail accumulates: a round that has been seen keeps its entry
      // and its arrival time, so leaving and returning does not rewrite
      // history to look like the run only just got here.
      let trail = a.trail;
      if (event.round !== null && !trail.some((t) => t.round === event.round)) {
        trail = [
          ...trail,
          { round: event.round, gaps: event.gaps, atSecs: a.elapsedSecs },
        ].sort((x, y) => x.round - y.round);
      } else if (event.round !== null) {
        trail = trail.map((t) =>
          t.round === event.round ? { ...t, gaps: event.gaps } : t,
        );
      }
      _active = {
        ...a,
        round: event.round,
        maxRounds: event.max_rounds,
        stage: event.stage,
        gaps: event.gaps,
        budget: event.budget,
        consent: event.consent,
        trail,
      };
      break;
    }
    case "heartbeat":
      _active = {
        ...a,
        elapsedSecs: event.elapsed_secs,
        quietSecs: event.quiet_secs,
        // A heartbeat carries the stage too, so a run that changes stage
        // without changing any artifact we diff on still reads correctly.
        stage: event.stage || a.stage,
        lastBeatMs: Date.now(),
      };
      _nowMs = Date.now();
      break;
    case "report_ready":
      _finished = {
        runId: event.report.run_id,
        question: a.question || event.report.question,
        report: event.report,
        error: null,
        stopRequested: a.stopRequested,
        seen: false,
      };
      _active = null;
      detach();
      break;
    case "failed":
      _finished = {
        runId: a.runId ?? a.jobId,
        question: a.question,
        report: null,
        error: event.error,
        stopRequested: a.stopRequested,
        seen: false,
      };
      _active = null;
      detach();
      break;
  }
}

function detach(): void {
  _unlisten?.();
  _unlisten = null;
  stopTicker();
}

async function subscribe(channel: string): Promise<void> {
  detach();
  _unlisten = await listen<DeepResearchRunProgress>(channel, (ev) => {
    apply(ev.payload);
  });
  startTicker();
}

export const deepResearchStore = {
  /** The run in flight, or `null`. Reactive. */
  get active(): DrActiveState | null {
    return _active;
  },

  /** A terminal run awaiting acknowledgement, or `null`. Reactive. */
  get finished(): DrFinishedRun | null {
    return _finished;
  },

  /** Is a run in flight? The single question every other surface asks. */
  get isRunning(): boolean {
    return _active !== null;
  },

  /** The named liveness verdict for the run in flight. `null` when idle. */
  get liveness(): DrLiveness | null {
    return livenessOf(_active, _nowMs);
  },

  /** Seconds since the last heartbeat, or `null` before the first. */
  get signalAgeSecs(): number | null {
    return signalAgeSecs(_active, _nowMs);
  },

  /** A finished run the user has not been shown yet. This is what makes a
   *  report that landed while they were elsewhere impossible to miss. */
  get unseenFinished(): DrFinishedRun | null {
    return _finished && !_finished.seen ? _finished : null;
  },

  /** Take ownership of a freshly started run and subscribe to its channel.
   *  From here the run belongs to the app, not to whichever view happened
   *  to start it. */
  async attach(
    handle: { job_id: string; channel: string },
    question: string,
  ): Promise<void> {
    _finished = null;
    _active = {
      jobId: handle.job_id,
      channel: handle.channel,
      question,
      runId: null,
      round: null,
      maxRounds: null,
      stage: "",
      gaps: [],
      budget: { spent: {}, remaining: {} },
      consent: null,
      trail: [],
      elapsedSecs: 0,
      quietSecs: 0,
      lastBeatMs: null,
      stopRequested: false,
    };
    await subscribe(handle.channel);
  },

  /** Re-adopt a run the backend is already driving. Called on app start and
   *  whenever the deep-research surface opens, so a run in flight is found
   *  rather than replaced by an empty composer. Idempotent, and a no-op
   *  while a run is already attached. */
  async recover(): Promise<void> {
    if (_active) return;
    if (_recovering) return _recovering;
    _recovering = (async () => {
      try {
        const runs = await drActiveRuns();
        const run = runs[0];
        if (!run || _active) return;
        _active = {
          jobId: run.run_id,
          channel: run.channel,
          question: run.question ?? "",
          runId: run.run_id,
          round: null,
          maxRounds: null,
          stage: "",
          gaps: [],
          budget: { spent: {}, remaining: {} },
          consent: null,
          trail: [],
          // The backend's next heartbeat (≤1 s away) supplies the real
          // elapsed. Seeding 0 would flash a wrong number; the view shows
          // "—" until a beat lands.
          elapsedSecs: 0,
          quietSecs: 0,
          lastBeatMs: null,
          stopRequested: false,
        };
        await subscribe(run.channel);
      } catch {
        // A backend that cannot answer leaves the store idle; the surface
        // says so on its own terms rather than inventing a run.
      } finally {
        _recovering = null;
      }
    })();
    return _recovering;
  },

  /** Record that the user asked the run to stop. The backend lands a
   *  truncated report with the truncation declared, so this is a normal
   *  ending, not a failure — the flag is what keeps the terminal surface
   *  from reporting it as one. */
  markStopRequested(): void {
    if (_active) _active = { ..._active, stopRequested: true };
  },

  /** Acknowledge the terminal run (the user has now seen it). */
  markFinishedSeen(): void {
    if (_finished) _finished = { ..._finished, seen: true };
  },

  /** Drop the terminal run entirely — the user has moved on. */
  clearFinished(): void {
    _finished = null;
  },

  /** Did a close attempt get refused because a run is in flight? */
  get quitBlocked(): boolean {
    return _quitBlocked;
  },

  /** Subscribe to the window's refusal-to-quit. Idempotent; safe to call
   *  from every consumer's mount. */
  async initQuitGuard(): Promise<void> {
    if (_quitUnlisten) return;
    if (_quitGuardStarting) return _quitGuardStarting;
    _quitGuardStarting = (async () => {
      _quitUnlisten = await listen(QUIT_BLOCKED_EVENT, () => {
        _quitBlocked = true;
      });
    })();
    await _quitGuardStarting;
    _quitGuardStarting = null;
  },

  /** The operator decided to stay. */
  dismissQuitBlock(): void {
    _quitBlocked = false;
  },

  /** The operator decided to go. The run dir keeps everything written so
   *  far, so the run returns as resumable rather than lost. */
  async quitAnyway(): Promise<void> {
    _quitBlocked = false;
    await drQuitAnyway();
  },

  /** Test seam: drop everything and unsubscribe. */
  reset(): void {
    _quitUnlisten?.();
    _quitUnlisten = null;
    _quitBlocked = false;
    detach();
    _active = null;
    _finished = null;
  },
};
