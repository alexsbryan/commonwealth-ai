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
// process, listens to `corpus-progress` (and `enrich://progress/*`
// once the post-install enrichment subprocess is spawned) globally,
// and exposes a small `importsStore` surface the tab reads.
//
// Two-stage flow this store models:
//   1. **Ingest** — kicked off by `import_anthropic_zip`. Progress
//      arrives on the shared `corpus-progress` event channel. When
//      that channel reports `phase = "complete"`, the store auto-
//      invokes `enrich_build_async` to fire the v2 atlas pipeline.
//      Without that hop, the user's atoms.json never lands and the
//      Atlas-View row never appears.
//   2. **Enrich** — the subprocess streams `EnrichProgress` on
//      `enrich://progress/{job_id}`. The store mirrors those into
//      its own derived view + flips `stage` to `complete` on the
//      enrichment `complete` event.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  enrichBuildAsync,
  getCorpusProgress,
  type ImportStartResponse,
} from "../api";
import type { CorpusProgressPayload, EnrichProgress, EnrichBuildStep } from "../types";

/** localStorage key for the most recent pre-flight response. Lets a
 *  desktop restart restore the message-count + estimated_minutes
 *  display alongside the live progress card. */
const LAST_START_RESPONSE_KEY = "imports.lastStartResponse.v1";

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
  /** Pre-flight info from `import_anthropic_zip`. `null` until the
   *  Tauri command resolves. */
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
  /** Latest enrichment-side progress event. */
  enrichProgress: EnrichProgress | null;
  /** Step ordinal during enrichment (1..total). Mirrors
   *  `EnrichProgress::StepStart`. */
  enrichStep: { step: EnrichBuildStep; ordinal: number; total: number } | null;
  /** Active enrich job id, if a subprocess is running. */
  enrichJobId: string | null;
  /** Per-job channel name for the enrich progress stream. */
  enrichChannel: string | null;
}

const TARGET_CORPUS_ID = "conversations-anthropic";

const INITIAL: ImportState = {
  stage: "idle",
  startResponse: null,
  pendingReset: null,
  startedAtMs: null,
  errorMessage: null,
  ingestProgress: null,
  enrichProgress: null,
  enrichStep: null,
  enrichJobId: null,
  enrichChannel: null,
};

let _state: ImportState = $state({ ...INITIAL });
let _corpusUnlisten: UnlistenFn | null = null;
let _enrichUnlisten: UnlistenFn | null = null;
let _initStarted: Promise<void> | null = null;

function applyCorpusProgress(p: CorpusProgressPayload): void {
  if (p.corpus_id !== TARGET_CORPUS_ID) return;
  _state = { ..._state, ingestProgress: p };
  if (_state.stage === "starting" || _state.stage === "ingesting") {
    if (p.phase === "complete") {
      // Ingest done — kick the v2 atlas enrichment subprocess. Without
      // this hop, the conversation_atlas pipeline never runs against
      // the freshly-ingested chunks and `atoms.json` never lands, so
      // the Atlas-View "Conversations" header never gets a row.
      void triggerEnrichment();
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

async function triggerEnrichment(): Promise<void> {
  _state = { ..._state, stage: "enriching" };
  try {
    const handle = await enrichBuildAsync(TARGET_CORPUS_ID, null, null);
    _state = {
      ..._state,
      enrichJobId: handle.job_id,
      enrichChannel: handle.channel,
    };
    await ensureEnrichListener(handle.channel);
  } catch (e) {
    _state = {
      ..._state,
      stage: "failed",
      errorMessage:
        e instanceof Error
          ? `Could not start enrichment: ${e.message}`
          : `Could not start enrichment: ${String(e)}`,
    };
  }
}

async function ensureEnrichListener(channel: string): Promise<void> {
  // Drop the prior listener (if a re-import is firing within the same
  // session) so we don't pump events from two different job IDs into
  // the same state slot.
  if (_enrichUnlisten) {
    _enrichUnlisten();
    _enrichUnlisten = null;
  }
  _enrichUnlisten = await listen<EnrichProgress>(channel, (event) => {
    applyEnrichProgress(event.payload);
  });
}

function applyEnrichProgress(e: EnrichProgress): void {
  _state = { ..._state, enrichProgress: e };
  switch (e.kind) {
    case "step_start":
      _state = {
        ..._state,
        enrichStep: { step: e.step, ordinal: e.ordinal, total: e.total },
      };
      break;
    case "complete":
      _state = { ..._state, stage: "complete" };
      detachEnrichListener();
      break;
    case "aborted":
    case "spawn_failed":
    case "cancelled":
      _state = {
        ..._state,
        stage: "failed",
        errorMessage:
          "message" in e
            ? e.message
            : e.kind === "aborted"
              ? `Enrichment aborted at step ${e.failed_step}`
              : "Enrichment cancelled",
      };
      detachEnrichListener();
      break;
  }
}

function detachEnrichListener(): void {
  if (_enrichUnlisten) {
    _enrichUnlisten();
    _enrichUnlisten = null;
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
    const raw = localStorage.getItem(LAST_START_RESPONSE_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as ImportStartResponse;
  } catch {
    return null;
  }
}

function persistStartResponse(resp: ImportStartResponse): void {
  try {
    localStorage.setItem(LAST_START_RESPONSE_KEY, JSON.stringify(resp));
  } catch {
    // localStorage quota / disabled — best-effort persistence;
    // import still works in-session.
  }
}

function clearPersistedStartResponse(): void {
  try {
    localStorage.removeItem(LAST_START_RESPONSE_KEY);
  } catch {
    // ignore
  }
}

/** On boot, check the daemon for an in-flight import we should
 *  resume. Avoids the bug where the desktop process restarts mid-
 *  ingest and the ImportsTab shows an "Import Claude export" button
 *  even though the daemon is actively chewing through the user's
 *  conversations.
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
  let snapshot: CorpusProgressPayload | null = null;
  try {
    snapshot = await getCorpusProgress(TARGET_CORPUS_ID);
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

export const importsStore = {
  /** Reactive snapshot of the import state machine. */
  get state(): ImportState {
    return _state;
  },

  /** Corpus id the tab is keyed on. v1 ships the Anthropic path;
   *  ChatGPT + Gemini would extend this to a per-source dict. */
  get corpusId(): string {
    return TARGET_CORPUS_ID;
  },

  /** Idempotent listener attach + daemon-state hydrate. Safe to
   *  call from every consumer mount — the module-level singleton
   *  means the first caller pays the listener cost; subsequent
   *  mounts read the existing state and observe new events without
   *  re-subscribing. The hydrate step queries the daemon for any
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
   *  tab once the user picks a zip and the Tauri command is about
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

  /** Surface a synchronous failure from `import_anthropic_zip` or
   *  the file picker. */
  setError(message: string): void {
    _state = { ..._state, stage: "failed", errorMessage: message };
  },

  /** Reset to idle. Called when the user dismisses a terminal
   *  state (Retry button). Also drops the persisted pre-flight so
   *  a future desktop restart doesn't restore stale state. */
  reset(): void {
    detachEnrichListener();
    clearPersistedStartResponse();
    _state = { ...INITIAL };
  },
};
