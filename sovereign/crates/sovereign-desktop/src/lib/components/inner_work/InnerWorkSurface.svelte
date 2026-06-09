<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    createConversation,
    getConversation,
    sendMessageStream,
    cancelStream,
    toggleSkill,
    listSkills,
    renameConversation,
    getLastTurnProvenance,
    deleteConversation,
    finalizeInnerWorkConversation,
    forgetMemory,
    weakenMemory,
  } from "../../api";
  import type {
    MessageChunkPayload,
    MessageCompletePayload,
    ErrorPayload,
    MessageEntry,
  } from "../../types";
  import type { TurnProvenance } from "../../api";
  import { innerWorkSession } from "../../stores/innerWorkSession.svelte";
  import EchoOverlay from "./EchoOverlay.svelte";
  import ProvenancePanel from "./ProvenancePanel.svelte";
  import EntryHistoryDrawer from "./EntryHistoryDrawer.svelte";
  import type { DrawerEntry } from "./EntryHistoryDrawer.svelte";
  import HintCues from "./HintCues.svelte";
  import {
    humanizeWitnessError,
    tokenize,
    formatRelativeDate,
    formatDateline,
  } from "./innerWorkText";

  interface Props {
    /// Called when the user wants to leave inner-work mode. The
    /// sidebar entry doubles as the toggle; the brand mark in the
    /// corner also routes here.
    onExit?: () => void;
    /// Increment to trigger a history-drawer toggle from outside
    /// (nav-rail re-tap, Cmd+[ while inner work is active).
    historyToggle?: number;
    /// True iff the inner-work surface is the currently-visible view.
    /// App.svelte keeps this component mounted across visits (see
    /// the `inner-work-layer` keep-alive comment in App.svelte), so
    /// onMount/onDestroy fire at most once per app session. This
    /// prop drives the per-visit lifecycle: snapshot+deactivate
    /// peer skills on activate, restore them on deactivate.
    active?: boolean;
  }

  let { onExit, historyToggle = 0, active = true }: Props = $props();

  // React to external toggle signals (nav-rail re-tap, Cmd+[).
  // Initialize `prev` to the same literal default as the prop (0)
  // rather than reading `historyToggle` directly — referencing a
  // prop outside a reactive context trips `state_referenced_locally`
  // and would otherwise capture only the value at first render.
  let prevHistoryToggle = $state(0);
  $effect(() => {
    if (historyToggle !== prevHistoryToggle) {
      prevHistoryToggle = historyToggle;
      if (historyVisible) {
        closeHistory();
      } else {
        void openHistory();
      }
    }
  });

  // ── Threshold ───────────────────────────────────────────────
  // Once-per-window. The first navigation into inner-work plays an
  // 800ms gradient-only fade before the date and column appear. Re-
  // entering during the same session lands on the page directly.
  let thresholdActive = $state(!innerWorkSession.thresholdShown);

  // ── Welcome hints ───────────────────────────────────────────
  // Soft, staggered cues for the two non-discoverable shortcuts
  // (summon witness, view provenance). Played at most once per
  // window session and only when the surface arrives empty (no
  // prior turns, no resumed draft) — a returning entry doesn't get
  // re-welcomed. Total runtime ~12s; Esc dismisses immediately.
  let hintsActive = $state(false);
  let hintsTimer: ReturnType<typeof setTimeout> | null = null;
  const HINT_LIST = [
    { chord: "⌘↵", body: "when you're ready, summon a witness" },
    { chord: "⌘/", body: "to see what the witness drew on" },
  ];

  // ── Today's session ─────────────────────────────────────────
  let date = $state(innerWorkSession.todayIsoDate());
  let dateline = $derived(formatDateline(date));
  // True when the surface is viewing today (writeable mode). When the
  // user navigates to a past entry from the history drawer this flips
  // false: prior turns render, but the textarea + draft hide so the
  // surface treats past dates as read-only. Returning to today via
  // the drawer's "← Today" button restores write mode.
  let isOnToday = $derived(date === innerWorkSession.todayIsoDate());

  // The committed turns rendered in the document above the textarea.
  // A turn pairs the user's prose with the witness's reflection. While
  // the witness is composing, `witness_text` is null and `pending` is
  // true — the document shows the user's settled paragraph with a
  // single subtle dot in the gutter.
  type Turn = {
    /// Stable client-side id. Generated at creation, used to look up
    /// a Turn by `findIndex` within the (deep-proxied by Svelte 5)
    /// `turns` array. Avoids `array.indexOf(turn)` — that compares
    /// the captured plain-object reference against the proxied entry
    /// stored in the reactive array, which Svelte 5 explicitly warns
    /// about (`state_proxy_equality_mismatch`).
    client_id: string;
    user_text: string;
    witness_text: string | null;
    /// Set once `sendMessageStream` returns, used to match incoming
    /// `message-chunk` / `message-complete` / `message-error` events.
    message_id: string | null;
    pending: boolean;
    /// Buffered chunk text. We collect chunks but don't render them
    /// until `message-complete` fires — the design brief specifically
    /// rejects token streaming for this surface ("performs effort and
    /// pulls focus"). On complete we prefer `full_text` from the
    /// completion event over the buffered chunks for accuracy.
    buffer: string;
    /// Set when the witness stream errored before producing a reply.
    /// The surface renders this in place of the witness slot so the
    /// writer sees a visible non-response instead of silently empty
    /// space. Pre-2026-05-23 this was console.warn-only; an inner-work
    /// session that overflowed the witness's context window looked
    /// indistinguishable from a daemon hang.
    error: string | null;
  };

  function newClientId(): string {
    return (
      globalThis.crypto?.randomUUID?.() ??
      `t-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`
    );
  }

  let turns: Turn[] = $state([]);
  let conversationId: string | null = $state(null);

  // The current draft — what the user is typing but hasn't summoned
  // the witness for yet. Persisted to localStorage on every keystroke
  // (debounced) so a window close mid-write doesn't lose the text.
  let draftText = $state("");
  let textareaEl: HTMLTextAreaElement | null = $state(null);
  let scrollerEl: HTMLDivElement | null = $state(null);

  // True while any turn's witness response is composing. Used to gate
  // Esc as a cancel command and to disable a second Cmd+Return until
  // the first completes (sequential summons only — the brief frames
  // each as a discrete invitation).
  let composing = $derived(turns.some((t) => t.pending));

  // ── Echoes ──────────────────────────────────────────────────
  // Phase 3a: when a witness turn completes, look back through prior
  // user paragraphs in this session for one that resonates with the
  // just-committed paragraph. If we find one, queue a soft dot to
  // appear in the gutter beside the new paragraph after 8–12 seconds.
  // The dot click opens an overlay with the resonant fragment.
  //
  // Capped at MAX_ECHOES per session — more than that and the gutter
  // becomes a sidebar and the user starts reading the dots instead of
  // writing.
  //
  // The data source is intentionally local to this conversation for
  // now. Phase 3b swaps it out for the runtime's pre-turn memory
  // recall (`memories-used` event over the FTS5 memories store) — the
  // UI layer here doesn't change, only `findEcho` does.
  type Echo = {
    fragment: string;
    date_label: string;
  };
  const MAX_ECHOES = 3;

  /// Pending and active echo dots. Keyed by turn index in `turns`.
  let echoesByTurn: Record<number, Echo> = $state({});
  let pendingEchoTimers: ReturnType<typeof setTimeout>[] = [];
  let activeEcho: Echo | null = $state(null);

  // ── Provenance (Cmd+?) ─────────────────────────────────────
  // Glassbox: when the user presses Cmd+? the surface fetches the
  // most recent witness-turn provenance from the runtime and renders
  // it inline beneath the most recent turn. Each press refetches —
  // the runtime updates the cache on every witness summon, so a
  // stale display would mislead. Esc closes. The panel is purely a
  // diagnostic surface; it has no backend mutations.
  let provenanceVisible = $state(false);
  let provenance: TurnProvenance | null = $state(null);
  let provenanceLoading = $state(false);

  async function openProvenance() {
    provenanceVisible = true;
    if (!conversationId) {
      // No conversation yet means no witness response yet — the
      // panel will show its "no witness response yet" empty state.
      provenance = null;
      provenanceLoading = false;
      return;
    }
    provenanceLoading = true;
    try {
      provenance = await getLastTurnProvenance(conversationId);
    } catch (e) {
      console.warn("inner-work: getLastTurnProvenance failed:", e);
      provenance = null;
    } finally {
      provenanceLoading = false;
    }
  }

  function closeProvenance() {
    provenanceVisible = false;
  }

  // ── Past entries drawer (Cmd+H / dateline click) ───────────
  // The drawer lists every inner-work entry recorded in localStorage
  // for this device, lets the user navigate to any of them, and
  // exposes two debug actions for today's session: a non-destructive
  // reset (clears local draft + the date→id binding; the conversation
  // stays in the store) and a destructive delete (removes the
  // conversation from the store, with a confirm prompt).
  let historyVisible = $state(false);
  let historyLoading = $state(false);
  let historyEntries: DrawerEntry[] = $state([]);

  async function openHistory() {
    historyVisible = true;
    historyLoading = true;
    // Snapshot the index from localStorage, then enrich with previews
    // by reading each conversation. Reads are sequential to keep load
    // simple; the typical user has tens of entries, not thousands.
    const index = innerWorkSession.listEntryIndex();
    const enriched: DrawerEntry[] = [];
    for (const item of index) {
      let preview = "";
      try {
        const detail = await getConversation(item.conversationId);
        const firstUser = detail.messages.find((m) => m.role === "user");
        if (firstUser) {
          const trimmed = firstUser.content.trim().replace(/\s+/g, " ");
          preview = trimmed.length > 140 ? trimmed.slice(0, 140) + "…" : trimmed;
        }
      } catch (e) {
        // The conversation may have been deleted from the main list
        // between when we recorded it locally and now. Surface it as
        // a stale entry rather than dropping it silently — clicking
        // shows nothing and the user can use the reset action to
        // clear the dangling map.
        console.warn(
          `inner-work: failed to load past entry ${item.conversationId}:`,
          e,
        );
        preview = "(unavailable — entry may have been deleted)";
      }
      enriched.push({
        dateIso: item.dateIso,
        conversationId: item.conversationId,
        dateLabel: formatDateline(item.dateIso),
        preview,
        isCurrent: item.conversationId === conversationId,
      });
    }
    historyEntries = enriched;
    historyLoading = false;
  }

  function closeHistory() {
    historyVisible = false;
  }

  /// Belt-and-suspenders: flush any pending draft save before we
  /// swap the date out from under it. The 400ms debounce in
  /// `scheduleSave` could otherwise write the previous date's draft
  /// against the new date if a user types and immediately opens the
  /// drawer.
  function flushPendingDraftSave() {
    if (saveTimer) {
      clearTimeout(saveTimer);
      innerWorkSession.saveDraft(date, draftText);
      saveTimer = null;
    }
  }

  /// Tear down per-view ephemera (echo timers, echo dots, provenance
  /// panel) when navigating between dates so a stale dot from
  /// yesterday doesn't appear next to a fresh paragraph.
  function clearViewEphemera() {
    for (const t of pendingEchoTimers) clearTimeout(t);
    pendingEchoTimers = [];
    echoesByTurn = {};
    provenanceVisible = false;
    provenance = null;
  }

  async function selectEntry(entry: DrawerEntry) {
    closeHistory();
    flushPendingDraftSave();
    clearViewEphemera();
    date = entry.dateIso;
    conversationId = entry.conversationId;
    // Past entries are read-only — drop any draft text from the prior
    // view; today's draft remains in localStorage and resumes on
    // return.
    draftText = "";
    try {
      const detail = await getConversation(entry.conversationId);
      turns = pairTurnsFromMessages(detail.messages);
    } catch (e) {
      console.warn("inner-work: failed to load entry:", e);
      turns = [];
    }
    await tick();
    scrollerEl?.scrollTo({ top: 0, behavior: "instant" });
  }

  async function returnToToday() {
    closeHistory();
    clearViewEphemera();
    const todayIso = innerWorkSession.todayIsoDate();
    date = todayIso;
    const todayConvId = innerWorkSession.getConversationIdFor(todayIso);
    conversationId = todayConvId;
    draftText = innerWorkSession.loadDraft(todayIso);
    if (todayConvId) {
      try {
        const detail = await getConversation(todayConvId);
        turns = pairTurnsFromMessages(detail.messages);
      } catch {
        turns = [];
      }
    } else {
      turns = [];
    }
    await tick();
    autoSizeTextarea();
    textareaEl?.focus();
  }

  async function resetTodayLocal() {
    if (!isOnToday) return;
    flushPendingDraftSave();
    clearViewEphemera();
    innerWorkSession.saveDraft(date, "");
    innerWorkSession.clearConversationIdFor(date);
    draftText = "";
    conversationId = null;
    turns = [];
    closeHistory();
    await tick();
    autoSizeTextarea();
    textareaEl?.focus();
  }

  async function deleteTodayEntry() {
    // The drawer arms this with its own two-step "click again to
    // confirm" UI before invoking; we don't run a `window.confirm`
    // here. (Tauri's WKWebView bridge returned `true` from
    // `window.confirm()` regardless of which button the user clicked,
    // so Cancel was being silently ignored. Confirmation has to live
    // in the drawer, not the system dialog.)
    if (!isOnToday) return;
    if (!conversationId) {
      // No persisted conversation yet — nothing to delete; equivalent
      // to a local reset.
      await resetTodayLocal();
      return;
    }
    try {
      await deleteConversation(conversationId);
    } catch (e) {
      console.warn("inner-work: failed to delete today's entry:", e);
      // Continue with the local reset — the user's intent is "start
      // fresh," and a stale local map is worse than a possibly-
      // orphaned conversation in the store.
    }
    await resetTodayLocal();
  }

  function findEchoFromConversation(targetIdx: number): Echo | null {
    const target = turns[targetIdx];
    if (!target) return null;
    const targetWords = tokenize(target.user_text);
    let best: { idx: number; overlap: number } | null = null;
    for (let i = 0; i < targetIdx; i++) {
      const candidate = turns[i];
      if (!candidate) continue;
      const candWords = tokenize(candidate.user_text);
      let overlap = 0;
      for (const w of candWords) if (targetWords.has(w)) overlap++;
      // Threshold conservative: 3 shared content words. Erroneous
      // echoes break trust faster than missing echoes break value.
      if (overlap >= 3 && (!best || overlap > best.overlap)) {
        best = { idx: i, overlap };
      }
    }
    if (!best) return null;
    return {
      fragment: turns[best.idx].user_text,
      date_label: "earlier in this entry",
    };
  }

  // Phase 3b: prefer the runtime's pre-turn memory recall when the
  // message metadata carries it. The runtime emits
  // `metadata.recalled_memories: [{id, content, created_at}]` on the
  // relational/witness path — that's the FTS5 + cosine top-K result
  // the witness actually drew on, not a frontend heuristic.
  //
  // Falls through to in-conversation similarity when metadata is
  // absent (e.g., older runtimes, non-relational classifications, or
  // pure-test scenarios where the shim doesn't populate metadata).
  type RecalledMemory = { id: string; content: string; created_at: number };

  function findEchoFromMetadata(
    metadata: Record<string, unknown> | null | undefined,
  ): Echo | null {
    if (!metadata) return null;
    const raw = (metadata as { recalled_memories?: unknown }).recalled_memories;
    if (!Array.isArray(raw) || raw.length === 0) return null;
    // Take the top-ranked memory. The runtime sorted by cosine
    // already, so item 0 is the most resonant; later entries fade
    // into ambient noise.
    const top = raw[0] as Partial<RecalledMemory>;
    if (!top || typeof top.content !== "string") return null;
    const created = typeof top.created_at === "number" ? top.created_at : null;
    return {
      fragment: top.content,
      date_label: created !== null ? formatRelativeDate(created) : "",
    };
  }

  function maybeQueueEcho(
    turnIdx: number,
    completionMetadata?: Record<string, unknown> | null,
  ) {
    if (Object.keys(echoesByTurn).length >= MAX_ECHOES) return;
    if (echoesByTurn[turnIdx]) return; // already echoed
    // Prefer the runtime's recalled memories (Phase 3b), fall back
    // to in-conversation similarity (Phase 3a) when metadata is absent.
    const candidate =
      findEchoFromMetadata(completionMetadata) ??
      findEchoFromConversation(turnIdx);
    if (!candidate) return;
    // 8–12 second delay. Immediate appearance reads as surveillance;
    // delayed appearance reads as reflection. Tests override via
    // `window.__inner_work_echo_delay_ms__` to avoid burning real
    // wall-clock seconds in the e2e suite.
    const override = (
      globalThis as { __inner_work_echo_delay_ms__?: number }
    ).__inner_work_echo_delay_ms__;
    const delayMs =
      typeof override === "number"
        ? override
        : 8000 + Math.floor(Math.random() * 4000);
    const timer = setTimeout(() => {
      // Re-check the cap and turn validity in case state shifted while
      // the timer was queued.
      if (Object.keys(echoesByTurn).length >= MAX_ECHOES) return;
      if (!turns[turnIdx]) return;
      echoesByTurn = { ...echoesByTurn, [turnIdx]: candidate };
    }, delayMs);
    pendingEchoTimers.push(timer);
  }

  // ── Listeners ───────────────────────────────────────────────
  let unlistenChunk: UnlistenFn | null = null;
  let unlistenComplete: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;
  let thresholdTimer: ReturnType<typeof setTimeout> | null = null;

  // Snapshot of skills that were active when the user entered the
  // inner-work surface, captured so we can restore the user's prior
  // skill set on exit. Excludes "inner-work" itself.
  //
  // Why exclusive activation matters: a witness that shares the
  // active set with a research/knowledge skill will see optional
  // tools from that other skill in its planner space, and the
  // routing classifier may pick the other skill as primary on
  // shapes the inner-work skill doesn't strongly trigger on. The
  // 2026-05-04 incident — heartfelt journal entry routed through a
  // citation-grounded retrieval path that surfaced code-corpus
  // chunks — was a direct consequence of co-active non-witness
  // skills. The witness owns the page, full stop.
  let priorActiveSkillIds: string[] = [];

  // Surface skill id — tags every conversation this surface creates
  // so routing applies the inner-work intent_policy + witness path.
  // Co-located with the surface that owns it (2026-05-24
  // architecture redesign). See `Runtime::resolve_active_mode`.
  const SURFACE_SKILL_ID = "inner-work";

  /// Per-visit entry. Pre-2026-05-24 this also snapshotted peer
  /// skills + toggled the inner-work skill on (a workaround for the
  /// global-registry-state routing model that would otherwise let
  /// other active skills' tools leak into the witness path). With
  /// routing now driven by the conversation's surface tag set at
  /// create-time, peer skills can't pollute the witness path even
  /// when active — the structural surface override is the single
  /// source of truth. Entry becomes a no-op; kept as a hook so
  /// future per-visit work (analytics, telemetry) has a clear seam.
  async function enterSurface(): Promise<void> {
    // intentionally empty post-redesign
  }

  /// Per-visit exit. Cancels any in-flight witness stream, flushes
  /// the draft save, and triggers memory extraction so the
  /// conversation's witness-tagged memories land before the user
  /// can leave. Skill toggling removed (see `enterSurface`).
  function leaveSurface(): void {
    // If a witness response is in-flight, cancel it — leaving an
    // orphaned stream would render its message-complete event into
    // a void. Unlatch each pending turn locally too so a late-
    // arriving completion event has no turn to attach itself to.
    if (composing && conversationId) {
      cancelStream(conversationId).catch(() => {});
      for (const t of turns) {
        if (t.pending) {
          t.pending = false;
          t.witness_text = null;
          t.buffer = "";
          t.message_id = null;
        }
      }
    }
    // Trigger memory extraction on the just-finished conversation.
    // The runtime stamps every extracted memory with
    // source_skill_id = "inner-work" so they only recall in
    // future inner-work sessions.
    if (conversationId) {
      finalizeInnerWorkConversation(conversationId).catch((e) => {
        console.warn("inner-work: memory extraction failed:", e);
      });
    }
    // Flush any pending draft save.
    if (saveTimer) {
      clearTimeout(saveTimer);
      innerWorkSession.saveDraft(date, draftText);
      saveTimer = null;
    }
    // Drop any queued echo dots that haven't fired yet.
    for (const t of pendingEchoTimers) clearTimeout(t);
    pendingEchoTimers = [];
  }

  // Per-visit lifecycle. `active` is true on first mount (default
  // prop), so we run an initial enter via onMount and then this
  // effect handles every subsequent transition.
  let prevActive = $state(true);
  $effect(() => {
    if (active === prevActive) return;
    prevActive = active;
    if (active) {
      void enterSurface();
    } else {
      leaveSurface();
    }
  });

  onMount(async () => {
    // First-mount entry: peer-skill snapshot + activate.
    await enterSurface();

    // Resume today's existing inner-work conversation if one is
    // remembered locally, otherwise leave creation lazy until the
    // first witness summon. This keeps "open the page, write nothing,
    // close" from creating empty conversations in the main list.
    const existing = innerWorkSession.getConversationIdFor(date);
    if (existing) {
      try {
        const detail = await getConversation(existing);
        conversationId = detail.id;
        turns = pairTurnsFromMessages(detail.messages);
      } catch (e) {
        // The conversation may have been deleted from the main list.
        // Drop the mapping and start fresh on next summon.
        console.warn(
          "inner-work: failed to load remembered conversation, clearing map:",
          e,
        );
        innerWorkSession.clearConversationIdFor(date);
      }
    }

    // Migrate Phase 1 draft into the textarea. Phase 2 doesn't
    // change the storage key, so a user who left a draft yesterday
    // still finds it today on the right date — and a draft from
    // earlier today resumes seamlessly.
    draftText = innerWorkSession.loadDraft(date);

    // Wire stream listeners. Filtering by conversation_id and
    // message_id happens inside the handlers — listeners stay live
    // across the surface's lifetime.
    // Match incoming events to a turn. We prefer the message_id
    // (set after `sendMessageStream` resolves), but fall back to
    // "the single in-flight pending turn" when the id hasn't been
    // attached yet — the runtime can emit completion before our
    // post-await assignment lands. Sequential summons are gated by
    // `composing`, so at most one turn is ever pending at a time.
    function findTurnFor(messageId: string): Turn | undefined {
      const byId = turns.find((t) => t.message_id === messageId);
      if (byId) return byId;
      return turns.find((t) => t.pending);
    }

    unlistenChunk = await listen<MessageChunkPayload>(
      "message-chunk",
      (event) => {
        const p = event.payload;
        const target = findTurnFor(p.message_id);
        if (target) target.buffer += p.chunk;
      },
    );

    unlistenComplete = await listen<MessageCompletePayload>(
      "message-complete",
      async (event) => {
        const p = event.payload;
        const target = findTurnFor(p.message_id);
        if (target) {
          // Prefer the authoritative full_text over the buffered
          // chunks — they should match, but full_text is what gets
          // persisted server-side.
          target.witness_text = p.full_text || target.buffer;
          target.pending = false;
          // Belated id-attach so a future event re-finds the same
          // turn deterministically.
          if (!target.message_id) target.message_id = p.message_id;
          // Phase 3a/3b: queue an echo against this just-completed
          // turn. The metadata may carry the runtime's recalled
          // memories (preferred); if not, the in-conversation
          // similarity heuristic is the fallback.
          const idx = turns.findIndex((t) => t.client_id === target.client_id);
          if (idx >= 0) {
            maybeQueueEcho(
              idx,
              p.metadata as Record<string, unknown> | undefined,
            );
          }
        }
        // After a beat, scroll the (now-grown) document so the
        // textarea is back in view and the cursor lands where the
        // user expects.
        await tick();
        scrollToTextarea();
        textareaEl?.focus();
      },
    );

    unlistenError = await listen<ErrorPayload>("message-error", (event) => {
      // Drop any pending witness text — design brief: no half-paragraph
      // stranded in the document. The user's prose stays as a settled
      // paragraph; the marginalia is replaced with a single faint line
      // surfacing why the witness didn't speak. Pre-2026-05-23 this was
      // console.warn-only — a witness whose system prompt overflowed
      // the context window looked indistinguishable from a daemon hang,
      // and the user typed into a surface that appeared frozen.
      console.warn("inner-work: stream error:", event.payload.message);
      const friendly = humanizeWitnessError(event.payload.message);
      for (const t of turns) {
        if (t.pending) {
          t.pending = false;
          t.witness_text = null;
          t.error = friendly;
        }
      }
    });

    if (thresholdActive) {
      thresholdTimer = setTimeout(() => {
        thresholdActive = false;
        innerWorkSession.markThresholdShown();
        // Defer focus until the threshold fades to avoid a focus ring
        // flicker against the gradient field.
        setTimeout(() => textareaEl?.focus(), 200);
        thresholdTimer = null;
        maybePlayHints(/* postThreshold */ true);
      }, 800);
    } else {
      // Returning user — focus immediately.
      setTimeout(() => textareaEl?.focus(), 0);
      maybePlayHints(/* postThreshold */ false);
    }
  });

  /// Play the welcome hints once per window session, but only when
  /// the surface arrives empty (no prior turns, no resumed draft).
  /// A returning entry already speaks for itself; the cues would
  /// just clutter the column the user came back to read.
  ///
  /// Total runtime: 2 cues × 1400ms stagger + 9500ms keyframe =
  /// ~12.3s end-to-end. The cleanup timer is the upper bound;
  /// Esc dismisses earlier when the user wants quiet (handled in
  /// the surface keyboard handler — `hintsActive = false`).
  function maybePlayHints(postThreshold: boolean) {
    if (innerWorkSession.hintsShown) return;
    if (turns.length > 0 || draftText.length > 0) {
      // Mark shown anyway so a future empty-arrival in this same
      // window doesn't replay them after the user has been writing.
      innerWorkSession.markHintsShown();
      return;
    }
    // Small grace period before the cues appear so the column has
    // a moment to settle. After the threshold fade the eye lands on
    // the dateline first, then the hints float in.
    const lead = postThreshold ? 700 : 350;
    setTimeout(() => {
      hintsActive = true;
      innerWorkSession.markHintsShown();
      // Tear down the DOM after the longest cue's animation
      // completes so we don't keep a no-op element parked at z=5.
      const HINT_RUNTIME =
        HINT_LIST.length * 1400 + 9500 + 200; // staggers + keyframe + slack
      hintsTimer = setTimeout(() => {
        hintsActive = false;
        hintsTimer = null;
      }, HINT_RUNTIME);
    }, lead);
  }

  // ── Draft persistence ───────────────────────────────────────
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  function scheduleSave() {
    autoSizeTextarea();
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      innerWorkSession.saveDraft(date, draftText);
      saveTimer = null;
    }, 400);
  }

  function autoSizeTextarea() {
    if (!textareaEl) return;
    textareaEl.style.height = "auto";
    textareaEl.style.height = `${textareaEl.scrollHeight}px`;
  }

  function scrollToTextarea() {
    if (textareaEl && scrollerEl) {
      const target = textareaEl.offsetTop - 80;
      scrollerEl.scrollTo({ top: target, behavior: "smooth" });
    }
  }

  onDestroy(() => {
    // Timer + listener teardown. Per-visit skill restoration +
    // memory extraction live in leaveSurface(), which runs on every
    // `active` false-transition; here we only handle the once-per-
    // app-session unmount path (full window close, HMR teardown).
    if (thresholdTimer) {
      clearTimeout(thresholdTimer);
      thresholdTimer = null;
    }
    if (hintsTimer) {
      clearTimeout(hintsTimer);
      hintsTimer = null;
    }
    unlistenChunk?.();
    unlistenComplete?.();
    unlistenError?.();
    // If the user closes the window while inner-work is still the
    // active view, run the per-visit cleanup one last time —
    // leaveSurface() is idempotent against an already-empty snapshot.
    if (active) {
      leaveSurface();
    }
  });

  // ── Witness summon ──────────────────────────────────────────
  async function summonWitness() {
    const text = draftText.trim();
    if (text.length === 0) return;
    if (composing) return; // sequential summons only

    // Lazy-create the conversation on first summon of the day. This
    // keeps "open, write nothing, close" from leaving empty entries
    // in the main conversation list. Tagged with the inner-work
    // surface skill so routing applies the witness handler from
    // turn one — without the tag the conversation would fall
    // through to default chat (2026-05-24 architecture redesign).
    if (!conversationId) {
      try {
        const created = await createConversation(SURFACE_SKILL_ID);
        conversationId = created.id;
        innerWorkSession.setConversationIdFor(date, created.id);
        // Title with the dateline so the entry is recognisable in
        // the main conversation list. Best-effort — a rename failure
        // doesn't block the summon, the fallback title is fine.
        renameConversation(created.id, `Inner Work — ${dateline}`).catch(() => {});
      } catch (e) {
        console.error("inner-work: failed to create conversation:", e);
        return;
      }
    }

    // Append a pending turn with the user's text. The witness slot
    // is empty until the stream completes. The document grows; the
    // textarea clears and scrolls into view for continued writing.
    const turn: Turn = {
      client_id: newClientId(),
      user_text: text,
      witness_text: null,
      message_id: null,
      pending: true,
      buffer: "",
      error: null,
    };
    turns = [...turns, turn];

    // Clear the draft (both UI and localStorage) — the text is now
    // committed as the user portion of a turn.
    draftText = "";
    innerWorkSession.saveDraft(date, "");
    if (saveTimer) {
      clearTimeout(saveTimer);
      saveTimer = null;
    }
    await tick();
    autoSizeTextarea();
    scrollToTextarea();

    try {
      const started = await sendMessageStream(text, conversationId);
      // Attach the message_id so incoming events route to this turn.
      const idx = turns.findIndex((t) => t.client_id === turn.client_id);
      if (idx >= 0) turns[idx].message_id = started.message_id;
    } catch (e) {
      console.error("inner-work: failed to start stream:", e);
      // Roll back the pending turn — the witness never had a chance
      // to compose. Restore the draft so the user can try again.
      const idx = turns.findIndex((t) => t.client_id === turn.client_id);
      if (idx >= 0) {
        turns = turns.filter((_, i) => i !== idx);
      }
      draftText = text;
      innerWorkSession.saveDraft(date, text);
      await tick();
      autoSizeTextarea();
    }
  }

  async function cancelWitness() {
    if (!composing || !conversationId) return;
    try {
      await cancelStream(conversationId);
    } catch (e) {
      console.warn("inner-work: cancel failed:", e);
    }
    // Even if the cancel call hadn't reached the runtime yet, we
    // unlatch the pending turn locally — the design contract says
    // Esc discards any partial output and returns the cursor to the
    // user. We also null `message_id` so a late-arriving
    // `message-complete` doesn't re-attach itself by id and rewrite
    // `witness_text` after the fact (findTurnFor's by-id branch would
    // otherwise match the cancelled turn).
    for (const t of turns) {
      if (t.pending) {
        t.pending = false;
        t.witness_text = null;
        t.buffer = "";
        t.message_id = null;
      }
    }
    // `composing` is $derived from turns.some(pending); flipping
    // pending=false above also flips composing automatically.
    textareaEl?.focus();
  }

  // ── Keyboard ────────────────────────────────────────────────
  function handleKeydown(e: KeyboardEvent) {
    const isSummonChord = (e.metaKey || e.ctrlKey) && e.key === "Enter";
    if (isSummonChord) {
      e.preventDefault();
      void summonWitness();
      return;
    }
    // Cmd+? toggles the provenance panel. We bind to the physical
    // Slash key (`e.code === "Slash"`) rather than `e.key === "?"`
    // because browsers and the Tauri WebView disagree on what `e.key`
    // reports when Cmd is held with Shift+/ — some yield "?", others
    // yield "/" and leave Shift on `e.shiftKey`. Binding to the code
    // matches both. We also accept plain Cmd+/ (no Shift) as a
    // friendlier alias since the physical chord is awkward and
    // there's no conflicting binding here.
    //
    // The chord re-fetches every press so the panel always reflects
    // the most recent witness turn, never a stale capture.
    const isProvenanceChord =
      (e.metaKey || e.ctrlKey) && e.code === "Slash";
    if (isProvenanceChord) {
      e.preventDefault();
      if (provenanceVisible) {
        closeProvenance();
      } else {
        void openProvenance();
      }
      return;
    }
    // Cmd+H toggles the past-entries drawer. Bound to `code === "KeyH"`
    // for the same physical-key reasons as Cmd+/ above (the WebView
    // can disagree on `e.key` when Cmd is held). On macOS the system-
    // wide Cmd+H ("Hide window") is intercepted by the OS BEFORE the
    // WebView sees the keydown, so this binding only fires inside
    // contexts where Cmd+H is not the system shortcut — i.e. when
    // the inner-work surface is focused but Tauri's window is not the
    // hide-target. In practice users open the drawer via the dateline
    // button; the chord is a power-user nicety.
    const isHistoryChord =
      (e.metaKey || e.ctrlKey) && e.code === "KeyH";
    if (isHistoryChord) {
      e.preventDefault();
      if (historyVisible) {
        closeHistory();
      } else {
        void openHistory();
      }
      return;
    }
    if (e.key === "Escape") {
      // Esc precedence: cancel a composing stream first (the most
      // user-disruptive thing to leave running), then close the
      // provenance panel, then close the history drawer, then
      // dismiss the welcome cues if they're still playing.
      if (composing) {
        e.preventDefault();
        void cancelWitness();
        return;
      }
      if (provenanceVisible) {
        e.preventDefault();
        closeProvenance();
        return;
      }
      if (historyVisible) {
        e.preventDefault();
        closeHistory();
        return;
      }
      if (hintsActive) {
        e.preventDefault();
        hintsActive = false;
        if (hintsTimer) {
          clearTimeout(hintsTimer);
          hintsTimer = null;
        }
        return;
      }
    }
  }

  function exit() {
    if (onExit) onExit();
  }

  // ── Helpers ─────────────────────────────────────────────────
  /// Pair user/assistant messages from a conversation detail into the
  /// document's Turn shape. Skips system messages and orphan user
  /// messages (a user message without a following assistant message
  /// is rare in a normal flow but can happen if a stream errored
  /// server-side after persistence).
  function pairTurnsFromMessages(messages: MessageEntry[]): Turn[] {
    const result: Turn[] = [];
    let pendingUser: MessageEntry | null = null;
    for (const m of messages) {
      if (m.role === "user") {
        pendingUser = m;
      } else if (m.role === "assistant" && pendingUser) {
        result.push({
          client_id: newClientId(),
          user_text: pendingUser.content,
          witness_text: m.content,
          message_id: m.id,
          pending: false,
          buffer: "",
          error: null,
        });
        pendingUser = null;
      } else {
        // assistant without a preceding user (system bootstraps,
        // tool-only turns) — skip rather than render uncoupled.
      }
    }
    if (pendingUser) {
      // Orphan user turn — render it as a settled paragraph with no
      // marginalia, mirroring the cancelled-stream presentation.
      result.push({
        client_id: newClientId(),
        user_text: pendingUser.content,
        witness_text: null,
        message_id: null,
        pending: false,
        buffer: "",
        error: null,
      });
    }
    return result;
  }

  // After draftText changes via paste / programmatic set, keep the
  // textarea sized correctly. `oninput` covers keystroke; this $effect
  // handles initial mount + restore-on-error paths.
  $effect(() => {
    if (draftText !== undefined && textareaEl) {
      autoSizeTextarea();
    }
  });
</script>

<svelte:window on:keydown={handleKeydown} />

<div class="root" class:threshold={thresholdActive}>
  <!-- Background field: gradient + grain. Both fixed to the viewport
       so they don't scroll with content. -->
  <div class="field" aria-hidden="true"></div>
  <div class="grain" aria-hidden="true"></div>

  <button
    class="exit-mark"
    onclick={exit}
    title="Return"
    aria-label="Return to chat"
  >◈</button>

  <div class="local-indicator" aria-label="Stored locally">
    <svg width="9" height="11" viewBox="0 0 9 11" fill="none" aria-hidden="true">
      <rect x="1" y="5" width="7" height="5" rx="0.7" stroke="currentColor" stroke-width="0.9"/>
      <path d="M2.5 5V3.2a2 2 0 014 0V5" stroke="currentColor" stroke-width="0.9" fill="none"/>
    </svg>
    <span>local</span>
  </div>

  {#if !thresholdActive}
    <div class="scroller" bind:this={scrollerEl}>
      <div class="content">
        <button
          type="button"
          class="dateline"
          onclick={() => (historyVisible ? closeHistory() : void openHistory())}
          title="Past entries (⌘H)"
          aria-label="Open past entries"
        >
          <span class="dateline-text">{dateline}</span>
          {#if !isOnToday}
            <span class="dateline-tag">past entry</span>
          {/if}
        </button>

        {#each turns as turn, i (i)}
          <article class="turn" data-pending={turn.pending}>
            {#if echoesByTurn[i]}
              <button
                class="echo-dot"
                aria-label="Open earlier writing this paragraph echoes"
                title="Open earlier writing this paragraph echoes"
                onclick={() => (activeEcho = echoesByTurn[i])}
              ></button>
            {/if}
            <p class="user-prose">{turn.user_text}</p>
            {#if turn.pending}
              <div class="composing" aria-label="The witness is composing">
                <span class="composing-dot"></span>
              </div>
            {:else if turn.witness_text}
              <blockquote class="witness">{turn.witness_text}</blockquote>
            {:else if turn.error}
              <p class="witness-error" role="status">{turn.error}</p>
            {/if}
          </article>
        {/each}

        {#if provenanceVisible}
          <ProvenancePanel
            {provenance}
            loading={provenanceLoading}
            onClose={closeProvenance}
            onRefresh={() => void openProvenance()}
          />
        {/if}

        {#if isOnToday}
          <textarea
            class="column"
            bind:this={textareaEl}
            bind:value={draftText}
            oninput={scheduleSave}
            spellcheck="true"
            autocapitalize="sentences"
            placeholder=""
            aria-label="Inner work entry for {dateline}"
          ></textarea>
        {:else}
          <p class="past-note">
            Viewing a past entry. Press <kbd>⌘H</kbd> for history,
            or use the drawer's <em>← Today</em> button to return.
          </p>
        {/if}
      </div>
    </div>
  {/if}

  <EntryHistoryDrawer
    open={historyVisible}
    loading={historyLoading}
    entries={historyEntries}
    {isOnToday}
    onClose={closeHistory}
    onSelect={selectEntry}
    onReturnToToday={returnToToday}
    onResetToday={resetTodayLocal}
    onDeleteToday={deleteTodayEntry}
  />

  {#if hintsActive}
    <HintCues hints={HINT_LIST} />
  {/if}

  {#if activeEcho}
    <EchoOverlay
      fragment={activeEcho.fragment}
      dateLabel={activeEcho.date_label}
      onClose={() => (activeEcho = null)}
    />
  {/if}
</div>

<style>
  /* ────────────────────────────────────────────────────────────
     Inner Work surface — conditioned aesthetic.
     The page is not designed in the sense of having visible design
     choices; it's prepared in the sense that a room can be prepared.
     ──────────────────────────────────────────────────────────── */

  .root {
    --inner-bg-warm: oklch(98.5% 0.008 85);
    --inner-bg-cool: oklch(97.8% 0.006 250);
    --inner-ink: oklch(22% 0.015 250);
    --inner-ink-muted: oklch(45% 0.012 250);
    --inner-ink-faint: oklch(70% 0.010 250);
    --inner-rule: oklch(75% 0.008 250 / 0.4);
    --inner-witness-rule: oklch(60% 0.04 250 / 0.55);
    --inner-caret: oklch(55% 0.15 250);
    --inner-selection: oklch(85% 0.04 250 / 0.45);
    --inner-focus: oklch(70% 0.04 250);
    --inner-grain-blend: multiply;
    --inner-grain-opacity: 0.025;

    position: absolute;
    inset: 0;
    overflow: hidden;
    color: var(--inner-ink);
    background: oklch(98% 0.006 250);
    font-family: var(--inner-font-sans);
    font-size: clamp(1.0625rem, 1.5vw + 0.5rem, 1.25rem);
    line-height: 1.7;
    letter-spacing: -0.005em;
    text-rendering: optimizeLegibility;
    font-feature-settings: "ss01", "calt", "liga";
    -webkit-font-smoothing: antialiased;
    -moz-osx-font-smoothing: grayscale;
  }

  @media (prefers-color-scheme: dark) {
    .root {
      --inner-bg-warm: oklch(17% 0.012 250);
      --inner-bg-cool: oklch(15% 0.008 280);
      --inner-ink: oklch(86% 0.015 250);
      --inner-ink-muted: oklch(62% 0.012 250);
      --inner-ink-faint: oklch(45% 0.010 250);
      --inner-rule: oklch(50% 0.012 250 / 0.4);
      --inner-witness-rule: oklch(75% 0.06 250 / 0.55);
      --inner-caret: oklch(75% 0.12 250);
      --inner-selection: oklch(40% 0.06 250 / 0.55);
      --inner-focus: oklch(60% 0.04 250);
      --inner-grain-blend: screen;
      --inner-grain-opacity: 0.04;
      background: oklch(15% 0.008 270);
    }
  }

  .field {
    position: fixed;
    inset: -10%;
    background: radial-gradient(
      ellipse 120% 80% at 30% 0%,
      var(--inner-bg-warm) 0%,
      var(--inner-bg-cool) 100%
    );
    pointer-events: none;
    z-index: 0;
  }

  @media (prefers-reduced-motion: no-preference) {
    .field {
      animation: breathe 240s ease-in-out infinite alternate;
    }
  }

  @keyframes breathe {
    from {
      transform: rotate(0deg) scale(1);
      filter: hue-rotate(0deg);
    }
    to {
      transform: rotate(8deg) scale(1.02);
      filter: hue-rotate(-6deg);
    }
  }

  .grain {
    position: fixed;
    inset: 0;
    pointer-events: none;
    opacity: var(--inner-grain-opacity);
    mix-blend-mode: var(--inner-grain-blend);
    background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 240 240'><filter id='n'><feTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='2' stitchTiles='stitch'/><feColorMatrix values='0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.5 0'/></filter><rect width='100%25' height='100%25' filter='url(%23n)'/></svg>");
    background-size: 240px 240px;
    z-index: 1;
  }

  .root.threshold .scroller {
    opacity: 0;
  }

  /* ── Scroller layer ─────────────────────────────────────────
     The whole document scrolls within this layer; the field +
     grain stay fixed to the viewport so the gradient reads as a
     stable atmosphere. */
  .scroller {
    position: relative;
    z-index: 2;
    height: 100%;
    overflow-y: auto;
    scroll-padding-bottom: 5rem;
    opacity: 1;
    transition: opacity 1200ms ease-out;
  }

  .content {
    max-width: 64ch;
    margin: 0 auto;
    padding: clamp(4rem, 12vh, 9rem) clamp(1.25rem, 4vw, 2rem) 6rem;
  }

  /* The dateline doubles as the entrance to the past-entries drawer:
     a date is the only handle this surface gives the user, so the
     date is what they click to reach other dates. The button styling
     is intentionally invisible — only on hover does a faint backdrop
     hint that it's interactive. */
  .dateline {
    display: inline-flex;
    align-items: baseline;
    gap: 0.6em;
    margin: 0 0 clamp(2.5rem, 6vh, 4.5rem);
    padding: 2px 6px;
    background: transparent;
    border: 0;
    border-radius: 4px;
    color: var(--inner-ink-muted);
    font: inherit;
    font-size: 0.95em;
    font-weight: 400;
    letter-spacing: 0.005em;
    cursor: pointer;
    transition: color 220ms ease, background 220ms ease;
    /* `display:block` was the prior layout — preserve column placement
       by anchoring with `align-self` flush-left in the content column. */
    align-self: flex-start;
  }

  .dateline:hover,
  .dateline:focus-visible {
    color: var(--inner-ink);
    background: oklch(from var(--inner-bg-cool) calc(l - 0.02) c h);
    outline: none;
  }

  .dateline:focus-visible {
    box-shadow: 0 0 0 2px var(--inner-focus);
  }

  .dateline-tag {
    color: var(--inner-ink-faint);
    font-size: 0.78em;
    font-variant: small-caps;
    letter-spacing: 0.06em;
    padding: 1px 6px;
    border: 1px solid var(--inner-rule);
    border-radius: 3px;
  }

  .past-note {
    margin: 1.5em 0 0;
    color: var(--inner-ink-faint);
    font-size: 0.92em;
    font-style: italic;
  }

  .past-note kbd {
    display: inline-block;
    padding: 0 4px;
    margin: 0 1px;
    font-family: var(--inner-font-mono);
    font-size: 0.85em;
    color: var(--inner-ink-muted);
    background: oklch(from var(--inner-bg-cool) calc(l - 0.03) c h);
    border-radius: 3px;
    font-style: normal;
  }

  /* ── Past turns ─────────────────────────────────────────────
     A turn pairs the user's prose with the witness's reflection.
     Visually: the user's text reads as regular prose; the witness
     sits to its right with a thin left rule, in slightly muted
     ink — marginalia, not a turn in a conversation. */
  .turn {
    position: relative; /* anchor for the gutter echo dot */
    margin-bottom: 1.7em;
    /* Fade the witness in once when it arrives — animation runs on
       the blockquote element (see below) so the user's prose
       doesn't fade with it. */
  }

  /* ── Echo dot ───────────────────────────────────────────────
     A pencil-mark in the left gutter beside paragraphs that resonate
     with earlier writing. Sized so it's missable; the user who wants
     to follow the thread can. */
  .echo-dot {
    position: absolute;
    left: -1.4em;
    top: 0.6em;
    width: 9px;
    height: 9px;
    padding: 0;
    border-radius: 50%;
    border: 0;
    background: var(--inner-witness-rule);
    cursor: pointer;
    opacity: 0.55;
    transition: opacity 220ms ease, transform 220ms ease;
    /* Fade in once when the dot appears (after the 8–12s delay). */
    animation: echo-arrive 600ms ease-out both;
  }

  .echo-dot:hover,
  .echo-dot:focus-visible {
    opacity: 1;
    transform: scale(1.15);
    outline: none;
  }

  .echo-dot:focus-visible {
    box-shadow: 0 0 0 2px var(--inner-focus);
  }

  @keyframes echo-arrive {
    from {
      opacity: 0;
      transform: scale(0.7);
    }
    to {
      opacity: 0.55;
      transform: scale(1);
    }
  }

  .user-prose {
    margin: 0;
    white-space: pre-wrap; /* preserve newlines from the textarea */
  }

  .witness {
    margin: 0.8em 0 0;
    padding: 0 0 0 1.25em;
    border: 0;
    border-left: 1.5px solid var(--inner-witness-rule);
    color: var(--inner-ink-muted);
    font-style: italic;
    white-space: pre-wrap;
    /* Fade in once. The brief explicitly rejects token streaming:
       "performs effort and pulls focus." */
    animation: witness-arrive 700ms ease-out both;
  }

  @keyframes witness-arrive {
    from {
      opacity: 0;
      transform: translateY(2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .composing {
    margin: 0.8em 0 0;
    padding-left: 0.6em;
    height: 1em;
    display: flex;
    align-items: center;
  }

  /* Same column geometry as `.witness` so the slot doesn't shift when
     a witness reply is replaced by an error line. Color and weight
     pull back toward `--inner-ink-faint` — the error is information,
     not an alarm; the surface stays quiet. */
  .witness-error {
    margin: 0.8em 0 0;
    padding: 0 0 0 1.25em;
    border-left: 1.5px dashed var(--inner-rule);
    color: var(--inner-ink-faint);
    font-style: italic;
    font-size: 0.92em;
    animation: witness-arrive 700ms ease-out both;
  }

  .composing-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--inner-witness-rule);
    animation: composing-pulse 1700ms ease-in-out infinite;
  }

  @keyframes composing-pulse {
    0%, 100% {
      opacity: 0.25;
      transform: scale(1);
    }
    50% {
      opacity: 0.7;
      transform: scale(1.1);
    }
  }

  .column {
    display: block;
    width: 100%;
    min-height: 4em;
    background: transparent;
    border: 0;
    outline: 0;
    color: inherit;
    font: inherit;
    line-height: inherit;
    letter-spacing: inherit;
    resize: none;
    padding: 0 0 4rem;
    margin: 1.7em 0 0;
    overflow: hidden; /* auto-resize handles height */
    caret-color: var(--inner-caret);
  }

  .column::placeholder {
    color: var(--inner-ink-faint);
  }

  .column::selection,
  .user-prose::selection,
  .witness::selection {
    background: var(--inner-selection);
  }

  .column:focus-visible {
    outline: 0;
  }

  /* ── Brand-corner exit ──────────────────────────────────────
     Hover-only reveal so the surface stays bare for the typist.
     `position: absolute` (not `fixed`) so the mark is anchored to
     the inner-work layer's bounds — the viewport-anchored variant
     placed it under the always-on 60px NavRail and silently
     swallowed every click. */
  .exit-mark {
    position: absolute;
    top: 1.25rem;
    left: 1.5rem;
    z-index: 3;
    background: transparent;
    border: 0;
    padding: 6px 8px;
    color: var(--inner-ink-faint);
    font-size: 1.1rem;
    line-height: 1;
    cursor: pointer;
    opacity: 0.55;
    border-radius: 4px;
    transition: opacity 220ms ease, color 220ms ease;
  }

  .exit-mark:hover,
  .exit-mark:focus-visible {
    opacity: 1;
    color: var(--inner-ink-muted);
    outline: none;
  }

  .exit-mark:focus-visible {
    box-shadow: 0 0 0 2px var(--inner-focus);
    outline-offset: 2px;
  }

  /* ── Local indicator ────────────────────────────────────────
     Persistent. Unblinking. The user notices it once and stops.
     Anchored to the inner-work layer (not viewport) to avoid
     overlapping the NavRail — see the exit-mark comment above. */
  .local-indicator {
    position: absolute;
    bottom: 1rem;
    left: 1.5rem;
    z-index: 3;
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--inner-ink-faint);
    font-size: 0.7rem;
    letter-spacing: 0.05em;
    text-transform: lowercase;
    pointer-events: none;
    user-select: none;
  }

  .local-indicator svg {
    color: inherit;
  }

  @media (prefers-reduced-motion: reduce) {
    .scroller {
      transition: none;
    }
    .field {
      animation: none;
    }
    .witness {
      animation: none;
    }
    .composing-dot {
      animation: none;
      opacity: 0.5;
    }
    .echo-dot {
      animation: none;
    }
  }
</style>
