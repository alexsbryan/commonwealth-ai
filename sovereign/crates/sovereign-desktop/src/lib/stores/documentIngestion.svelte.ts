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

// ── ETA estimation ────────────────────────────────────────────
//
// We give the user an estimated completion time as soon as we have a
// measurable pace. The trick that makes this machine-independent: ETA
// = elapsed × (1 − fraction) / fraction. Absolute speed (fast laptop
// vs slow) divides out — only the *relative* cost of each phase needs
// to be known, and those are ratios, not wall-clock constants.
//
// The ingest walks three phases. Weights are their share of total
// wall-clock (the T3 RAPTOR build is explicitly the "~4-min window" in
// document_asset.rs, so it dominates). Rough but self-correcting: at
// each phase boundary `fraction` jumps to the cumulative weight and
// `elapsed` is known, so the estimate recalibrates. Calibrate the
// ratios against the book_report bench's time_to_rag_ready_ms /
// time_to_ready_ms if they drift.
type EtaPhase = "t1" | "t2" | "t3" | "done";
const PHASE_BASE: Record<EtaPhase, number> = { t1: 0.0, t2: 0.1, t3: 0.4, done: 1.0 };
const PHASE_SPAN: Record<EtaPhase, number> = { t1: 0.1, t2: 0.3, t3: 0.6, done: 0.0 };

interface EtaMeta {
  /** ms timestamp of the first sign of real work (embedding start). */
  startedAt: number;
  phase: EtaPhase;
  /** Sticky: once MultiHopReady fires, BuildingSkeleton means T3. */
  sawMultiHop: boolean;
}
// Plain object (not $state): read imperatively by the banner's 1s tick.
const _eta: Record<string, EtaMeta> = {};

function clamp01(x: number): number {
  return x < 0 ? 0 : x > 1 ? 1 : x;
}

/** Track phase + start time from the raw event stream. Driven off the
 *  event type (not the derived AssetState) so the T2↔T3 distinction —
 *  invisible in the reused `BuildingSkeleton` state — is preserved via
 *  the `MultiHopReady` milestone. */
function updateEtaMeta(assetId: string, p: DocumentProgressPayload): void {
  const m: EtaMeta = _eta[assetId] ?? {
    startedAt: 0,
    phase: "t1",
    sawMultiHop: false,
  };
  switch (p.type) {
    case "Started":
    case "Indexing":
      if (m.startedAt === 0) m.startedAt = Date.now();
      m.phase = "t1";
      break;
    case "RagAvailable":
      m.phase = "t2";
      break;
    case "MultiHopReady":
      m.sawMultiHop = true;
      m.phase = "t3";
      break;
    case "BuildingSkeleton":
      if (m.startedAt === 0) m.startedAt = Date.now();
      m.phase = m.sawMultiHop ? "t3" : "t2";
      break;
    case "Ready":
    case "Failed":
      m.phase = "done";
      break;
  }
  _eta[assetId] = m;
}

/** Sub-progress within the current phase, 0..1, from the live state's
 *  chunk counters. Milestone states (no counter) read as 0 — i.e. the
 *  start of the next phase. */
function subFraction(state: AssetState | undefined): number {
  if (typeof state === "object" && state !== null) {
    if ("Indexing" in state && state.Indexing.chunks_total > 0) {
      return clamp01(state.Indexing.chunks_done / state.Indexing.chunks_total);
    }
    if ("BuildingSkeleton" in state && state.BuildingSkeleton.chunks_total > 0) {
      return clamp01(
        state.BuildingSkeleton.chunks_done / state.BuildingSkeleton.chunks_total,
      );
    }
  }
  return 0;
}

function payloadToState(
  p: DocumentProgressPayload,
  prior?: AssetState,
): AssetState | undefined {
  // Translation mirrors IngestBanner.svelte's old inline logic, kept
  // in one place so future event types land here once.
  if (p.type === "Started") {
    // Earliest signal — embedding is about to begin. Flip the banner
    // off "Queued…" immediately to Indexing at 0%, seeding the total
    // from the event so the bar has a denominator before the first
    // batch completes. The embed slot's cold model load lands in this
    // window; without this the banner looked frozen on "Queued…".
    return {
      Indexing: {
        chunks_done: 0,
        chunks_total: p.chunk_count ?? p.total ?? 1,
      },
    };
  }
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
  if (p.type === "MultiHopReady") {
    return "MultiHopReady";
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
        // Track ETA phase off the raw event even when the derived state
        // is unchanged (e.g. a repeat BuildingSkeleton tick).
        updateEtaMeta(assetId, p);
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

  /** Estimated seconds until ingest completes, or `null` when there
   *  isn't enough signal yet (before the first embedding batch
   *  returns) or the asset is already terminal. The banner polls this
   *  ~once a second and renders a coarse "~N min left" label. */
  etaSeconds(assetId: string): number | null {
    const m = _eta[assetId];
    const state = _byId[assetId];
    if (!m || m.startedAt === 0 || m.phase === "done" || !state) return null;
    const fraction =
      PHASE_BASE[m.phase] + PHASE_SPAN[m.phase] * subFraction(state);
    // Below ~2% we have essentially no pace signal — say "estimating".
    if (fraction <= 0.02) return null;
    const elapsedSec = (Date.now() - m.startedAt) / 1000;
    return Math.max(0, (elapsedSec * (1 - fraction)) / fraction);
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
