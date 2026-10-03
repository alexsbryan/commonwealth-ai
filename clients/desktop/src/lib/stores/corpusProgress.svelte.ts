// SPDX-License-Identifier: AGPL-3.0-or-later
// Runed singleton for `corpus-progress` Tauri events — keyed by
// `corpus_id`. The sovereign backend emits per-phase progress during
// long-running corpus installs (download → extract → chunk → embed →
// index → extract claims → …); every in-flight corpus has its own
// entry in `_byId`.
//
// Before this module, KnowledgeStatus.svelte and
// CorpusProgressBanner.svelte each attached their own `listen()` and
// maintained a near-identical `Record<corpus_id, payload>` $state.
// Double listener means double state — update race if the two were
// ever out of phase. Collapsing to a single subscriber eliminates the
// duplication and any possible drift.
//
// State updates go through `produce()`: every write yields a new
// top-level record reference so `$derived` selectors re-evaluate.
// Terminal phases (complete, failed) remove their entry after a short
// debounce so consumers can briefly observe the terminal state
// (useful for a "just finished" flash) before the row disappears.
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { produce } from "immer";
import type { CorpusProgressPayload } from "../types";

let _byId: Record<string, CorpusProgressPayload> = $state({});
let _unlisten: UnlistenFn | null = null;
let _listenerStarting: Promise<void> | null = null;

/// Corpora whose failure the user has explicitly dismissed.
///
/// Needed because a failure is now STICKY on the daemon side: the record
/// lives in `corpus_progress` until a retry sweeps it, so the desktop's
/// status poller re-emits the same `failed` payload every second. Without
/// this set, `dismiss()` would delete the entry and the very next poll
/// tick would put it straight back — a Dismiss button that visibly does
/// nothing.
///
/// Not reactive: it only gates writes into `_byId`, which is the reactive
/// surface. Dismissal is cleared as soon as a non-`failed` payload
/// arrives for that corpus (i.e. a retry actually started), so a
/// subsequent failure of a later attempt is shown again rather than
/// being swallowed by a stale acknowledgement.
const _dismissed = new Set<string>();

/** The two terminal phases the backend can report for an install.
 *  Everything else means "still working". Exported (and used by the
 *  components) so the phase strings live in one place instead of being
 *  re-spelled at every comparison site — a typo in one of those
 *  literals silently reclassifies a finished install as in-flight. */
export function isTerminalPhase(phase: string): boolean {
  return phase === "complete" || phase === "failed";
}

/** Does this terminal phase retire itself after a beat?
 *
 *  `complete` does: the row flashes its finished state, then gets out of
 *  the way. `failed` does NOT — it is the one terminal state the user
 *  has to act on, it carries the reason and (for an authorisation
 *  refusal) the remedy, and it typically lands minutes into an
 *  unattended download. An 800 ms flash of it is indistinguishable from
 *  never reporting the failure at all, which is the bug this store was
 *  part of. A failure is retired by an explicit `dismiss()` or by the
 *  next install attempt for the same corpus overwriting it. */
export function selfPrunes(phase: string): boolean {
  return phase === "complete";
}

/** How long a self-pruning terminal phase stays visible. */
export const TERMINAL_PRUNE_MS = 800;

/** Should this payload be stored, given what the user has dismissed?
 *
 *  Returns false for a re-delivery of an already-dismissed failure (the
 *  poller repeats it every tick, see `_dismissed`), and clears the
 *  dismissal as a side effect once the corpus reports anything else —
 *  which means a retry has started and a *new* failure is again worth
 *  showing. Exported for the unit tests; the listener is the only
 *  production caller. */
export function shouldStore(
  payload: Pick<CorpusProgressPayload, "corpus_id" | "phase">,
  dismissed: Set<string> = _dismissed,
): boolean {
  if (payload.phase === "failed") return !dismissed.has(payload.corpus_id);
  dismissed.delete(payload.corpus_id);
  return true;
}

/// Thin wrapper so the listener reads cleanly against the module set.
function applyDismissal(p: CorpusProgressPayload): boolean {
  return shouldStore(p, _dismissed);
}

/** Idempotent listener attach. Callers don't need to worry about
 *  double-subscription — this resolves to the same underlying
 *  listener across repeated invocations. */
async function ensureListening(): Promise<void> {
  if (_unlisten) return;
  if (_listenerStarting) return _listenerStarting;
  _listenerStarting = (async () => {
    _unlisten = await listen<CorpusProgressPayload>(
      "corpus-progress",
      (event) => {
        const p = event.payload;
        if (!applyDismissal(p)) return;
        _byId = produce(_byId, (draft) => {
          draft[p.corpus_id] = p;
        });
        // See `selfPrunes` for why a failure is exempt from this.
        if (selfPrunes(p.phase)) {
          setTimeout(() => {
            _byId = produce(_byId, (draft) => {
              // Only drop if still the same terminal phase — otherwise
              // a new install for the same corpus might have started
              // in the interim.
              if (
                draft[p.corpus_id] &&
                draft[p.corpus_id].phase === p.phase
              ) {
                delete draft[p.corpus_id];
              }
            });
          }, TERMINAL_PRUNE_MS);
        }
      },
    );
  })();
  await _listenerStarting;
}

/** Seconds remaining for the current phase, from backend throughput.
 *  `null` when we can't honestly estimate (no live rate, or the total
 *  isn't known yet) — callers render "—" / indeterminate rather than a
 *  fabricated number. Pure so it's unit-tested in isolation. */
export function etaSecondsFor(p: CorpusProgressPayload | undefined): number | null {
  if (!p) return null;
  const rate = p.chunks_per_sec ?? 0;
  const total = p.chunks_total ?? 0;
  if (rate <= 0 || total <= 0) return null;
  const remaining = total - p.chunks_processed;
  if (remaining <= 0) return 0;
  return remaining / rate;
}

/** Compact, honestly-approximate ETA string ("~4 min", "~45s", "~1.5 h").
 *  `null` seconds → "—". Always prefixed with "~": this is an estimate. */
export function formatEta(seconds: number | null): string {
  if (seconds === null) return "—";
  if (seconds <= 0) return "almost done";
  if (seconds < 90) return `~${Math.max(1, Math.round(seconds))}s`;
  const mins = seconds / 60;
  if (mins < 90) return `~${Math.round(mins)} min`;
  const hours = mins / 60;
  return `~${hours.toFixed(hours < 10 ? 1 : 0)} h`;
}

export const corpusProgressStore = {
  /** Reactive record: corpus_id → latest progress payload. Readers
   *  in .svelte components rerender on any write. */
  get byId() {
    return _byId;
  },

  /** Currently-active installs (phase not `complete` / `failed`).
   *  Useful for banner-style "something is in progress" UIs. */
  get active() {
    return Object.values(_byId).filter((p) => !isTerminalPhase(p.phase));
  },

  /** True iff any install is currently in flight. */
  get anyInstalling() {
    return this.active.length > 0;
  },

  /** Installs that ended in failure and have not been dismissed.
   *  Unlike `active`, these persist — a failed install is a standing
   *  condition the user still has to resolve, and its `message` carries
   *  the reason (and, for an authorisation refusal, the remedy). */
  get failures() {
    return Object.values(_byId).filter((p) => p.phase === "failed");
  },

  /** Lookup for a specific corpus. Returns `undefined` if no event
   *  has arrived for it yet or if it's already been pruned. */
  get(corpusId: string): CorpusProgressPayload | undefined {
    return _byId[corpusId];
  },

  /** Retire a terminal entry the user has acknowledged. Only meaningful
   *  for `failed` (the sole phase that doesn't self-prune); a no-op for
   *  a corpus with no entry.
   *
   *  Records the acknowledgement so the status poller's next repeat of
   *  the same failure doesn't immediately undo it (see `_dismissed`). */
  dismiss(corpusId: string): void {
    _dismissed.add(corpusId);
    _byId = produce(_byId, (draft) => {
      delete draft[corpusId];
    });
  },

  /** Attach the listener on demand. Callers typically invoke this
   *  once per consumer mount — it's safe to call repeatedly. */
  async init(): Promise<void> {
    await ensureListening();
  },
};
