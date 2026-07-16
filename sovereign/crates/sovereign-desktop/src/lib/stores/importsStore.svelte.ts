// SPDX-License-Identifier: AGPL-3.0-or-later
// Persistent state for Settings → Imports.
//
// Originally ImportsTab.svelte held this state in component-local
// `$state`. Result: switching off the Imports tab unmounted the
// component, dropped `stage` / `startResponse` / `startedAtMs`, and
// the user came back to a blank "Idle" panel even though the daemon
// was actively ingesting their export in the background. That was
// the user-visible "the button resets" bug.
//
// This module owns the state for the lifetime of the desktop
// process, listens to `corpus-progress` globally, and exposes a
// small store surface the tab reads. Post-ingest enrichment is
// observed by polling `enrichmentStatus` (in-process), not by a
// subprocess event channel.
//
// **Multi-source.** The state machine is identical across chat
// vendors, so it's a `createImportsStore(cfg)` factory keyed on a
// corpus id + a localStorage key. One instance per source
// (`anthropicImportsStore`, `chatgptImportsStore`) runs independently
// — each filters the shared `corpus-progress` channel to its own
// corpus id, so importing Claude and ChatGPT can even progress side by
// side. `importsStore` stays exported as the Anthropic instance for
// back-compat with existing callers/specs.
//
// Two-stage flow each instance models:
//   1. **Ingest** — kicked off by the source's import command
//      (`import_anthropic_zip` / `import_chatgpt_zip`). Progress
//      arrives on the shared `corpus-progress` event channel. The
//      chat recipes declare `[enrichment] type="tiered"`, so the
//      heavy T2/T3 enrichment runs IN-PROCESS inside `engine.ingest`
//      — the corpus stays "ingesting" until it finishes.
//   2. **Enrich** — after ingest reports `phase = "complete"`, the
//      daemon's detached post-install hook builds the structural
//      atlas (`atoms.json`) in-process and stamps
//      `_enrichment_state.json`. The store observes that by polling
//      `enrichmentStatus(corpusId)` to `complete` — no subprocess,
//      no CLI (`sovereign-cli` isn't bundled with the desktop, so
//      the old `enrich_build_async` hop exited 127 in shipped
//      builds). Sources whose recipe ships `[enrichment]` OFF
//      (`autoEnrich: false`, e.g. email-archive) skip this stage and
//      treat ingest completion as terminal.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  enrichmentStatus,
  getCorpusProgress,
  listCorpora,
  type EnrichmentStatus,
  type ImportStartResponse,
} from "../api";
import type { CorpusProgressPayload } from "../types";

export type ImportStage =
  | "idle"
  | "starting"
  | "needs_reset_confirm"
  | "ingesting"
  | "enriching"
  | "complete"
  | "failed";

/** Snapshot of the response shape when the daemon reports an
 *  existing partial index. Surfaced so the ImportsTab can render
 *  the destructive-confirm banner with the index path the user is
 *  about to wipe. */
export interface PendingReset {
  zipPath: string;
  indexPath: string;
  totalMessages: number;
  estimatedMinutes: number;
  canonicalPath: string;
}

export interface ImportState {
  stage: ImportStage;
  /** True when this source's corpus is already installed on disk from
   *  a prior import. Independent of `stage`: a user with a finished
   *  import who hasn't done anything since the desktop launched sits at
   *  `stage: "idle"` + `alreadyInstalled: true`. Hydrated once on
   *  `init()` via `listCorpora()` so the picker can render a "Re-import"
   *  affordance instead of "Import …" next to a corpus the daemon has
   *  already chewed through. */
  alreadyInstalled: boolean;
  /** Pre-flight info from the import command. `null` until the Tauri
   *  command resolves. */
  startResponse: ImportStartResponse | null;
  /** Populated when the command returned `partial_index_exists`.
   *  Tracks the zip the user picked so the confirmation click can
   *  re-invoke with the same path + `resetPartial: true`. */
  pendingReset: PendingReset | null;
  /** `performance.now()`-style epoch the user clicked Import. */
  startedAtMs: number | null;
  errorMessage: string | null;
  /** Latest ingest-side progress event, or `null` before any
   *  arrives. Mirrored separately from `corpusProgressStore.byId`
   *  so terminal-phase pruning there doesn't clobber the
   *  Imports-tab UI. */
  ingestProgress: CorpusProgressPayload | null;
  /** Latest polled enrichment status for this corpus, or `null`
   *  before the enriching stage begins. Drives the card's
   *  second-half progress bar + phase caption. Read from the
   *  in-process `_enrichment_state.json` via `enrichmentStatus`. */
  enrichStatus: EnrichmentStatus | null;
}

const INITIAL: ImportState = {
  stage: "idle",
  alreadyInstalled: false,
  startResponse: null,
  pendingReset: null,
  startedAtMs: null,
  errorMessage: null,
  ingestProgress: null,
  enrichStatus: null,
};

/** Per-source bindings for a {@link createImportsStore} instance. */
export interface ImportsStoreConfig {
  /** Corpus id == recipe id the source installs (the discriminator on
   *  the shared `corpus-progress` channel). */
  corpusId: string;
  /** localStorage key for the most recent pre-flight response. Lets a
   *  desktop restart restore the message-count + estimate display.
   *  Must be unique per source so two imports don't collide. */
  localStorageKey: string;
  /** Poll `enrichmentStatus` for the in-process structural-atlas hop
   *  after ingest completes (the chat imports' conversation-atlas
   *  step). Default `true`. The email-archive recipe ships
   *  `[enrichment]` OFF, so its store treats ingest completion as
   *  terminal instead. */
  autoEnrich?: boolean;
}

/** The reactive surface a consumer (ImportsTab / ConversationImportCard)
 *  reads. Returned by {@link createImportsStore}. */
export type ImportsStore = ReturnType<typeof createImportsStore>;

/** Build an independent import state machine for one chat source. */
export function createImportsStore(cfg: ImportsStoreConfig) {
  let _state: ImportState = $state({ ...INITIAL });
  let _corpusUnlisten: UnlistenFn | null = null;
  let _enrichPollHandle: ReturnType<typeof setInterval> | null = null;
  // Consecutive polls where `_enrichment_state.json` had no state yet.
  // The detached structural-atlas hook stamps state shortly after
  // ingest commits; if nothing ever stamps (corpus produces no atlas),
  // we don't hang the card — see `pollEnrichmentOnce`.
  let _enrichNullPolls = 0;
  let _initStarted: Promise<void> | null = null;

  const ENRICH_POLL_INTERVAL_MS = 2000;
  // ~16s of null enrichment state → assume nothing is pending and
  // treat the import as complete (ingest itself already finished).
  const ENRICH_NULL_POLL_LIMIT = 8;

  function applyCorpusProgress(p: CorpusProgressPayload): void {
    if (p.corpus_id !== cfg.corpusId) return;
    _state = { ..._state, ingestProgress: p };
    if (_state.stage === "starting" || _state.stage === "ingesting") {
      if (p.phase === "complete") {
        if (cfg.autoEnrich === false) {
          // No enrichment hop for this source (email-archive ships
          // `[enrichment]` off) — ingest complete IS complete.
          _state = { ..._state, stage: "complete", alreadyInstalled: true };
        } else {
          // Ingest done — the daemon's in-process post-install hook
          // now builds the structural atlas (`atoms.json`) so the
          // Atlas-View "Conversations" row appears. Observe it by
          // polling `enrichmentStatus` to `complete` (no subprocess).
          startEnrichPoll();
        }
      } else if (p.phase === "failed") {
        _state = {
          ..._state,
          stage: "failed",
          errorMessage: p.message ?? "Ingest failed",
        };
      } else {
        _state = { ..._state, stage: "ingesting" };
      }
    }
  }

  /** Enter the enriching stage and start polling the in-process
   *  enrichment status. Idempotent-ish: a re-import within the same
   *  session stops the prior poll first. */
  function startEnrichPoll(): void {
    stopEnrichPoll();
    _enrichNullPolls = 0;
    _state = { ..._state, stage: "enriching", enrichStatus: null };
    _enrichPollHandle = setInterval(
      () => void pollEnrichmentOnce(),
      ENRICH_POLL_INTERVAL_MS,
    );
    void pollEnrichmentOnce();
  }

  async function pollEnrichmentOnce(): Promise<void> {
    let st: EnrichmentStatus;
    try {
      st = await enrichmentStatus(cfg.corpusId);
    } catch {
      return; // transient daemon hiccup — keep polling
    }
    _state = { ..._state, enrichStatus: st };
    const phase = st.state?.phase;
    if (phase === "complete") {
      _state = { ..._state, stage: "complete", alreadyInstalled: true };
      stopEnrichPoll();
      return;
    }
    if (phase === "failed" || st.is_stalled) {
      _state = {
        ..._state,
        stage: "failed",
        errorMessage:
          st.state?.error ?? "Enrichment stalled — no progress. Try again.",
      };
      stopEnrichPoll();
      return;
    }
    if (st.state === null) {
      // The detached structural-atlas hook hasn't stamped state yet —
      // or this corpus produces no atlas. Wait a bounded window; if
      // nothing ever stamps, ingest is already done, so complete
      // rather than hang on a phantom build.
      _enrichNullPolls += 1;
      if (_enrichNullPolls >= ENRICH_NULL_POLL_LIMIT) {
        _state = { ..._state, stage: "complete", alreadyInstalled: true };
        stopEnrichPoll();
      }
    } else {
      _enrichNullPolls = 0;
    }
  }

  function stopEnrichPoll(): void {
    if (_enrichPollHandle) {
      clearInterval(_enrichPollHandle);
      _enrichPollHandle = null;
    }
  }

  async function ensureCorpusListener(): Promise<void> {
    if (_corpusUnlisten) return;
    _corpusUnlisten = await listen<CorpusProgressPayload>(
      "corpus-progress",
      (event) => applyCorpusProgress(event.payload),
    );
  }

  /** Restore the most-recent pre-flight response from localStorage.
   *  Returns `null` when absent or unparseable so the caller can
   *  decide whether to display a stub or wait for the live event. */
  function restoreStartResponse(): ImportStartResponse | null {
    try {
      const raw = localStorage.getItem(cfg.localStorageKey);
      if (!raw) return null;
      return JSON.parse(raw) as ImportStartResponse;
    } catch {
      return null;
    }
  }

  function persistStartResponse(resp: ImportStartResponse): void {
    try {
      localStorage.setItem(cfg.localStorageKey, JSON.stringify(resp));
    } catch {
      // localStorage quota / disabled — best-effort persistence;
      // import still works in-session.
    }
  }

  function clearPersistedStartResponse(): void {
    try {
      localStorage.removeItem(cfg.localStorageKey);
    } catch {
      // ignore
    }
  }

  /** On boot, check the daemon for an in-flight import we should
   *  resume. Avoids the bug where the desktop process restarts mid-
   *  ingest and the ImportsTab shows an "Import …" button even though
   *  the daemon is actively chewing through the user's conversations.
   *
   *  The check has two sources:
   *  1. `getCorpusProgress(corpus_id)` — synchronous Tauri call into
   *     the AppState progress map. Returns whatever the status poller
   *     observed most recently. May be `null` immediately after a
   *     desktop start if the poller hasn't ticked yet.
   *  2. The `corpus-progress` event listener — once `init()` returns,
   *     any in-flight import emits within ~1s (poller cadence). The
   *     listener's `applyCorpusProgress` handles it.
   *
   *  Source (1) closes the gap between "listener attached" and "first
   *  event" so the tab doesn't briefly flash the idle/button state on
   *  start. */
  async function hydrateFromDaemon(): Promise<void> {
    // Check whether this source's corpus is already installed from a
    // prior import. This is independent of the in-flight check below
    // — a user with a finished import who hasn't done anything in
    // this session sits at stage `idle` + `alreadyInstalled: true`,
    // which the picker uses to render a "Re-import" affordance
    // instead of misleading them with "Import …".
    try {
      const corpora = await listCorpora();
      const target = corpora.find((c) => c.id === cfg.corpusId);
      if (target && target.status === "installed") {
        _state = { ..._state, alreadyInstalled: true };
      }
    } catch {
      // Daemon offline — leave alreadyInstalled false; the listener
      // path will catch up once the daemon comes back.
    }

    let snapshot: CorpusProgressPayload | null = null;
    try {
      snapshot = await getCorpusProgress(cfg.corpusId);
    } catch {
      // Daemon offline / IPC down — the listener path will catch up
      // once the daemon comes back.
      return;
    }
    if (!snapshot) return;
    if (snapshot.phase === "complete" || snapshot.phase === "failed") {
      // Stale terminal entry — don't surface it as in-progress.
      return;
    }

    const persisted = restoreStartResponse();
    // Detect "already in flight" by setting stage to ingesting and
    // restoring the persisted pre-flight info when available. Without
    // a saved startResponse the message-count + ETA band degrade but
    // the live progress card still renders correctly.
    _state = {
      ...INITIAL,
      stage: "ingesting",
      startResponse: persisted,
      startedAtMs: persisted ? performance.now() : performance.now(),
      ingestProgress: snapshot,
    };
    applyCorpusProgress(snapshot);
  }

  return {
    /** Reactive snapshot of the import state machine. */
    get state(): ImportState {
      return _state;
    },

    /** Corpus id this instance is keyed on. */
    get corpusId(): string {
      return cfg.corpusId;
    },

    /** Idempotent listener attach + daemon-state hydrate. Safe to
     *  call from every consumer mount — the singleton instance means
     *  the first caller pays the listener cost; subsequent mounts read
     *  the existing state and observe new events without re-
     *  subscribing. The hydrate step queries the daemon for any
     *  in-flight import so a desktop restart mid-ingest restores the
     *  progress card immediately instead of flashing the idle state. */
    async init(): Promise<void> {
      if (_initStarted) return _initStarted;
      _initStarted = (async () => {
        await ensureCorpusListener();
        await hydrateFromDaemon();
      })();
      await _initStarted;
    },

    /** Mark the import as starting. Called from `pickAndStart` on the
     *  card once the user picks a zip and the Tauri command is about
     *  to fire. Resets any prior terminal state so a fresh import is
     *  observable. */
    beginImport(): void {
      _state = {
        ...INITIAL,
        stage: "starting",
        startedAtMs: performance.now(),
      };
    },

    /** Record the Tauri command's pre-flight response. Routes the
     *  state-machine on the response variant: `started` → keep the
     *  spinner up, wait for progress; `partial_index_exists` →
     *  surface the confirmation banner.
     *
     *  Persists `started` responses to localStorage so a desktop
     *  restart mid-import restores the message-count + estimate
     *  display alongside the daemon-resumed progress card. */
    setStartResponse(resp: ImportStartResponse, zipPath: string): void {
      if (resp.kind === "partial_index_exists") {
        _state = {
          ..._state,
          stage: "needs_reset_confirm",
          startResponse: resp,
          pendingReset: {
            zipPath,
            indexPath: resp.index_path,
            totalMessages: resp.total_messages,
            estimatedMinutes: resp.estimated_minutes,
            canonicalPath: resp.canonical_path,
          },
        };
      } else {
        persistStartResponse(resp);
        _state = {
          ..._state,
          startResponse: resp,
          pendingReset: null,
        };
      }
    },

    /** Surface a synchronous failure from the import command or the
     *  file picker. */
    setError(message: string): void {
      _state = { ..._state, stage: "failed", errorMessage: message };
    },

    /** Reset to idle. Called when the user dismisses a terminal
     *  state (Retry button). Also drops the persisted pre-flight so
     *  a future desktop restart doesn't restore stale state. */
    reset(): void {
      stopEnrichPoll();
      clearPersistedStartResponse();
      _state = { ...INITIAL };
    },
  };
}

/** Settings → Imports store for the Claude (Anthropic) source. */
export const anthropicImportsStore = createImportsStore({
  corpusId: "conversations-anthropic",
  localStorageKey: "imports.lastStartResponse.v1",
});

/** Settings → Imports store for the ChatGPT (OpenAI) source. */
export const chatgptImportsStore = createImportsStore({
  corpusId: "conversations-chatgpt",
  localStorageKey: "imports.lastStartResponse.chatgpt.v1",
});

/** Library → Add store for the email-archive source. No auto-enrich:
 *  the recipe ships `[enrichment]` off, so ingest completion is terminal. */
export const emailImportsStore = createImportsStore({
  corpusId: "email-archive",
  localStorageKey: "imports.lastStartResponse.email.v1",
  autoEnrich: false,
});

/** Back-compat alias — the original single-source export. Points at the
 *  Anthropic instance so existing callers and e2e specs keep working. */
export const importsStore = anthropicImportsStore;
