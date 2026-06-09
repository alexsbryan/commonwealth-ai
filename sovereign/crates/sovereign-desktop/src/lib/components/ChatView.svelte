<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { useMachine } from "@xstate/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import {
    sendMessageStream,
    resumeSession,
    cancelStream,
    getConversation,
    createConversation,
    ingestDocument,
    askDocument,
    getDocumentAsset,
    enrichListCorpora,
    enrichGetStarterQuestions,
    warmupPrimarySlot,
  } from "../api";
  import type { IngestDocumentResult } from "../api";
  import type {
    MessageEntry,
    TaskStep,
    ApprovalRequestPayload,
    UserInputRequestPayload,
    MessageChunkPayload,
    MessageCompletePayload,
    ErrorPayload,
    DocOpProgress,
    DocumentAsset,
    DocumentOperationPayload,
    InformationRequestPayload,
    MessageRefinedPayload,
    NextStepOffer,
    StarterQuestion,
  } from "../types";
  import { enrichProgressStore } from "../stores/enrichProgress.svelte";
  import { chatSeedStore } from "../stores/chatSeed.svelte";
  import StarterChips from "./StarterChips.svelte";
  import {
    interleaveStarters,
    visibleStarters,
    advanceStarterCursor,
    type StarterWithCorpus,
  } from "./starterQuestions";
  import BrandMark from "./BrandMark.svelte";
  import { MAX_TURN_MESSAGE_CHARS, OVERSIZE_MESSAGE_HINT } from "../types";
  import { WordBufferedStream } from "@sovereign/chat-ui";
  import { chatMachine } from "../machines/chat.machine";
  import { routingStore } from "../stores/routing.svelte";
  import MessageBubble from "./MessageBubble.svelte";
  import TaskProgress from "./TaskProgress.svelte";
  import ApprovalCard from "./ApprovalCard.svelte";
  import InformationRequestCard from "./InformationRequestCard.svelte";
  import InterpretationBanner from "./InterpretationBanner.svelte";
  import ClarificationCard from "./ClarificationCard.svelte";
  import NarrationChip from "./NarrationChip.svelte";
  import CorpusProgressBanner from "./CorpusProgressBanner.svelte";
  import CorpusFilterStrip from "./CorpusFilterStrip.svelte";
  import AttachmentBanner from "./AttachmentBanner.svelte";
  import DocumentPicker from "./DocumentPicker.svelte";
  import PassageContextChip from "./reading/PassageContextChip.svelte";
  import { readingSession } from "../stores/readingSession.svelte";
  import { documentIngestionStore } from "../stores/documentIngestion.svelte";

  interface Props {
    conversationId: string | null;
    taskSteps: TaskStep[];
    onClearTask: () => void;
    onOpenSettings?: () => void;
    onConversationCreated?: (id: string) => void;
  }

  let {
    conversationId,
    taskSteps,
    onClearTask,
    onOpenSettings,
    onConversationCreated,
  }: Props = $props();

  // ── Starter questions for the empty state ────────────────────
  //
  // Mined from every enriched corpus on disk. Two surfaced at a time
  // (any more felt like cognitive load and went stale fast); a small
  // "spin again" affordance below cycles the pool in pairs so the
  // user can flick through the menu without committing to any one.
  // The pool is round-robined across corpora so a user with
  // `folder-abc` + `obsidian-def` sees a mix, not five from whichever
  // one indexed first. Refetched whenever an enrichment job
  // transitions to `complete` so a freshly-built atlas flows in
  // without a manual refresh.
  // Each starter carries its source corpus_id so the StarterChips
  // each-block can key by `${corpus_id}:${atom_id}` — atom_ids
  // (`question-0001`, …) restart at 1 inside every atlas, so a
  // round-robin merge across corpora collides on the bare id and
  // crashes Svelte's keyed-each with `each_key_duplicate`. That
  // crash freezes ChatView's reactive subtree, which is what was
  // making conversation switches feel "stuck". `StarterWithCorpus` and
  // the pool math now live in `./starterQuestions` (unit-tested).

  // Larger pool that the cycle button advances through two at a time.
  // 12 keeps fetches cheap (each enrich_get_starter_questions call is
  // a small SQLite read) while still giving the user ~6 distinct
  // pairs before we loop or re-fetch.
  const STARTER_POOL_TARGET = 12;
  const STARTERS_VISIBLE = 2;

  let starterPool: StarterWithCorpus[] = $state([]);
  let starterCursor = $state(0);
  // Drives a tiny 360° rotate on the cycle button. The flag self-
  // clears via setTimeout so the animation doesn't restart while the
  // pointer still hovers.
  let starterSpinning = $state(false);
  let buildingCorporaCount = $state(0);

  let starters = $derived(
    visibleStarters(starterPool, starterCursor, STARTERS_VISIBLE),
  );

  // Used to suppress the cycle button when the pool is too small to
  // produce a fresh second pair on click (avoids the user mashing it
  // and getting the same two back).
  let canCycleStarters = $derived(
    starterPool.length > STARTERS_VISIBLE,
  );

  async function refreshStarters() {
    try {
      const corpora = await enrichListCorpora();
      if (corpora.length === 0) {
        starterPool = [];
        starterCursor = 0;
        return;
      }
      // Pull ~enough per corpus to fill the pool even when only one
      // corpus is enriched. Per-corpus fetch is cheap; the daemon
      // caches by atlas signature.
      const perCorpusTarget = Math.max(
        4,
        Math.ceil(STARTER_POOL_TARGET / corpora.length),
      );
      const perCorpus: StarterWithCorpus[][] = await Promise.all(
        corpora.map(async (c) => {
          const list = await enrichGetStarterQuestions(
            c.corpus_id,
            perCorpusTarget,
          ).catch(() => []);
          return list.map((q) => ({ ...q, corpus_id: c.corpus_id }));
        }),
      );
      // Round-robin interleave to keep the cycle order corpus-fair.
      starterPool = interleaveStarters(perCorpus, STARTER_POOL_TARGET);
      starterCursor = 0;
    } catch (e) {
      console.warn("refreshStarters failed:", e);
      starterPool = [];
      starterCursor = 0;
    }
  }

  async function cycleStarters() {
    if (starterPool.length === 0) return;
    starterSpinning = true;
    // Advance two at a time so the visible pair fully turns over.
    // If we'd wrap on the next advance, refresh in the background so
    // the third loop pulls fresh atoms rather than recycling.
    const { cursor, shouldRefresh } = advanceStarterCursor(
      starterCursor,
      starterPool.length,
      STARTERS_VISIBLE,
    );
    starterCursor = cursor;
    if (shouldRefresh) void refreshStarters();
    // Match the CSS transition (320ms) so the icon settles after the
    // chip-swap rather than mid-flight.
    window.setTimeout(() => {
      starterSpinning = false;
    }, 320);
  }

  async function pickStarter(q: StarterQuestion) {
    // Simpler than handleNextStep's session-resume path: this is a
    // fresh turn, no parent session context.
    inputText = q.text;
    await handleSend();
  }

  // Watch the enrichment progress store: when any active job flips
  // to terminal `complete`, refetch starters so the empty-state chip
  // row upgrades from excerpt-derived (pre-atlas) to atom-derived
  // (post-atlas). Also update the in-flight count so the empty
  // state can show "Building atlas · N in flight".
  let lastSeenCompletions = $state(0);
  $effect(() => {
    const allJobs = Object.values(enrichProgressStore.byJobId);
    const completed = allJobs.filter((j) => j.terminal === "complete").length;
    const building = allJobs.filter((j) => !j.terminal).length;
    buildingCorporaCount = building;
    if (completed > lastSeenCompletions) {
      lastSeenCompletions = completed;
      void refreshStarters();
    }
  });

  // Seed-question handoff: any surface (FolderDropFlow, toast action,
  // FirstCorpusFlow, SettingsPanel) can push a seed into
  // `chatSeedStore`. We consume + submit it here when the chat pane
  // is empty and idle. Gated on messages.length === 0 so an
  // in-progress conversation doesn't get hijacked.
  $effect(() => {
    const pending = chatSeedStore.pending;
    if (!pending) return;
    if (messages.length > 0) return;
    const q = chatSeedStore.consume();
    if (!q) return;
    // Defer one microtask so the store's $state writers see the
    // consume before we mutate additional state below.
    queueMicrotask(() => {
      void pickStarter(q);
    });
  });

  // ── chatMachine — owns messages, streaming, info-request state ─
  //
  // Everything in the conversation pane that used to live as separate
  // $state vars (messages, streamingMessageId, pendingInfoRequest,
  // activeConversationId) now lives as chatMachine context. Updates
  // go through `send(event)` and hit immer's produce() internally, so
  // a class of "shallow mutation doesn't propagate to consumers" bugs
  // — notably the provenance-doesn't-appear-until-chat-cycled one —
  // is structurally impossible. See docs/frontend-state.md.
  const { snapshot, send } = useMachine(chatMachine);

  // ── Purely-local UI state (no cross-component coordination) ──
  let inputText = $state("");
  let messagesContainer: HTMLDivElement;

  // Per-conversation corpus allow-list, hydrated from the
  // Conversation row each time hydrateConversation runs. `null` is
  // the sentinel "no filter — all installed corpora participate";
  // an array is an explicit subset. The CorpusFilterStrip reads this
  // and writes back through its own Tauri call. Tracked here only so
  // the strip can stay reactive across conversation switches.
  let enabledCorpora = $state<string[] | null>(null);

  // Document attachment (picker / legacy ingest). Kept local: nothing
  // else in the app reads it, and it's discarded at send time.
  let attachment = $state<{
    source: string;
    filePath: string;
    chunksCreated: number;
  } | null>(null);
  let isIngesting = $state(false);
  let showDocPicker = $state(false);
  let attachedAsset: DocumentAsset | null = $state(null);

  // Tracks whether a non-streaming API call (askDocument, searchWeb)
  // is currently in flight. Merged with the machine's streaming state
  // to produce the unified `isLoading` derived below.
  let docOpInFlight = $state(false);

  // Transient doc-progress / doc-op progress text. Not worth modelling
  // as state — it's label soup emitted by the tools layer.
  let docProgressText: string | null = $state(null);

  // WordBufferedStream prevents mid-word rendering during streaming.
  // Component-local (one instance per ChatView mount) because it holds
  // no semantic state — only output-smoothing.
  let wordBuffer = new WordBufferedStream();

  // Unified loading flag surfaced to the template. Three contributors:
  //   • `preparing` — between user click and `send_message_stream`
  //     resolving (cold-daemon round-trip; without this the surface
  //     looked frozen for seconds after a click)
  //   • `streaming` — chunks flowing into the placeholder
  //   • `docOpInFlight` — non-streaming API calls (askDocument, web)
  let isLoading = $derived(
    $snapshot.matches({ turn: "preparing" }) ||
      $snapshot.matches({ turn: "streaming" }) ||
      docOpInFlight,
  );

  // Time-to-First-Intelligence: surface the most recent narration in
  // the loading slot so the user sees a calm, specific signal of what
  // the system is doing instead of bare typing dots. The
  // NarrationChip below the bubble keeps the running history; this
  // promotes the freshest line into the indicator the user is already
  // looking at.
  let latestNarrationText = $derived(
    routingStore.narrationLog.length > 0
      ? routingStore.narrationLog[routingStore.narrationLog.length - 1].text
      : null,
  );

  // Dot-stare guard: when isLoading has been true for >400ms with NO
  // specific signal (no docProgressText, no narration), render a calm
  // placeholder in the loading slot. The runtime suppresses narration
  // below ~5s elapsed, and fast-path queries can complete with no
  // narration at all — without this, the user sees only typing dots
  // for the entire wait. Threshold tuned via TTFI harness silent-fast
  // scenario; below 400ms is imperceptible, above 600ms feels frozen.
  const PLACEHOLDER_DELAY_MS = 400;
  let placeholderActive = $state(false);
  $effect(() => {
    // Reset on any condition that should HIDE the placeholder.
    if (!isLoading || docProgressText || latestNarrationText) {
      placeholderActive = false;
      return;
    }
    // Loading, no specific signal yet — arm the timer. Cleanup
    // returned by $effect cancels it on dependency change.
    const t = setTimeout(() => {
      placeholderActive = true;
    }, PLACEHOLDER_DELAY_MS);
    return () => {
      clearTimeout(t);
    };
  });

  // Sentence-stare guard: even when the slot has a specific signal,
  // the user can stare at the same sentence for many seconds during
  // long synthesis or non-streaming fallback paths. After 3500ms with
  // no slot-text update, append " — still working"; after 7000ms,
  // " — still on it". Caps there — beyond that the diamond pulse
  // animation provides the "still alive" cue without adding false-
  // progress claims. Suspended when a clarification card is up
  // (system is genuinely waiting on the user, not crunching).
  //
  // The prior cadence (1500ms steps, ending at "taking longer than
  // usual") felt naggy because the second nudge could land less than
  // 3s into a turn — premature when the typical synthesis is 4–8s.
  // 3500ms steps + softer wording read as the system staying with the
  // user, not apologising for itself.
  const STALE_INTERVAL_MS = 3500;
  const STALE_SUFFIXES = [
    "",
    " — still working",
    " — still on it",
  ];
  let staleRotation = $state(0);
  $effect(() => {
    // Reading every dependency inside the body so Svelte tracks
    // them — any change resets the rotation counter and (if still
    // active) restarts the interval.
    const loading = isLoading;
    const dpt = docProgressText;
    const lnt = latestNarrationText;
    const ph = placeholderActive;
    const isClarifying = !!routingStore.clarification;
    staleRotation = 0;
    if (!loading) return;
    if (isClarifying) return;
    const haveSlotText = !!dpt || !!lnt || ph;
    if (!haveSlotText) return;
    const interval = setInterval(() => {
      staleRotation = Math.min(staleRotation + 1, STALE_SUFFIXES.length - 1);
    }, STALE_INTERVAL_MS);
    return () => {
      clearInterval(interval);
    };
  });
  let staleSuffix = $derived(STALE_SUFFIXES[staleRotation] ?? "");

  // Convenience snapshot accessors. Svelte 5 re-derives whenever
  // `$snapshot` changes (which is on every event send).
  let messages = $derived($snapshot.context.messages);
  let streamingMessageId = $derived($snapshot.context.streamingMessageId);
  let pendingInfoRequest = $derived($snapshot.context.pendingInfoRequest);
  let activeConversationId = $derived($snapshot.context.conversationId);

  // PR2e — size ceiling for chat-pipeline messages. When the user
  // pastes a document-sized block into the main input, disable send
  // and hint at the attached-file flow instead. Backend applies the
  // same cap; this is the UX affordance so the request never fires.
  // Attached-document flows ARE long-input-safe (map-reduce path),
  // so the check is skipped whenever an asset/attachment is present.
  let inputIsOversized = $derived(
    !attachedAsset &&
      !attachment &&
      inputText.length > MAX_TURN_MESSAGE_CHARS,
  );

  // Send-guard: when the user has explicitly muted every parent corpus
  // (selected = empty array, distinct from `null` which means "all
  // enabled"), retrieval has nothing to search. Disable Send + surface
  // an inline hint rather than letting the turn go and produce a
  // sources-empty answer. Attached-document flows bypass the chat
  // retrieval path entirely (map-reduce on the attached file), so the
  // guard is skipped when one is present.
  let allSourcesMuted = $derived(
    !attachedAsset &&
      !attachment &&
      Array.isArray(enabledCorpora) &&
      enabledCorpora.length === 0,
  );

  // PR6b — routing state is stored in a singleton (routingStore)
  // so it persists across conversation switches by default, which
  // means a clarification card / proposed banner / narration log
  // from conversation A leaks into B's view. Fix: when the chat
  // machine's conversationId changes, clear the three transient
  // regions. Firing on initial mount is safe — the dispatches are
  // idle-state no-ops when nothing is pending.
  $effect(() => {
    // Track activeConversationId so the effect re-runs on change.
    const _track = activeConversationId;
    void _track;
    routingStore.send({ type: "DISMISS_PROPOSED" });
    routingStore.send({ type: "DISMISS_CLARIFICATION" });
    routingStore.send({ type: "CLEAR_NARRATION" });
  });

  // Antifragile-routing redirect bridge. When the routing FSM
  // completes a `REDIRECT_SUBMIT`, it exposes the new assistant
  // message_id on `routingStore.lastRedirectedMessageId`. Wire that
  // into chat.machine so the chat FSM creates a placeholder bubble
  // before chunks stream in. Acknowledge-and-clear so we only fire
  // once per redirect.
  $effect(() => {
    const newId = routingStore.lastRedirectedMessageId;
    if (!newId) return;
    send({ type: "REDIRECT_STARTED", newAssistantMessageId: newId });
    routingStore.send({ type: "ACKNOWLEDGE_REDIRECT" });
  });

  // PR6 — same bridge for ClarificationCard submissions. Without
  // this, a successful Tauri `resume_session` returns a new
  // message_id, the backend emits message-chunk events against it,
  // and chat.machine drops every chunk because no placeholder
  // exists. User sees zero response even though the backend is
  // busy. REDIRECT_STARTED is semantically identical — install the
  // new placeholder, mark the prior bubble (if any) redirected —
  // so we reuse the same event.
  $effect(() => {
    const newId = routingStore.lastClarifiedMessageId;
    if (!newId) return;
    send({ type: "REDIRECT_STARTED", newAssistantMessageId: newId });
    routingStore.send({ type: "ACKNOWLEDGE_CLARIFIED" });
  });

  let unlistenChunk: UnlistenFn | null = null;
  let unlistenComplete: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;
  let unlistenDocProgress: UnlistenFn | null = null;
  let unlistenDocOp: UnlistenFn | null = null;
  let unlistenSkeletonRebuilt: UnlistenFn | null = null;
  let unlistenInfoRequest: UnlistenFn | null = null;
  let unlistenMessageRefined: UnlistenFn | null = null;

  // Sync the external `conversationId` prop into the machine. `HYDRATE`
  // loads the conversation (or resets to empty). Runs whenever the
  // parent changes the selected conversation.
  $effect(() => {
    if (conversationId !== activeConversationId) {
      loadConversation(conversationId);
    }
  });

  onMount(async () => {
    // Kick off starter-question fetch before wiring the stream
    // listeners — it's cheap (reads atoms.json from disk) and the
    // empty state reads `starters` directly.
    void refreshStarters();

    // Eagerly warm the primary chat slot so the user's first
    // turn doesn't pay the 10–90s lazy-load tax. Fire-and-forget
    // — the backend spawns the load and the user is typing while
    // it runs. Idempotent on the backend if the slot is already
    // warm. Errors are swallowed by the backend (logged, not
    // raised) so we don't surface a false-alarm on pre-setup
    // mounts where there's no inference provider yet. The
    // window-focus warmup at the Tauri level handles the
    // "user came back to the app after a long pause" case;
    // mount handles "first open of the chat surface."
    void warmupPrimarySlot();

    // Stream handlers now forward into the machine. The wordBuffer
    // stays component-local (pure output smoothing) — only flushed
    // words are sent as MESSAGE_CHUNK, so the machine never has to
    // know about buffering.
    unlistenChunk = await listen<MessageChunkPayload>(
      "message-chunk",
      (event) => {
        const p = event.payload;
        const flushed = wordBuffer.push(p.chunk);
        if (flushed !== null) {
          send({ type: "MESSAGE_CHUNK", messageId: p.message_id, text: flushed });
          scrollToBottom();
        }
      },
    );

    unlistenComplete = await listen<MessageCompletePayload>(
      "message-complete",
      (event) => {
        const p = event.payload;
        const pendingText = wordBuffer.flush();
        send({
          type: "MESSAGE_COMPLETE",
          messageId: p.message_id,
          fullText: p.full_text,
          pendingText,
          metadata: p.metadata,
        });
        docProgressText = null;
        scrollToBottom();
        // Antifragile-routing: a proposed-interpretation banner
        // persists through the turn so the user can still cheaply
        // redirect while reading. After 30s of silence we GC it
        // (matches `SESSION_RETENTION` in query_session.rs). The
        // setTimeout captures the routingStore closure, not the
        // current `proposed` — if a fresh Propose arrives in the
        // meantime, the DISMISS_PROPOSED only fires in the idle
        // state (the FSM ignores it in `pending` after a new
        // payload overwrote the slot). Safe as a late no-op.
        setTimeout(() => {
          routingStore.send({ type: "DISMISS_PROPOSED" });
        }, 30_000);
      },
    );

    unlistenError = await listen<ErrorPayload>("message-error", (event) => {
      send({ type: "MESSAGE_ERROR", error: event.payload.message });
      docProgressText = null;
    });

    // Listen for DocumentOperationTool progress (map/reduce phases).
    unlistenDocProgress = await listen<DocOpProgress>(
      "document-progress",
      (event) => {
        docProgressText = docProgressLabel(event.payload);
      },
    );

    // Listen for DocumentAssetManager progress (routing/retrieving/synthesising).
    unlistenDocOp = await listen<DocumentOperationPayload>(
      "document:operation",
      (event) => {
        docProgressText = opProgressLabel(event.payload);
      },
    );

    // Auto-heal: when a background skeleton rebuild finishes, refresh the
    // attached asset so routing decisions on subsequent turns see the new
    // skeleton + document_type.
    unlistenSkeletonRebuilt = await listen<string>(
      "document:skeleton_rebuilt",
      async (event) => {
        const rebuiltId = event.payload;
        if (attachedAsset && attachedAsset.id === rebuiltId) {
          try {
            const refreshed = await getDocumentAsset(rebuiltId);
            if (refreshed) {
              attachedAsset = refreshed;
            }
          } catch (e) {
            console.error("Failed to refresh asset after skeleton rebuild:", e);
          }
        }
      },
    );

    // Epistemic humility mode — forwarded into the machine's parallel
    // infoRequest region. Conversation-switch clearing is handled by
    // HYDRATE/RESET inside the machine, not the listener.
    unlistenInfoRequest = await listen<InformationRequestPayload>(
      "information-request",
      (event) => {
        send({ type: "INFO_REQUEST_ARRIVED", payload: event.payload });
        scrollToBottom();
      },
    );

    // Post-stream refinement. The machine's guard drops the event
    // when the conversation id has moved on (user switched chats
    // mid-refinement).
    unlistenMessageRefined = await listen<MessageRefinedPayload>(
      "message-refined",
      (event) => {
        const p = event.payload;
        send({
          type: "MESSAGE_REFINED",
          conversationId: p.conversation_id,
          messageId: p.message_id,
          newContent: p.new_content,
        });
        // Targeted scroll: the refined message may not be at the
        // bottom of the conversation (the user could be reviewing a
        // multi-turn chat and have triggered search-now on an
        // earlier turn). `scrollToBottom` would skip past the
        // updated bubble in that case. `scrollToMessage` falls
        // through to `scrollToBottom` when the element isn't
        // findable (just-deleted, hydration race, etc.).
        scrollToMessage(p.message_id);
      },
    );
  });

  onDestroy(() => {
    unlistenChunk?.();
    unlistenComplete?.();
    unlistenError?.();
    unlistenDocProgress?.();
    unlistenDocOp?.();
    unlistenSkeletonRebuilt?.();
    unlistenInfoRequest?.();
    unlistenMessageRefined?.();
  });

  function docProgressLabel(p: DocOpProgress): string {
    switch (p.type) {
      case "Resolving":
        return `Reading ${p.source ?? "document"} (${p.chunks ?? "?"} sections)\u2026`;
      case "MapStarting":
        return `Analysing document (${p.total_batches ?? "?"} sections)\u2026`;
      case "MapProgress": {
        const pct =
          p.batches_done && p.total_batches
            ? Math.round((p.batches_done / p.total_batches) * 100)
            : 0;
        return `Analysing sections\u2026 ${pct}%`;
      }
      case "ReduceStarting":
        return `Synthesising across ${p.fragments ?? "?"} fragments\u2026`;
      case "ReduceProgress":
        return `Synthesising (pass ${(p.depth ?? 0) + 1})\u2026`;
      case "Synthesising":
        return "Composing final answer\u2026";
      default:
        return "Thinking\u2026";
    }
  }

  function opProgressLabel(p: DocumentOperationPayload): string {
    switch (p.type) {
      case "Routing":
        return `${p.operation ?? "Routing"}\u2026`;
      case "Retrieving":
        return "Retrieving relevant passages\u2026";
      case "AnalysingEntity":
        return `Analysing ${p.name ?? "entity"}\u2026`;
      case "Synthesising":
        return "Synthesising response\u2026";
      default:
        return "Processing\u2026";
    }
  }

  // ── In-flight attachment persistence ──────────────────────
  //
  // A document ingest runs for minutes (embed → skeleton → RAPTOR).
  // `attachedAsset` is component-local $state, and ChatView unmounts
  // whenever the user opens Settings/Atlas/Recipe-author (App.svelte's
  // view branches). Without persistence, navigating away and back
  // dropped the attachment even though the backend ingest and the
  // singleton documentIngestionStore listener both kept running.
  //
  // We mirror the existing `chat-draft:` localStorage pattern, keyed by
  // conversation id. On reload we re-fetch the asset (so its persisted
  // state is current even after a full app restart) and re-subscribe to
  // the live progress store.
  function attachmentKey(convId: string): string {
    return `chat-attachment:${convId}`;
  }

  function persistAttachment(convId: string | null, assetId: string | null) {
    if (!convId) return; // pre-conversation attach stays in-memory only
    try {
      if (assetId) localStorage.setItem(attachmentKey(convId), assetId);
      else localStorage.removeItem(attachmentKey(convId));
    } catch {
      // Best-effort; private-mode storage failures are tolerable.
    }
  }

  /** Restore (or clear) the attachment for `targetId`. Synchronously
   *  clears any carried-over attachment, then async-fetches the stored
   *  asset. Guards against the user switching conversations mid-fetch. */
  function restoreAttachment(targetId: string) {
    let storedId: string | null = null;
    try {
      storedId = localStorage.getItem(attachmentKey(targetId));
    } catch {
      storedId = null;
    }
    if (!storedId) {
      // No attachment for this conversation — clear whatever the
      // previous conversation left attached (ChatView persists across
      // conversation switches; only a view change unmounts it).
      attachedAsset = null;
      attachment = null;
      return;
    }
    getDocumentAsset(storedId)
      .then((asset) => {
        if (targetId !== conversationId) return; // user moved on
        const failed =
          asset != null &&
          typeof asset.state === "object" &&
          "Failed" in asset.state;
        if (!asset || failed) {
          // Asset was deleted, or its ingest failed — drop the stale key.
          persistAttachment(targetId, null);
          attachedAsset = null;
          attachment = null;
          return;
        }
        attachedAsset = asset;
        attachment = {
          source: asset.title || asset.filename,
          filePath: "",
          chunksCreated: asset.chunk_count,
        };
        // Subscribe to live progress so an ingest still in flight keeps
        // advancing the banner after the round-trip back to this view.
        void documentIngestionStore.init();
      })
      .catch(() => {
        // Fetch failed — leave the (already-cleared) state as-is.
      });
  }

  async function loadConversation(targetId: string | null) {
    onClearTask();
    if (!targetId) {
      send({ type: "RESET" });
      return;
    }

    // One-shot draft hydration. Used by seeded conversations on
    // first launch — a pre-filled prompt sits in the input box
    // when the user opens the conversation, and once it's been
    // surfaced it's removed so reopening the same conversation
    // doesn't keep re-pre-filling on top of whatever the user
    // typed.
    const draftKey = `chat-draft:${targetId}`;
    const draft = (() => {
      try {
        return localStorage.getItem(draftKey);
      } catch {
        return null;
      }
    })();
    if (draft && !inputText) {
      inputText = draft;
      try {
        localStorage.removeItem(draftKey);
      } catch {
        // Best-effort; private-mode storage failures are tolerable.
      }
    }

    // Restore (or clear) any document attachment for this conversation.
    // Synchronous clear happens inside, so a carried-over attachment from
    // the previously-open conversation never lingers.
    restoreAttachment(targetId);

    // Eager clear: bind the new conversation id and empty the
    // message list synchronously, BEFORE awaiting the backend
    // fetch. Without this, switching to a conversation with a
    // slow `get_conversation` (large history, cold disk) leaves
    // the previous conversation's bubbles on screen for the
    // duration of the await — which the user perceives as the
    // chat being "stuck on the first conversation that loaded".
    // Re-HYDRATE below replaces the empty list with the real
    // messages once they arrive.
    send({ type: "HYDRATE", conversationId: targetId, messages: [] });

    try {
      const detail = await getConversation(targetId);
      // Stale-response guard: if the user has moved on while
      // this fetch was in flight, drop the response. The
      // currently-selected conversation is owned by the parent
      // prop, not by `activeConversationId` (which we just set
      // optimistically above). Without this guard a slow A
      // resolving after the user clicked B would clobber B's
      // already-hydrated content.
      if (targetId !== conversationId) return;
      send({
        type: "HYDRATE",
        conversationId: targetId,
        messages: detail.messages,
      });
      enabledCorpora = detail.enabled_corpora ?? null;
      scrollToBottom();
    } catch {
      // Fetch failed (commonly: brand-new conversation that
      // create_conversation minted but didn't persist). The
      // eager HYDRATE above already left the chat empty +
      // bound to `targetId`, so there's nothing to do.
      enabledCorpora = null;
    }
  }

  function handleAttach() {
    showDocPicker = !showDocPicker;
  }

  function handleAssetSelected(asset: DocumentAsset) {
    attachedAsset = asset;
    // Also set legacy attachment for the banner display.
    attachment = {
      source: asset.title || asset.filename,
      filePath: "",
      chunksCreated: asset.chunk_count,
    };
    // Persist so the attachment survives a round-trip through another
    // view while the ingest is still running. For a brand-new chat with
    // no id yet, this is a no-op — ensureConversation persists it once
    // the id is minted on first send.
    persistAttachment(activeConversationId, asset.id);
    showDocPicker = false;
  }

  // Legacy attach for files ingested via the old path (kept for backward compat).
  async function handleLegacyAttach(filePath: string) {
    isIngesting = true;
    try {
      const result = await ingestDocument(filePath);
      attachment = {
        source: result.source,
        filePath,
        chunksCreated: result.chunks_created,
      };
    } catch (e) {
      console.error("Failed to ingest document:", e);
    } finally {
      isIngesting = false;
    }
  }

  /** Ensure there's an active conversation before sending. Returns the
   *  id. If none exists we create one and bind it to the current turn
   *  via CONVERSATION_BOUND (which preserves any messages already on
   *  screen — HYDRATE would wipe an optimistically-pushed user bubble). */
  async function ensureConversation(): Promise<string> {
    if (activeConversationId) return activeConversationId;
    const created = await createConversation();
    send({ type: "CONVERSATION_BOUND", conversationId: created.id });
    onConversationCreated?.(created.id);
    // If a document was attached before the conversation had an id,
    // persist it now under the freshly-minted id so it survives a
    // later view round-trip.
    if (attachedAsset) persistAttachment(created.id, attachedAsset.id);
    return created.id;
  }

  async function handleSend() {
    let text = inputText.trim();
    if (!text || isLoading) return;

    // ── Document asset path (non-streaming) ─────────────────
    // When a DocumentAsset is attached, route through the
    // DocumentAssetManager (ask_document). Returns a fully-formed
    // assistant message rather than streaming chunks, so we forward
    // it as a single ASSISTANT_MESSAGE_RECEIVED event.
    if (attachedAsset) {
      const asset = attachedAsset;
      const convoId = await ensureConversation();

      send({
        type: "ASSISTANT_MESSAGE_RECEIVED",
        message: {
          id: crypto.randomUUID(),
          role: "user",
          content: text,
          created_at: Math.floor(Date.now() / 1000),
        },
      });
      inputText = "";
      attachment = null;
      docOpInFlight = true;
      onClearTask();
      scrollToBottom();

      try {
        const result = await askDocument(asset.id, text, convoId);
        send({
          type: "ASSISTANT_MESSAGE_RECEIVED",
          message: {
            id: crypto.randomUUID(),
            role: "assistant",
            content: result.response,
            created_at: Math.floor(Date.now() / 1000),
            metadata: { operation: result.operation, sources: result.sources },
          },
        });
      } catch (e) {
        send({
          type: "ASSISTANT_MESSAGE_RECEIVED",
          message: {
            id: crypto.randomUUID(),
            role: "assistant",
            content: `Error: ${e}`,
            created_at: Math.floor(Date.now() / 1000),
          },
        });
      } finally {
        docOpInFlight = false;
        docProgressText = null;
        scrollToBottom();
      }
      return;
    }

    // ── Legacy attachment path (text prefix) ────────────────
    if (attachment && !attachedAsset) {
      text = `[Document attached: ${attachment.source}]\n\n${text}`;
    }

    // ── Streaming path ──────────────────────────────────────
    // SEND_INITIATED appends the user bubble and flips the FSM into
    // `preparing` — `isLoading` goes true RIGHT NOW, before any
    // bridge await. This is the lock-down for the "60s blank window"
    // bug: if `create_conversation` or `send_message_stream` is slow
    // (cold daemon, mesh handoff, etc.), the user still sees their
    // message + the typing indicator immediately. SEND_START fires
    // later with the real assistant message id.
    const userMsg: MessageEntry = {
      id: crypto.randomUUID(),
      role: "user",
      content: text,
      created_at: Math.floor(Date.now() / 1000),
    };
    send({ type: "SEND_INITIATED", userMessage: userMsg });
    inputText = "";
    attachment = null;
    onClearTask();
    scrollToBottom();

    try {
      // Antifragile-routing: flush the prior turn's narration log
      // so the new turn starts clean. Any proposed-interpretation
      // banner from a prior turn stays until its own 30s GC fires
      // or the user redirects — those states are per-session and
      // shouldn't be conflated with narration, which is per-turn.
      routingStore.send({ type: "CLEAR_NARRATION" });
      // PR6 — if a clarification card is still open when the user
      // types a fresh message in the main input, dismiss it. Without
      // this, the card lingers over an unrelated new turn and the
      // user gets a confusing "which query am I answering?" state.
      if (routingStore.clarification) {
        routingStore.send({ type: "DISMISS_CLARIFICATION" });
      }

      const convoId = await ensureConversation();
      // Glass-box reading-surface handoff: when the user has
      // focused a passage, attach it as scoped context so the
      // librarian's answer scopes to what's open. Focus persists
      // across turns until the user clears it or closes the
      // surface, supporting "let's discuss this passage" flows.
      const focused = readingSession.focusedPassage;
      const contextChunks = focused
        ? [{ corpus_id: focused.corpusId, chunk_id: focused.chunkId }]
        : undefined;
      const started = await sendMessageStream(text, convoId, contextChunks);
      wordBuffer.reset();
      send({ type: "SEND_START", assistantMessageId: started.message_id });
      scrollToBottom();
      // Streaming continues via MESSAGE_CHUNK / MESSAGE_COMPLETE.
    } catch (e) {
      // create_conversation or send_message_stream threw before any
      // stream began. SEND_FAILED appends a stand-alone error bubble
      // and returns to idle — the user message we already pushed
      // stays.
      send({ type: "SEND_FAILED", error: String(e) });
      scrollToBottom();
    }
  }

  /** PR6 — abort the in-flight stream. Calls the Tauri command
   *  which cancels the session's cancel-token; the sampler breaks,
   *  the stream closes, message-complete fires naturally, and
   *  chat.machine transitions back to idle. No explicit
   *  chat.machine event — the existing complete-path handles it. */
  async function handleStop() {
    const convoId = activeConversationId;
    if (!convoId) return;
    try {
      await cancelStream(convoId);
    } catch (e) {
      console.warn("cancelStream failed:", e);
      // Belt-and-braces: tell chat.machine to bail to idle so the
      // UI recovers even if the Tauri call errored.
      send({ type: "MESSAGE_ERROR", error: "cancelled" });
    }
  }

  /** PR3 — next-step offer click. Mirrors `handleSend`'s streaming
   *  path but uses `resumeSession` when the offer carries a live
   *  session_ref (so the runtime skips reclassification and reuses
   *  the parent session's context). Falls back to
   *  `sendMessageStream` if the session is gone (expired / unknown)
   *  so the click never silently fails. */
  async function handleNextStep(offer: NextStepOffer) {
    if (isLoading) return;
    const text = offer.follow_up_query;
    if (!text) return;

    // Same optimistic-dispatch shape as handleSend: SEND_INITIATED
    // before any bridge await so the user sees feedback instantly,
    // SEND_START after the stream id resolves.
    const userMsg: MessageEntry = {
      id: crypto.randomUUID(),
      role: "user",
      content: text,
      created_at: Math.floor(Date.now() / 1000),
    };
    send({ type: "SEND_INITIATED", userMessage: userMsg });
    onClearTask();
    scrollToBottom();

    try {
      routingStore.send({ type: "CLEAR_NARRATION" });

      const convoId = await ensureConversation();
      const tryResume =
        offer.session_ref && offer.intent_hint
          ? await resumeSession(
              text,
              convoId,
              offer.session_ref,
              offer.intent_hint,
            ).catch((err: unknown) => {
              // 30s session GC already fired (or server restarted)
              // — fall back to a fresh turn. Log for glassbox but
              // don't surface a hard error; the user's click still
              // succeeds, it just re-classifies.
              console.info("resumeSession failed, falling back", err);
              return null;
            })
          : null;
      // Same focused-passage handoff as handleSend — the next-step
      // offer is "still about this passage" by default.
      const focused = readingSession.focusedPassage;
      const contextChunks = focused
        ? [{ corpus_id: focused.corpusId, chunk_id: focused.chunkId }]
        : undefined;
      const started =
        tryResume ?? (await sendMessageStream(text, convoId, contextChunks));

      wordBuffer.reset();
      send({ type: "SEND_START", assistantMessageId: started.message_id });
      scrollToBottom();
    } catch (e) {
      send({ type: "SEND_FAILED", error: String(e) });
      scrollToBottom();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  /// Resume a length-truncated reply. Move C of the cutoff-legibility
  /// trio (A: surface finish_reason, B: tell model its budget, C:
  /// Continue affordance). We re-prompt as a fresh turn — the model
  /// has the prior assistant text in its conversation history, so a
  /// short imperative is enough to pick up where it stopped. We
  /// deliberately don't try to splice the new content into the prior
  /// bubble; making the resumption a new message keeps the audit
  /// trail honest (the user can see exactly what was generated where).
  async function handleContinueFromCutoff() {
    if (isLoading) return;
    inputText =
      "Continue from where you left off in the previous response. " +
      "Pick up mid-sentence if needed — don't restart from the top.";
    await handleSend();
  }

  function scrollToBottom() {
    requestAnimationFrame(() => {
      if (messagesContainer) {
        messagesContainer.scrollTop = messagesContainer.scrollHeight;
      }
    });
  }

  /// Scroll a specific assistant bubble into view by its message id.
  /// Used after MESSAGE_REFINED so the user sees the updated content
  /// even when the refined message isn't the last one in the chat
  /// (e.g. they triggered search-now on an earlier turn after
  /// scrolling up). Two rAFs deep: the first waits for Svelte to
  /// re-render the new content (the {#key content} block in
  /// AssistantMessage tears down + remounts the prose subtree on
  /// content swap), the second runs after the new DOM has laid out
  /// so scrollIntoView lands on the final position.
  ///
  /// Falls through to `scrollToBottom` when the element isn't in
  /// the DOM (just-deleted, conversation switch race, etc.).
  function scrollToMessage(id: string) {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        const el = document.querySelector(`[data-message-id="${id}"]`);
        if (el) {
          el.scrollIntoView({ behavior: "smooth", block: "center" });
        } else {
          scrollToBottom();
        }
      });
    });
  }
</script>

<div class="chat-view">
  <CorpusProgressBanner {onOpenSettings} />
  <div class="messages" bind:this={messagesContainer}>
    {#if messages.length === 0 && !isLoading}
      <div class="empty-state">
        <div class="empty-glow"></div>
        <div class="empty-mark">
          <BrandMark size={88} />
        </div>
        <h2>SOVEREIGN</h2>
        <p class="empty-sub">ai for the rest of us</p>

        {#if starters.length > 0}
          <div class="empty-starters">
            <div class="starters-header">
              <span class="starters-label">Suggestions?</span>
              {#if canCycleStarters}
                <button
                  type="button"
                  class="starters-cycle"
                  class:spinning={starterSpinning}
                  onclick={cycleStarters}
                  title="Shuffle suggestions"
                  aria-label="Shuffle suggestions"
                >
                  <!-- Inline so the rotation animates the icon itself,
                       not a child element. 14×14 keeps the affordance
                       subtle alongside the heading. -->
                  <svg width="14" height="14" viewBox="0 0 16 16" fill="none">
                    <path
                      d="M12.5 4.5A5.5 5.5 0 1 0 14 8"
                      stroke="currentColor"
                      stroke-width="1.4"
                      stroke-linecap="round"
                    />
                    <path
                      d="M13 2v3h-3"
                      stroke="currentColor"
                      stroke-width="1.4"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                </button>
              {/if}
            </div>
            <StarterChips questions={starters} onPick={pickStarter} />
          </div>
        {/if}
        {#if buildingCorporaCount > 0}
          <p class="empty-building">
            Building atlas · {buildingCorporaCount} in flight
            <span class="empty-building-hint">
              — questions will improve once the atlas is ready.
            </span>
          </p>
        {/if}
      </div>
    {:else}
      {#each messages as msg (msg.id)}
        <MessageBubble
          role={msg.role}
          content={msg.content}
          metadata={msg.metadata}
          messageId={msg.id}
          conversationId={activeConversationId ?? ""}
          isStreaming={msg.id === streamingMessageId}
          refining={msg.refining}
          searchAugmentation={msg.searchAugmentation}
          onNextStep={handleNextStep}
          onContinue={handleContinueFromCutoff}
        />
      {/each}

      <TaskProgress steps={taskSteps} />

      <ApprovalCard />

      <InformationRequestCard
        request={pendingInfoRequest}
        conversationId={activeConversationId}
        onHandled={() => send({ type: "CLEAR_INFO" })}
        onRefiningStarted={() => {
          // The to-be-refined message is the most recent COMPLETED
          // assistant bubble. Skip any in-flight streaming bubble
          // (refinement is a post-stream concept) — the same
          // discipline MESSAGE_REFINED's guard enforces.
          const target = [...messages]
            .reverse()
            .find(
              (m) =>
                m.role === "assistant" && m.id !== streamingMessageId,
            );
          if (target) send({ type: "MESSAGE_REFINING", messageId: target.id });
        }}
        onSearchAugmented={(augmentation) => {
          const target = [...messages]
            .reverse()
            .find(
              (m) =>
                m.role === "assistant" && m.id !== streamingMessageId,
            );
          if (target) {
            send({
              type: "SEARCH_AUGMENTED",
              messageId: target.id,
              augmentation,
            });
          }
        }}
      />

      <!-- Antifragile-routing UI. All three read from `routingStore`
           and render only when the FSM context has a live payload;
           when empty they render nothing. -->
      <InterpretationBanner />
      <ClarificationCard />
      <NarrationChip />

      {#if isLoading}
        {#if docProgressText}
          <div class="doc-progress-indicator" aria-label="Sovereign is processing document">
            <span class="progress-mark pulse">{"\u25C8"}</span>
            <span class="progress-text">{docProgressText}{staleSuffix}</span>
          </div>
        {:else if latestNarrationText}
          <div
            class="doc-progress-indicator"
            data-source="narration"
            aria-label="Sovereign is working"
          >
            <span class="progress-mark pulse">{"\u25C8"}</span>
            <span class="progress-text">{latestNarrationText}{staleSuffix}</span>
          </div>
        {:else if placeholderActive}
          <div
            class="doc-progress-indicator"
            data-source="placeholder"
            aria-label="Sovereign is working"
          >
            <span class="progress-mark pulse">{"\u25C8"}</span>
            <span class="progress-text">Working on it&hellip;{staleSuffix}</span>
          </div>
        {:else}
          <div class="typing-indicator" aria-label="Sovereign is responding">
            <span></span><span></span><span></span>
          </div>
        {/if}
      {/if}
    {/if}
  </div>

  <!-- Persistent corpus picker — between the messages list and the
       input area so the user can mute Wikipedia mid-thread without
       leaving the chat. Re-uses CorpusFilterStrip (also rendered in
       the empty state) so the visual + state model is identical from
       first paint through every turn. -->
  <CorpusFilterStrip
    conversationId={activeConversationId ?? null}
    initialEnabled={enabledCorpora}
    ensureConversation={ensureConversation}
    onChange={(next) => (enabledCorpora = next)}
  />

  <div class="input-area">
    <PassageContextChip />
    {#if attachedAsset}
      <AttachmentBanner
        filename={attachedAsset.title || attachedAsset.filename}
        chunksCreated={attachedAsset.chunk_count}
        assetId={attachedAsset.id}
        initialState={attachedAsset.state}
        onremove={() => {
          persistAttachment(activeConversationId, null);
          attachedAsset = null;
          attachment = null;
        }}
      />
    {:else if attachment}
      <AttachmentBanner
        filename={attachment.source}
        chunksCreated={attachment.chunksCreated}
        onremove={() => (attachment = null)}
      />
    {/if}
    <div class="input-row">
    <button
      class="attach-btn"
      onclick={handleAttach}
      disabled={isLoading || isIngesting}
      title={isIngesting ? "Ingesting document..." : "Attach a document"}
    >
      {#if isIngesting}
        <span class="attach-spinner"></span>
      {:else}
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
          <path d="M14 8.5l-5.6 5.6a3.5 3.5 0 01-5-5L9 3.5a2.5 2.5 0 013.5 3.5L7 12.5a1.5 1.5 0 01-2.1-2.1L10.3 5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
        </svg>
      {/if}
    </button>
    <textarea
      bind:value={inputText}
      placeholder={attachedAsset ? `Ask about ${attachedAsset.title || attachedAsset.filename}...` : attachment ? `Ask about ${attachment.source}...` : "Type a message..."}
      onkeydown={handleKeydown}
      rows="1"
      disabled={isLoading}
    ></textarea>
    {#if isLoading}
      <!-- PR6 — Stop button replaces Send while a stream is in
           flight. Gives the user a visible way to bail on a turn
           that's taking forever (e.g., a Primary synthesis that
           pulled in too much context). -->
      <button
        class="stop-btn"
        type="button"
        onclick={handleStop}
        title="Stop this turn"
      >
        Stop
      </button>
    {:else}
      <button
        class="send-btn"
        onclick={handleSend}
        disabled={!inputText.trim() || inputIsOversized || allSourcesMuted}
        title={inputIsOversized
          ? OVERSIZE_MESSAGE_HINT
          : allSourcesMuted
            ? "Enable at least one source to ask a question."
            : ""}
      >
        Send
      </button>
    {/if}
    </div>
    {#if allSourcesMuted}
      <div class="oversize-hint" role="status">
        <span class="oversize-mark">!</span>
        <span>Enable at least one source above to ask a question.</span>
      </div>
    {/if}
    {#if inputIsOversized}
      <div class="oversize-hint" role="status">
        <span class="oversize-mark">!</span>
        <span>{OVERSIZE_MESSAGE_HINT}</span>
        <button
          class="oversize-attach-btn"
          onclick={handleAttach}
          disabled={isIngesting}
        >
          Attach file instead
        </button>
      </div>
    {/if}
  </div>

  {#if showDocPicker}
    <DocumentPicker
      onSelect={handleAssetSelected}
      onClose={() => (showDocPicker = false)}
    />
  {/if}
</div>

<style>
  .chat-view {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  /* ── Messages ── */
  .messages {
    flex: 1;
    overflow-y: auto;
    padding: 24px 32px 16px;
    display: flex;
    flex-direction: column;
    /* Isolate from the input row below — typing into the textarea
       must not trigger a paint cycle over the entire message
       column. */
    contain: layout paint style;
  }

  /* ── Empty state ── */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    text-align: center;
    position: relative;
    gap: 0;
  }

  .empty-glow {
    position: absolute;
    width: 380px;
    height: 300px;
    border-radius: 50%;
    background: radial-gradient(
      ellipse at 50% 50%,
      rgba(155, 135, 196, 0.09) 0%,
      rgba(201, 168, 76,  0.04) 45%,
      transparent 70%
    );
    pointer-events: none;
  }

  .empty-mark {
    /* Wrapper for the BrandMark SVG. Holds the breathe animation +
       drop-shadow halo that the bare SVG doesn't need to know about. */
    display: inline-flex;
    line-height: 1;
    margin-bottom: 22px;
    animation: empty-breathe 3.5s ease-in-out infinite;
    position: relative;
    filter: drop-shadow(0 0 18px rgba(201, 168, 76, 0.35));
  }

  .empty-state h2 {
    font-family: var(--font-mono);
    font-size: 1.25rem;
    font-weight: 600;
    letter-spacing: 0.36em;
    color: var(--text-secondary);
    margin-bottom: 14px;
    position: relative;
  }

  .empty-sub {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    color: var(--text-muted);
    letter-spacing: 0.18em;
    text-transform: uppercase;
    margin-bottom: 36px;
    position: relative;
  }

  /* ── Starter chips + build indicator in empty state ── */
  .empty-starters {
    margin-top: 8px;
    max-width: 640px;
    text-align: left;
    position: relative;
  }
  .starters-header {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 14px;
    /* A faint hairline running through the heading row anchors the
       label + cycle button as a horizon line above the chips. Keeps
       the gold accent button feeling load-bearing rather than
       decorative. */
    padding-left: 2px;
  }
  .starters-label {
    font-size: 0.74rem;
    text-transform: uppercase;
    letter-spacing: 0.18em;
    color: var(--text-secondary, var(--text-primary));
    font-family: var(--font-mono);
  }
  /* Subtle shuffle affordance. Rests with a faint amethyst frame so
     it reads as "there's something here" — the previous transparent-
     until-hover version disappeared on the dark surface. Hover
     surfaces gold; click spins the icon 360°. */
  .starters-cycle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    border: 1px solid var(--border-bright);
    /* Sharp pill no more — match the terminal geometry of every
       chrome element. */
    border-radius: 2px;
    background: var(--lavender-glow);
    color: var(--text-secondary);
    cursor: pointer;
    transition:
      color 180ms ease,
      background 180ms ease,
      border-color 180ms ease,
      transform 180ms ease,
      box-shadow 180ms ease;
  }
  .starters-cycle:hover,
  .starters-cycle:focus-visible {
    color: var(--accent-light);
    background: var(--accent-dim);
    border-color: var(--accent);
    box-shadow: 0 0 10px rgba(201, 168, 76, 0.25);
    outline: none;
  }
  .starters-cycle:active {
    transform: scale(0.92);
  }
  .starters-cycle svg {
    transition: transform 360ms cubic-bezier(0.4, 1.4, 0.5, 1);
  }
  .starters-cycle.spinning svg {
    transform: rotate(360deg);
  }
  .empty-building {
    margin-top: 16px;
    font-size: 0.72rem;
    color: var(--text-muted);
    letter-spacing: 0.02em;
    font-family: var(--font-mono);
    position: relative;
  }
  .empty-building-hint {
    color: var(--text-muted);
    margin-left: 4px;
    font-style: italic;
  }

  @keyframes empty-breathe {
    0%, 100% {
      filter: drop-shadow(0 0 10px rgba(201, 168, 76, 0.38));
    }
    50% {
      filter: drop-shadow(0 0 24px rgba(201, 168, 76, 0.65));
    }
  }

  /* ── Input area ── */
  .input-area {
    display: flex;
    flex-direction: column;
    /* More vertical air than the previous 12/16 — the textarea +
       button row needs to look like a deliberate console, not a
       compressed footer. */
    padding: 16px 24px 20px;
    /* No border-top here — the CorpusFilterStrip sitting directly
       above owns the single hairline that separates the chrome
       bundle (strip + input-area) from the messages list. Two
       borders stacked read as a double-rule and broke the
       "deliberate console" silhouette. */
    background: var(--bg-secondary);
    /* Paint containment — tells the browser this subtree's
       layout/style/paint cannot affect the messages column above
       and vice versa. Without it WebKitGTK invalidates a much
       larger area on every keystroke than it needs to. The
       contain spec excludes `size` so the row still flexes its
       parent to fit growing content. */
    contain: layout paint style;
  }

  .input-row {
    display: flex;
    gap: 10px;
    align-items: flex-end;
    /* Same reasoning — keystrokes only invalidate this row, not
       the surrounding banners or hints. */
    contain: layout paint style;
  }

  /* All input-row chrome buttons share one square footprint so the
     row reads as a single aligned baseline regardless of which
     buttons happen to be visible (attach / search / insights / send /
     stop). Setting both `height` and `min-height` is load-bearing —
     `align-self: flex-end` with mismatched intrinsic heights was the
     source of the visible misalignment. */
  .attach-btn,
  .send-btn,
  .stop-btn {
    height: 42px;
    min-height: 42px;
    box-sizing: border-box;
    align-self: flex-end;
  }

  .attach-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 42px;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    /* 2px corners across all chrome — reads as terminal, not pill. */
    border-radius: 2px;
    color: var(--text-muted);
    cursor: pointer;
    flex-shrink: 0;
    transition: border-color 0.15s, color 0.15s, background 0.15s;
  }
  .attach-btn:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
    background: var(--accent-glow);
  }
  .attach-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .attach-spinner {
    width: 12px;
    height: 12px;
    border: 2px solid var(--text-muted);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  textarea {
    flex: 1;
    /* Padding tuned so single-line content lands at exactly 42px
       total (matches the chrome buttons). Math: content
       0.88rem * line-height 1.5 ≈ 21px + 2 × 9px padding + 2 × 1px
       border = 41px box-sizing border-box. The 1px slack lives in
       the line-box rounding — visually flush with the button row. */
    padding: 9px 14px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    /* Hard corner matches the chrome buttons. */
    border-radius: 2px;
    resize: none;
    outline: none;
    height: 42px;
    min-height: 42px;
    max-height: 120px;
    line-height: 1.5;
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: 0.84rem;
    transition: border-color 0.2s, box-shadow 0.2s;
  }

  textarea::placeholder {
    color: var(--text-muted);
  }

  textarea:focus {
    border-color: color-mix(in srgb, var(--accent) 50%, transparent);
    box-shadow: 0 0 0 2px var(--accent-glow);
  }

  .send-btn {
    padding: 0 24px;
    background: var(--accent);
    color: var(--bg-root);
    border: 1px solid var(--accent);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-weight: 600;
    font-size: 0.78rem;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    transition: background 0.2s, box-shadow 0.2s, transform 0.15s,
                border-color 0.2s;
  }

  .send-btn:hover:not(:disabled) {
    background: var(--accent-light);
    border-color: var(--accent-light);
    box-shadow: 0 0 22px var(--accent-dim);
    transform: translateY(-1px);
  }

  .send-btn:active:not(:disabled) {
    transform: translateY(0);
  }

  .send-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  /* ── Stop button (PR6) ── */
  .stop-btn {
    padding: 0 24px;
    background: transparent;
    color: var(--text-primary);
    border: 1px solid var(--text-primary);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-weight: 600;
    font-size: 0.78rem;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }
  .stop-btn:hover {
    background: var(--text-primary);
    color: var(--bg-primary);
  }

  /* ── Oversize input warning (PR2e) ── */
  .oversize-hint {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 8px;
    padding: 10px 14px;
    background: var(--bg-secondary);
    border: 1px solid var(--border-mid);
    border-left: 2px solid var(--accent);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 0.76rem;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    line-height: 1.5;
  }
  .oversize-mark {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    flex-shrink: 0;
    border-radius: 50%;
    background: var(--accent);
    color: var(--bg-primary);
    font-weight: 700;
    font-size: 0.8rem;
  }
  .oversize-attach-btn {
    margin-left: auto;
    flex-shrink: 0;
    padding: 5px 12px;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    background: transparent;
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 2px;
    cursor: pointer;
  }
  .oversize-attach-btn:hover:not(:disabled) {
    background: var(--accent);
    color: var(--bg-primary);
  }
  .oversize-attach-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* ── Typing indicator ── */
  .typing-indicator {
    display: flex;
    gap: 5px;
    padding: 4px 0 4px 16px;
    align-self: flex-start;
    border-left: 2px solid color-mix(in srgb, var(--lavender) 30%, transparent);
    margin-bottom: 12px;
  }

  .typing-indicator span {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--lavender);
    animation: typing-pulse 1.3s ease-in-out infinite;
  }

  .typing-indicator span:nth-child(2) {
    animation-delay: 0.2s;
  }

  .typing-indicator span:nth-child(3) {
    animation-delay: 0.4s;
  }

  .doc-progress-indicator {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 0 6px 16px;
    align-self: flex-start;
    border-left: 2px solid color-mix(in srgb, var(--accent) 40%, transparent);
    margin-bottom: 12px;
    font-size: 12px;
    color: var(--text-secondary);
    animation: fade-in 0.3s ease;
  }

  .progress-mark {
    color: var(--accent);
    font-size: 13px;
  }

  /* Calming pulse on the diamond accent — even when the slot text is
     static, the indicator visibly breathes so the user knows the
     system is still active. Slow + subtle so it doesn't compete
     with content for attention. */
  .progress-mark.pulse {
    display: inline-block;
    animation: progress-mark-breathe 2.4s ease-in-out infinite;
  }
  @keyframes progress-mark-breathe {
    0%, 100% { opacity: 0.55; }
    50% { opacity: 1; }
  }

  @keyframes fade-in {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  @keyframes typing-pulse {
    0%, 80%, 100% {
      transform: scale(0.55);
      opacity: 0.35;
    }
    40% {
      transform: scale(1);
      opacity: 1;
    }
  }
</style>
