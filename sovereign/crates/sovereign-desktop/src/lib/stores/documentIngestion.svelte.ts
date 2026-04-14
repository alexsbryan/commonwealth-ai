// Runed singleton for `document:progress` Tauri events — keyed by
// `asset_id`. Mirrors the shape of `corpusProgressStore` (see
// corpusProgress.svelte.ts); see that file's comment for the
// "why single listener" reasoning.
//
// Maps raw `DocumentProgressPayload` events into the shared
// `AssetState` tag-union so consumers don't have to re-derive it.
// DocumentLibrary, IngestBanner, and any future consumer read the
// same `state(assetId)` getter and get the same answer.
//
// Also exposes `onTerminal(callback)` so consumers that need to
// refetch a list (DocumentLibrary re-reads `listDocumentAssets()`
// when an asset transitions to Ready/Failed) can hook in without
// attaching yet another `listen("document:progress")`.
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { produce } from "immer";
import type {
  AssetState,
  DocumentProgressPayload,
} from "../types";

let _byId: Record<string, AssetState> = $state({});
let _unlisten: UnlistenFn | null = null;
let _listenerStarting: Promise<void> | null = null;
const _terminalHooks: Array<(assetId: string, state: AssetState) => void> = [];

function payloadToState(
  p: DocumentProgressPayload,
  prior?: AssetState,
): AssetState | undefined {
  // Translation mirrors IngestBanner.svelte's old inline logic, kept
  // in one place so future event types land here once.
  if (p.type === "Indexing") {
    return {
      Indexing: {
        chunks_done: p.done ?? 0,
        chunks_total: p.total ?? 1,
      },
    };
  }
  if (p.type === "RagAvailable") {
    return "PartiallyReady";
  }
  if (p.type === "BuildingSkeleton") {
    return {
      BuildingSkeleton: {
        chunks_done: p.done ?? 0,
        chunks_total: p.total ?? 1,
      },
    };
  }
  if (p.type === "Ready") {
    return "Ready";
  }
  if (p.type === "Failed") {
    return { Failed: { reason: p.reason ?? "Unknown error" } };
  }
  // Unknown event type — keep prior state.
  return prior;
}

function isTerminal(state: AssetState): boolean {
  return (
    state === "Ready" ||
    (typeof state === "object" && state !== null && "Failed" in state)
  );
}

async function ensureListening(): Promise<void> {
  if (_unlisten) return;
  if (_listenerStarting) return _listenerStarting;
  _listenerStarting = (async () => {
    _unlisten = await listen<DocumentProgressPayload>(
      "document:progress",
      (event) => {
        const p = event.payload;
        if (!p.asset_id) return;
        const assetId = p.asset_id;
        const prior = _byId[assetId];
        const next = payloadToState(p, prior);
        if (!next || next === prior) return;
        _byId = produce(_byId, (draft) => {
          draft[assetId] = next;
        });
        if (isTerminal(next)) {
          for (const hook of _terminalHooks) hook(assetId, next);
        }
      },
    );
  })();
  await _listenerStarting;
}

export const documentIngestionStore = {
  /** Reactive record: asset_id → latest `AssetState`. */
  get byId() {
    return _byId;
  },

  /** Current state for a specific asset, or `undefined` if no
   *  progress event has arrived yet. Callers typically fall back
   *  to the asset's initial `state` field when this is missing. */
  state(assetId: string): AssetState | undefined {
    return _byId[assetId];
  },

  /** Register a callback invoked when any asset transitions to a
   *  terminal state (Ready / Failed). Returns an unsubscribe fn.
   *  Used by DocumentLibrary to refetch the full asset list —
   *  previously that component attached its own `document:progress`
   *  listener for this purpose; now it's one shared listener.
   */
  onTerminal(
    hook: (assetId: string, state: AssetState) => void,
  ): () => void {
    _terminalHooks.push(hook);
    return () => {
      const idx = _terminalHooks.indexOf(hook);
      if (idx !== -1) _terminalHooks.splice(idx, 1);
    };
  },

  /** Attach the Tauri listener on demand. Idempotent. */
  async init(): Promise<void> {
    await ensureListening();
  },
};
