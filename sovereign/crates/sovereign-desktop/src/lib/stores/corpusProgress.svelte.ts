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
        _byId = produce(_byId, (draft) => {
          if (p.phase === "complete" || p.phase === "failed") {
            // Remove after a short delay so UI can show the terminal
            // state briefly. Done outside the produce() closure.
            draft[p.corpus_id] = p;
          } else {
            draft[p.corpus_id] = p;
          }
        });
        if (p.phase === "complete" || p.phase === "failed") {
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
          }, 800);
        }
      },
    );
  })();
  await _listenerStarting;
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
    return Object.values(_byId).filter(
      (p) => p.phase !== "complete" && p.phase !== "failed",
    );
  },

  /** True iff any install is currently in flight. */
  get anyInstalling() {
    return this.active.length > 0;
  },

  /** Lookup for a specific corpus. Returns `undefined` if no event
   *  has arrived for it yet or if it's already been pruned. */
  get(corpusId: string): CorpusProgressPayload | undefined {
    return _byId[corpusId];
  },

  /** Attach the listener on demand. Callers typically invoke this
   *  once per consumer mount — it's safe to call repeatedly. */
  async init(): Promise<void> {
    await ensureListening();
  },
};
