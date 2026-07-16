// SPDX-License-Identifier: AGPL-3.0-or-later
// Poll-based store tracking a peer-assisted ingest ("Blanket").
//
// Unlike `enrichProgress.svelte.ts` (event-driven over a Tauri channel), the
// daemon exposes assist progress via a `POST /collaborate/status` poll, so
// this store polls `meshAssistStatus` on an interval and reduces each snapshot
// into per-corpus state. Keyed by `corpus_id` — one ephemeral grant per corpus
// at a time, so `corpus_id` is the natural job key (and what `revoke` uses).
//
// The pure `applyStatus` reducer is split out so the snapshot→state logic (the
// part most likely to drift) is unit-tested in isolation, exactly as
// `enrichProgress.applyEvent` is.

import { produce } from "immer";
import { meshAssistRevoke, meshAssistStatus } from "../api";
import type {
  AssistPeerProgress,
  AssistVerification,
  CollaborateStatus,
} from "../types";

const POLL_INTERVAL_MS = 1500;
const TERMINAL_FLASH_MS = 1200;

export interface AssistJobState {
  corpus_id: string;
  /** Opaque handoff id round-tripped to `meshAssistStatus`. */
  handoff_id: unknown;
  phase: string;
  unitsTotal: number;
  complete: number;
  failed: number;
  leased: number;
  queued: number;
  perPeer: AssistPeerProgress[];
  grantExpiresAtMs: number | null;
  verification: AssistVerification | null;
  /// Terminal state:
  ///   - `complete` — the merge finished (phase Complete / queue gone)
  ///   - `revoked`  — the user stopped peer help; local ingest continues
  ///   - `error`    — a fatal status error
  terminal: "complete" | "revoked" | "error" | null;
  lastError: string | null;
  startedAt: number;
  terminatedAt: number | null;
}

export interface AssistHandle {
  corpus_id: string;
  handoff_id: unknown;
  grant_expires_at_ms?: number;
}

let _byCorpus: Record<string, AssistJobState> = $state({});
const _pollers: Record<string, ReturnType<typeof setInterval>> = {};

function makeInitial(handle: AssistHandle): AssistJobState {
  return {
    corpus_id: handle.corpus_id,
    handoff_id: handle.handoff_id,
    phase: "Open",
    unitsTotal: 0,
    complete: 0,
    failed: 0,
    leased: 0,
    queued: 0,
    perPeer: [],
    grantExpiresAtMs: handle.grant_expires_at_ms ?? null,
    verification: null,
    terminal: null,
    lastError: null,
    startedAt: Date.now(),
    terminatedAt: null,
  };
}

/** `HandoffPhase` serializes as a string ("Open"/"Draining"/…) or a tagged
 *  object for the data-carrying `Failed` variant. Normalize to a short label. */
export function phaseLabel(phase: unknown): string {
  if (typeof phase === "string") return phase;
  if (phase && typeof phase === "object") {
    return Object.keys(phase as Record<string, unknown>)[0] ?? "Unknown";
  }
  return "Unknown";
}

export function isTerminalPhase(phase: unknown): boolean {
  const label = phaseLabel(phase);
  return label === "Complete" || label === "Failed";
}

/** Reduce one status snapshot (or `null` = queue gone) into job state.
 *  Pure — the testable seam. */
export function applyStatus(
  state: AssistJobState,
  status: CollaborateStatus | null,
): AssistJobState {
  return produce(state, (draft) => {
    if (status === null) {
      // Queue retired / torn down — done from the UI's point of view.
      if (draft.terminal === null) {
        draft.terminal = "complete";
        draft.terminatedAt = Date.now();
      }
      return;
    }
    draft.phase = phaseLabel(status.phase);
    draft.unitsTotal = status.total_units;
    draft.complete = status.complete;
    draft.failed = status.failed;
    draft.leased = status.leased;
    draft.queued = status.queued;
    draft.perPeer = status.per_peer;
    if (status.grant) draft.grantExpiresAtMs = status.grant.expires_at_ms;
    // Verification only arrives once (post-merge); never unset it.
    if (status.verification) draft.verification = status.verification;
    if (isTerminalPhase(status.phase) && draft.terminal === null) {
      draft.terminal = "complete";
      draft.terminatedAt = Date.now();
    }
  });
}

function stopPolling(corpusId: string): void {
  const p = _pollers[corpusId];
  if (p) {
    clearInterval(p);
    delete _pollers[corpusId];
  }
}

function schedulePrune(corpusId: string): void {
  setTimeout(() => {
    const cur = _byCorpus[corpusId];
    if (!cur || cur.terminal === null) return;
    _byCorpus = produce(_byCorpus, (d) => {
      delete d[corpusId];
    });
  }, TERMINAL_FLASH_MS);
}

async function pollOnce(handle: AssistHandle): Promise<void> {
  const cur = _byCorpus[handle.corpus_id];
  if (!cur || cur.terminal !== null) return;
  let status: CollaborateStatus | null;
  try {
    status = await meshAssistStatus(handle.handoff_id);
  } catch (e) {
    _byCorpus = produce(_byCorpus, (d) => {
      const s = d[handle.corpus_id];
      if (s) s.lastError = String(e);
    });
    return;
  }
  const latest = _byCorpus[handle.corpus_id];
  if (!latest) return;
  _byCorpus = produce(_byCorpus, (d) => {
    d[handle.corpus_id] = applyStatus(latest, status);
  });
  if (_byCorpus[handle.corpus_id]?.terminal !== null) {
    stopPolling(handle.corpus_id);
    schedulePrune(handle.corpus_id);
  }
}

export const assistProgressStore = {
  /** Reactive record: corpus_id → latest assist state. */
  get byCorpus() {
    return _byCorpus;
  },

  /** Non-terminal assists in flight, newest first. */
  get active(): AssistJobState[] {
    return Object.values(_byCorpus)
      .filter((j) => j.terminal === null)
      .sort((a, b) => b.startedAt - a.startedAt);
  },

  get anyActive(): boolean {
    return this.active.length > 0;
  },

  /** Lookup by corpus_id. `undefined` when not tracked / already pruned. */
  get(corpusId: string): AssistJobState | undefined {
    return _byCorpus[corpusId];
  },

  /** Begin tracking a freshly-started assist: seed state and start the poll
   *  loop. Idempotent per corpus. Call right after `meshAssistStart`. */
  track(handle: AssistHandle): void {
    if (_pollers[handle.corpus_id]) return;
    _byCorpus = produce(_byCorpus, (d) => {
      d[handle.corpus_id] = makeInitial(handle);
    });
    _pollers[handle.corpus_id] = setInterval(
      () => void pollOnce(handle),
      POLL_INTERVAL_MS,
    );
    void pollOnce(handle);
  },

  /** Stop peer help for a corpus mid-run. Revokes the grant server-side (the
   *  local ingest continues) and flips the UI to a `revoked` terminal. */
  async revoke(corpusId: string): Promise<void> {
    try {
      await meshAssistRevoke(corpusId);
    } catch {
      // Best-effort — the TTL sweep / self-evict still tears the job down.
    }
    stopPolling(corpusId);
    _byCorpus = produce(_byCorpus, (d) => {
      const s = d[corpusId];
      if (s && s.terminal === null) {
        s.terminal = "revoked";
        s.terminatedAt = Date.now();
      }
    });
    schedulePrune(corpusId);
  },

  /** Drop an assist from the store immediately (explicit dismiss). */
  dismiss(corpusId: string): void {
    stopPolling(corpusId);
    _byCorpus = produce(_byCorpus, (d) => {
      delete d[corpusId];
    });
  },
};
