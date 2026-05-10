// Reading-session store.
//
// Drives the glass-box reading surface: when the user clicks a
// citation in chat, this store opens the reading column with the
// cited chunk + immediate textual neighbors; an inquiry-trail
// breadcrumb traces how they got there. v1 ships chunk-level reading;
// PR3 adds the atom layer (dotted underlines, hover tooltip, atom
// panel); PR5 adds the "ask about this passage" context handoff.
//
// State machine (informal):
//   resting        — currentReading == null; chat occupies the full
//                    middle column.
//   reading-open   — currentReading != null && atomPanel == null.
//   atom-open      — currentReading != null && atomPanel != null.
//
// Persistence is intentionally in-memory only: a reading session is
// the trace of one user's exploration of one citation; reload =
// fresh slate. The conversation it was sourced from persists via
// the existing chat store.

import { invoke } from "@tauri-apps/api/core";

// ─── Types ────────────────────────────────────────────────────────

export interface AtomSpan {
  atom_id: string;
  /// "entity" | "event" | "state" | "relation" | "claim" | "question" | "configuration"
  atom_type: string;
  /// Byte offsets into ChunkRecord.content. `text.slice(span_start, span_end)`
  /// equals `surface_form` (note: JS string slicing uses UTF-16 code
  /// units; for ASCII content the byte/code-unit indices coincide,
  /// but multibyte chars require care — see ChunkRenderer comment).
  span_start: number;
  span_end: number;
  surface_form: string;
}

export interface ChunkRecord {
  chunk_id: number;
  corpus_id: string;
  content: string;
  title: string | null;
  url: string | null;
  source_doc_id: string | null;
  section_id: string | null;
  /// Atom mentions located inside `content`. Empty when no atlas
  /// or no spans landed for this chunk.
  atom_spans?: AtomSpan[];
  metadata: Record<string, unknown>;
  /// Populated by the backend when `corpus_id == "conversation-history"`.
  /// `null` for every other corpus. The frontend uses presence of this
  /// field to pick the conversation-shaped renderer over the default
  /// book renderer.
  conversation?: ConversationChunkMeta | null;
}

export interface ConversationChunkMeta {
  conversation_id: string;
  title: string | null;
  updated_at: number | null;
  segments: ConversationSegment[];
}

export interface ConversationSegment {
  /// "user" | "assistant" | "system" — preserved verbatim from the
  /// messages table; the renderer maps unknown roles to a neutral
  /// pill style.
  role: string;
  content: string;
}

export interface NeighborWindow {
  center: ChunkRecord;
  prev: ChunkRecord[];
  next: ChunkRecord[];
  outbound_url: string | null;
  ordering: string;
}

export interface BreadcrumbStep {
  kind: "question" | "chunk" | "atom-jump";
  label: string;
  // Targets are recorded so a click on a breadcrumb item can
  // restore that step's reading state. PR4 wires the click action;
  // v1 just records.
  target?: {
    corpusId?: string;
    chunkId?: number;
    atomId?: string;
    conversationId?: string;
  };
}

export interface FocusedPassage {
  corpusId: string;
  chunkId: number;
  title: string;
}

export interface RelatedAtom {
  atom_id: string;
  atom_type: string;
  canonical_name: string;
  edge_type: string;
  role: "source" | "target";
  confidence: number;
}

export interface CrossCorpusLink {
  peer_corpus_id: string;
  peer_atom_id: string;
  peer_canonical_name: string;
  edge_type: string;
  signal: string;
  confidence: number;
}

export interface AtomCard {
  atom_id: string;
  atom_type: string;
  corpus_id: string;
  canonical_name: string;
  aliases: string[];
  description: string;
  salience: number | null;
  enrichment_depth: string;
  related: RelatedAtom[];
  cross_corpus: CrossCorpusLink[];
}

export interface SectionRef {
  section_id: string;
  chunk_id: number | null;
  preview: string | null;
}

export interface AtomElsewhere {
  atom_id: string;
  corpus_id: string;
  same_corpus: SectionRef[];
  cross_corpus: CrossCorpusLink[];
}

export interface AtomPanelState {
  atomId: string;
  corpusId: string;
  card: AtomCard | null;
  elsewhere: AtomElsewhere | null;
  loading: boolean;
  error: string | null;
}

// ─── Internal state ───────────────────────────────────────────────

let _currentReading = $state<NeighborWindow | null>(null);
let _trail = $state<BreadcrumbStep[]>([]);
let _focusedPassage = $state<FocusedPassage | null>(null);
let _loading = $state(false);
let _error = $state<string | null>(null);
let _atomPanel = $state<AtomPanelState | null>(null);

// Callback installed by App.svelte at mount: dispatches a request
// to switch the chat sidebar's selectedConversationId. Lets the
// reading surface's "View conversation" button hand control back
// to the chat without the store importing the chat layer (and
// without prop-drilling through ReadingSurface → ConversationChunkRenderer).
let _onOpenConversation: ((conversationId: string) => void) | null = null;

// ─── Tauri bridge ─────────────────────────────────────────────────

async function fetchNeighbors(
  corpusId: string,
  chunkId: number,
  radius = 1,
): Promise<NeighborWindow | null> {
  try {
    const result = await invoke<NeighborWindow | null>(
      "read_get_chunk_neighbors",
      { corpusId, chunkId, radius },
    );
    return result;
  } catch (e) {
    console.warn("readingSession.fetchNeighbors failed:", e);
    return null;
  }
}

async function fetchAtomCard(
  corpusId: string,
  atomId: string,
): Promise<AtomCard | null> {
  try {
    return await invoke<AtomCard | null>("read_get_atom_card", {
      corpusId,
      atomId,
    });
  } catch (e) {
    console.warn("readingSession.fetchAtomCard failed:", e);
    return null;
  }
}

async function fetchAtomElsewhere(
  corpusId: string,
  atomId: string,
): Promise<AtomElsewhere | null> {
  try {
    return await invoke<AtomElsewhere | null>("read_get_atom_elsewhere", {
      corpusId,
      atomId,
    });
  } catch (e) {
    console.warn("readingSession.fetchAtomElsewhere failed:", e);
    return null;
  }
}

// ─── Public API ───────────────────────────────────────────────────

export const readingSession = {
  get currentReading(): NeighborWindow | null {
    return _currentReading;
  },
  get trail(): BreadcrumbStep[] {
    return _trail;
  },
  get focusedPassage(): FocusedPassage | null {
    return _focusedPassage;
  },
  get loading(): boolean {
    return _loading;
  },
  get error(): string | null {
    return _error;
  },
  /// Convenience derived flag — true when the reading column is open.
  get isOpen(): boolean {
    return _currentReading != null;
  },
  get atomPanel(): AtomPanelState | null {
    return _atomPanel;
  },
  get isAtomPanelOpen(): boolean {
    return _atomPanel != null;
  },

  /// Open a citation in the reading surface. Pushes a `question`
  /// step (originLabel) if the trail is empty, then a `chunk`
  /// step. Sets the focused passage so PR5's "ask about this
  /// passage" chip can pick it up immediately.
  async openCitation(
    corpusId: string,
    chunkId: number,
    originLabel: string,
  ): Promise<void> {
    _loading = true;
    _error = null;
    const window = await fetchNeighbors(corpusId, chunkId, 1);
    _loading = false;
    if (!window) {
      _error = `Could not load chunk ${chunkId} from ${corpusId}`;
      return;
    }
    if (_trail.length === 0) {
      _trail = [{ kind: "question", label: originLabel }];
    }
    const title = window.center.title ?? `Chunk ${chunkId}`;
    _trail = [
      ..._trail,
      {
        kind: "chunk",
        label: title,
        target: { corpusId, chunkId },
      },
    ];
    _currentReading = window;
    _focusedPassage = { corpusId, chunkId, title };
  },

  /// Replace the current reading with a new chunk (e.g., the user
  /// clicked an "elsewhere" row in the atom panel). Pushes an
  /// `atom-jump` step. Closes the atom panel — following an
  /// elsewhere link is a "now I'm reading something new" moment;
  /// the user can re-open the same atom from the new chunk.
  async jumpToChunk(
    corpusId: string,
    chunkId: number,
    viaLabel: string,
  ): Promise<void> {
    _loading = true;
    _error = null;
    const window = await fetchNeighbors(corpusId, chunkId, 1);
    _loading = false;
    if (!window) {
      _error = `Could not load chunk ${chunkId} from ${corpusId}`;
      return;
    }
    const title = window.center.title ?? `Chunk ${chunkId}`;
    _trail = [
      ..._trail,
      {
        kind: "atom-jump",
        label: viaLabel,
        target: { corpusId, chunkId },
      },
    ];
    _currentReading = window;
    _focusedPassage = { corpusId, chunkId, title };
    _atomPanel = null;
  },

  /// Open the atom panel for the given atom. Fires the card and
  /// elsewhere fetches in parallel; the panel renders incrementally
  /// as each lands.
  async openAtom(corpusId: string, atomId: string): Promise<void> {
    _atomPanel = {
      atomId,
      corpusId,
      card: null,
      elsewhere: null,
      loading: true,
      error: null,
    };
    const [card, elsewhere] = await Promise.all([
      fetchAtomCard(corpusId, atomId),
      fetchAtomElsewhere(corpusId, atomId),
    ]);
    if (
      _atomPanel?.atomId === atomId &&
      _atomPanel?.corpusId === corpusId
    ) {
      _atomPanel = {
        ..._atomPanel,
        card,
        elsewhere,
        loading: false,
        error:
          card == null && elsewhere == null
            ? "Atom not found or atlas missing"
            : null,
      };
    }
  },

  closeAtom(): void {
    _atomPanel = null;
  },

  /// Truncate the trail to (and including) the indexed step.
  /// Used by Breadcrumb item clicks. Index 0 = first step.
  truncateTrailTo(index: number): void {
    if (index < 0 || index >= _trail.length) return;
    _trail = _trail.slice(0, index + 1);
  },

  /// Close the reading surface entirely — chat returns to full
  /// width. Leaves `_focusedPassage` alone so the chip can persist
  /// (user closed the surface but might still want to ask about
  /// the passage). Use `clearFocus()` to drop both.
  closeReading(): void {
    _currentReading = null;
    _trail = [];
    _atomPanel = null;
  },

  setFocusedPassage(p: FocusedPassage | null): void {
    _focusedPassage = p;
  },

  clearFocus(): void {
    _focusedPassage = null;
  },

  /// Wire the "View conversation" callback. Called once from
  /// App.svelte's onMount; the reading surface invokes
  /// `openConversation(id)` to bounce back to the chat. Calling
  /// twice replaces the previous callback (the latest mount wins —
  /// in practice App is a singleton so this never matters).
  setConversationOpener(
    fn: ((conversationId: string) => void) | null,
  ): void {
    _onOpenConversation = fn;
  },

  /// Invoke the registered conversation opener (if any) and close
  /// the reading surface. Closes because the user is choosing to
  /// jump to the live conversation — staying on the reading column
  /// would obscure the chat they just opened.
  openConversation(conversationId: string): void {
    if (_onOpenConversation) {
      _onOpenConversation(conversationId);
    }
    _currentReading = null;
    _trail = [];
    _atomPanel = null;
    _focusedPassage = null;
  },
};
