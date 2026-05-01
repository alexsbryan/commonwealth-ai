<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { useMachine } from "@xstate/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import {
    sendMessageStream,
    resumeSession,
    cancelStream,
    searchWeb,
    getConversation,
    createConversation,
    listCorpora,
    ingestDocument,
    askDocument,
    getDocumentAsset,
    enrichListCorpora,
    enrichGetStarterQuestions,
  } from "../api";
  import type { IngestDocumentResult } from "../api";
  import type {
    MessageEntry,
    TaskStep,
    ApprovalRequestPayload,
    UserInputRequestPayload,
    CorpusEntry,
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
  import { MAX_TURN_MESSAGE_CHARS, OVERSIZE_MESSAGE_HINT } from "../types";
  import { WordBufferedStream } from "../stream-buffer";
  import { chatMachine } from "../machines/chat.machine";
  import { insightStore } from "../stores/insights.svelte";
  import { routingStore } from "../stores/routing.svelte";
  import MessageBubble from "./MessageBubble.svelte";
  import TaskProgress from "./TaskProgress.svelte";
  import ApprovalCard from "./ApprovalCard.svelte";
  import InformationRequestCard from "./InformationRequestCard.svelte";
  import InterpretationBanner from "./InterpretationBanner.svelte";
  import ClarificationCard from "./ClarificationCard.svelte";
  import NarrationChip from "./NarrationChip.svelte";
  import CorpusProgressBanner from "./CorpusProgressBanner.svelte";
  import AttachmentBanner from "./AttachmentBanner.svelte";
  import DocumentPicker from "./DocumentPicker.svelte";

  interface Props {
    conversationId: string | null;
    taskSteps: TaskStep[];
    onClearTask: () => void;
    onOpenSettings?: () => void;
    onToggleInsights?: () => void;
    onConversationCreated?: (id: string) => void;
  }

  let {
    conversationId,
    taskSteps,
    onClearTask,
    onOpenSettings,
    onToggleInsights,
    onConversationCreated,
  }: Props = $props();

  // ── Starter questions for the empty state ────────────────────
  //
  // Mined from every enriched corpus on disk. Round-robins across
  // corpora so a user with `folder-abc` + `obsidian-def` sees a mix,
  // not five from whichever one indexed first. Refetched whenever an
  // enrichment job transitions to `complete` so a freshly-built atlas
  // flows into the empty state without a refresh.
  let starters: StarterQuestion[] = $state([]);
  let buildingCorporaCount = $state(0);

  async function refreshStarters() {
    try {
      const corpora = await enrichListCorpora();
      if (corpora.length === 0) {
        starters = [];
        return;
      }
      const perCorpus: StarterQuestion[][] = await Promise.all(
        corpora.map((c) =>
          enrichGetStarterQuestions(c.corpus_id, 3).catch(() => []),
        ),
      );
      // Round-robin interleave up to 5.
      const picked: StarterQuestion[] = [];
      let idx = 0;
      while (picked.length < 5 && perCorpus.some((p) => p.length > idx)) {
        for (const row of perCorpus) {
          if (idx < row.length && picked.length < 5) picked.push(row[idx]);
        }
        idx += 1;
      }
      starters = picked;
    } catch (e) {
      console.warn("refreshStarters failed:", e);
      starters = [];
    }
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
  // long synthesis or non-streaming fallback paths. After 1500ms with
  // no slot-text update, append "(still working)"; after 3000ms,
  // "(taking longer than usual)". Caps there — beyond that the diamond
  // pulse animation provides the "still alive" cue without adding
  // false-progress claims. Suspended when a clarification card is up
  // (system is genuinely waiting on the user, not crunching).
  const STALE_INTERVAL_MS = 1500;
  const STALE_SUFFIXES = [
    "",
    " (still working)",
    " (taking longer than usual)",
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
        scrollToBottom();
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

  async function loadConversation(targetId: string | null) {
    onClearTask();
    if (!targetId) {
      send({ type: "RESET" });
      return;
    }

    try {
      const detail = await getConversation(targetId);
      send({
        type: "HYDRATE",
        conversationId: targetId,
        messages: detail.messages,
      });
      scrollToBottom();
    } catch {
      // New conversation — no history yet. HYDRATE with an empty
      // array so the machine still transitions conversationId.
      send({ type: "HYDRATE", conversationId: targetId, messages: [] });
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
      const started = await sendMessageStream(text, convoId);
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
      const started = tryResume ?? (await sendMessageStream(text, convoId));

      wordBuffer.reset();
      send({ type: "SEND_START", assistantMessageId: started.message_id });
      scrollToBottom();
    } catch (e) {
      send({ type: "SEND_FAILED", error: String(e) });
      scrollToBottom();
    }
  }

  async function handleSearch() {
    const text = inputText.trim();
    if (!text || isLoading) return;

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
      const response = await searchWeb(text, convoId);
      send({
        type: "ASSISTANT_MESSAGE_RECEIVED",
        message: {
          id: response.message_id,
          role: "assistant",
          content: response.content,
          created_at: Math.floor(Date.now() / 1000),
        },
      });
    } catch (e) {
      send({
        type: "ASSISTANT_MESSAGE_RECEIVED",
        message: {
          id: crypto.randomUUID(),
          role: "assistant",
          content: `Search error: ${e}`,
          created_at: Math.floor(Date.now() / 1000),
        },
      });
    } finally {
      docOpInFlight = false;
      scrollToBottom();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  function scrollToBottom() {
    requestAnimationFrame(() => {
      if (messagesContainer) {
        messagesContainer.scrollTop = messagesContainer.scrollHeight;
      }
    });
  }
</script>

<div class="chat-view">
  <CorpusProgressBanner {onOpenSettings} />
  <div class="messages" bind:this={messagesContainer}>
    {#if messages.length === 0 && !isLoading}
      <div class="empty-state">
        <div class="empty-glow"></div>
        <div class="empty-mark">◈</div>
        <h2>SOVEREIGN</h2>
        <p class="empty-sub">Your AI. Your data. Your mesh.</p>
        {#await listCorpora() then corpora}
          {#if corpora.filter((c: CorpusEntry) => c.status === "installed").length > 0}
            <div class="kb-tags">
              {#each corpora.filter((c: CorpusEntry) => c.status === "installed") as corpus}
                <span class="kb-tag">{corpus.name}</span>
              {/each}
            </div>
          {/if}
        {:catch}
          <!-- silently ignore if corpus listing fails -->
        {/await}

        {#if starters.length > 0}
          <div class="empty-starters">
            <StarterChips
              questions={starters}
              onPick={pickStarter}
              heading="Try asking"
              subheading="Mined from your enriched knowledge"
            />
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
          onNextStep={handleNextStep}
        />
      {/each}

      <TaskProgress steps={taskSteps} />

      <ApprovalCard />

      <InformationRequestCard
        request={pendingInfoRequest}
        onHandled={() => send({ type: "CLEAR_INFO" })}
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

  <div class="input-area">
    {#if attachedAsset}
      <AttachmentBanner
        filename={attachedAsset.title || attachedAsset.filename}
        chunksCreated={attachedAsset.chunk_count}
        onremove={() => { attachedAsset = null; attachment = null; }}
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
    <button
      class="search-btn"
      onclick={handleSearch}
      disabled={isLoading || !inputText.trim()}
      title="Search the web"
    >
      <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
        <circle cx="7" cy="7" r="5.5" stroke="currentColor" stroke-width="1.5"/>
        <line x1="11" y1="11" x2="14.5" y2="14.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
      </svg>
    </button>
    {#if onToggleInsights}
      <button
        class="insights-toggle-btn"
        onclick={onToggleInsights}
        title="Toggle insights panel"
      >
        &#x25C8;
        {#if insightStore.count > 0}
          <span class="insights-badge">{insightStore.count}</span>
        {/if}
      </button>
    {/if}
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
        disabled={!inputText.trim() || inputIsOversized}
        title={inputIsOversized ? OVERSIZE_MESSAGE_HINT : ""}
      >
        Send
      </button>
    {/if}
    </div>
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
    font-size: 2.8rem;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 14px rgba(201, 168, 76, 0.45));
    margin-bottom: 16px;
    animation: empty-breathe 3.5s ease-in-out infinite;
    position: relative;
  }

  .empty-state h2 {
    font-size: 1.1rem;
    font-weight: 700;
    letter-spacing: 0.22em;
    color: var(--text-secondary);
    margin-bottom: 10px;
    position: relative;
  }

  .empty-sub {
    font-size: 0.8rem;
    color: var(--text-muted);
    letter-spacing: 0.05em;
    margin-bottom: 20px;
    position: relative;
  }

  .kb-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    justify-content: center;
    position: relative;
  }

  .kb-tag {
    font-size: 0.67rem;
    padding: 3px 10px;
    border: 1px solid var(--border-mid);
    border-radius: 100px;
    color: var(--text-muted);
    font-family: 'Syne Mono', monospace;
    letter-spacing: 0.04em;
    background: var(--bg-surface);
  }

  /* ── Starter chips + build indicator in empty state ── */
  .empty-starters {
    margin-top: 24px;
    max-width: 640px;
    text-align: left;
    position: relative;
  }
  .empty-building {
    margin-top: 16px;
    font-size: 0.72rem;
    color: var(--text-muted);
    letter-spacing: 0.02em;
    font-family: 'Syne Mono', monospace;
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
    padding: 12px 20px 16px;
    border-top: 1px solid var(--border-mid);
    background: var(--bg-secondary);
  }

  .input-row {
    display: flex;
    gap: 8px;
    align-items: flex-end;
  }

  .attach-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    color: var(--text-muted);
    cursor: pointer;
    flex-shrink: 0;
    transition: border-color 0.15s, color 0.15s;
  }
  .attach-btn:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
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
    padding: 10px 14px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    resize: none;
    outline: none;
    min-height: 42px;
    max-height: 120px;
    line-height: 1.5;
    color: var(--text-primary);
    transition: border-color 0.2s, box-shadow 0.2s;
  }

  textarea::placeholder {
    color: var(--text-muted);
  }

  textarea:focus {
    border-color: color-mix(in srgb, var(--accent) 50%, transparent);
    box-shadow: 0 0 0 2px var(--accent-glow);
  }

  .search-btn {
    padding: 10px;
    background: var(--bg-surface);
    color: var(--text-muted);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    align-self: flex-end;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: all 0.2s;
  }

  .search-btn:hover:not(:disabled) {
    background: var(--sky-dim);
    border-color: var(--sky);
    color: var(--sky);
  }

  .search-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .insights-toggle-btn {
    padding: 10px;
    background: var(--bg-surface);
    color: var(--amber);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    align-self: flex-end;
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 14px;
    cursor: pointer;
    transition: all 0.2s;
    position: relative;
  }

  .insights-toggle-btn:hover {
    border-color: var(--amber);
    background: rgba(186, 117, 23, 0.06);
  }

  .insights-badge {
    font-size: 9px;
    font-family: var(--font-mono);
    background: var(--accent-glow);
    border: 0.5px solid color-mix(in srgb, var(--amber) 40%, transparent);
    border-radius: 999px;
    padding: 0 4px;
    color: var(--amber);
  }

  .send-btn {
    padding: 10px 20px;
    background: var(--accent);
    color: var(--bg-root);
    border-radius: var(--radius);
    font-weight: 700;
    font-size: 0.82rem;
    letter-spacing: 0.05em;
    align-self: flex-end;
    transition: background 0.2s, box-shadow 0.2s, transform 0.15s;
  }

  .send-btn:hover:not(:disabled) {
    background: var(--accent-light);
    box-shadow: 0 0 18px var(--accent-dim);
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
    padding: 9px 20px;
    background: transparent;
    color: var(--text-primary);
    border: 1px solid var(--text-primary);
    border-radius: var(--radius);
    font-family: var(--font-sans);
    font-weight: 500;
    font-size: 0.9rem;
    cursor: pointer;
    align-self: flex-end;
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
    margin-top: 6px;
    padding: 8px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border-mid);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius);
    font-size: 0.82rem;
    color: var(--text-secondary);
    line-height: 1.45;
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
    font-size: 0.8rem;
    font-weight: 500;
    background: transparent;
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
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
