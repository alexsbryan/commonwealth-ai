<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { useMachine } from "@xstate/svelte";
  import { chatMachine } from "../machines/chat.machine";
  import { attachStreamListeners, attachNarrationListener, type NarrationEntry } from "../events";
  import { getConversation, sendMessageStream } from "../api";
  import type { MessageEntry } from "../types";
  import AssistantMessage from "../components/AssistantMessage.svelte";
  import NarrationChip from "../components/NarrationChip.svelte";

  let { conversationId, onback }: { conversationId: string; onback: () => void } = $props();

  const { snapshot, send } = useMachine(chatMachine);
  let input = $state("");
  // Live turn progress (glassbox). Transient — kept out of the chat FSM
  // (mirrors desktop's separate routingStore); cleared when the turn ends.
  let narration = $state<NarrationEntry[]>([]);
  const offs: UnlistenFn[] = [];

  onMount(async () => {
    // Same event contract the desktop FSM consumes — the Rust core
    // re-emits message-start/chunk/complete/error.
    offs.push(await attachStreamListeners(send));
    // Live progress narration → the in-flight chip stack.
    offs.push(
      await attachNarrationListener((n) => {
        if (n.conversation_id !== conversationId) return;
        narration = [...narration, n].slice(-6);
      }),
    );
    // The answer has landed (or failed) → drop the progress trace.
    offs.push(await listen("message-complete", () => (narration = [])));
    offs.push(await listen("message-error", () => (narration = [])));

    send({ type: "CONVERSATION_BOUND", conversationId });
    // Cache-first hydrate (offline-read / instant relaunch).
    const convo = await getConversation(conversationId);
    if (convo) {
      send({
        type: "HYDRATE",
        conversationId,
        messages: convo.messages as unknown as MessageEntry[],
      });
      // On open, land at the latest message instantly — no animated catch-up
      // scrolling the whole history. (Streaming uses the gentle follow.)
      requestAnimationFrame(() => {
        const el = scrollEl;
        if (el) {
          el.scrollTop = el.scrollHeight;
          stick = true;
        }
      });
    }
  });

  onDestroy(() => {
    offs.forEach((off) => off());
    if (rafId) cancelAnimationFrame(rafId);
  });

  async function submit() {
    const text = input.trim();
    if (!text) return;
    input = "";
    narration = []; // fresh turn — clear any prior progress trace
    stick = true; // re-engage follow for the user's turn + the answer
    const userMsg: MessageEntry = {
      id: `local-${Date.now()}`,
      role: "user",
      content: text,
      created_at: Date.now(),
    };
    send({ type: "SEND_INITIATED", userMessage: userMsg });
    try {
      // Kicks off the WS stream in the Rust core. SEND_START + chunks
      // arrive via events; on a busy host a message-error fires.
      await sendMessageStream(conversationId, text);
    } catch (e) {
      send({ type: "MESSAGE_ERROR", error: String(e) });
    }
  }

  const messages = $derived($snapshot.context.messages as MessageEntry[]);

  // Keep the latest content in view as it streams — but *gently*. A hard
  // `scrollTop = scrollHeight` per token (the old behaviour) reads as a jarring
  // jitter against fast token output, and it yanks the reader back even when
  // they've scrolled up to re-read. Instead: ease toward the bottom over a few
  // frames (a catch-up, not a snap), and only while the reader is already near
  // the bottom. Scrolling up releases the follow; scrolling back re-engages it.
  let scrollEl = $state<HTMLDivElement | null>(null);
  let stick = true;
  let rafId = 0;
  let lastTop = 0;

  function followBottom() {
    const el = scrollEl;
    if (!el || !stick) {
      rafId = 0;
      return;
    }
    const target = el.scrollHeight - el.clientHeight;
    const delta = target - el.scrollTop;
    if (delta > 1) {
      // ~18% of the remaining gap per frame (floored) — a smooth approach.
      el.scrollTop += Math.max(delta * 0.18, 0.5);
      rafId = requestAnimationFrame(followBottom);
    } else {
      el.scrollTop = target;
      rafId = 0;
    }
  }

  function nudgeFollow() {
    if (stick && rafId === 0) rafId = requestAnimationFrame(followBottom);
  }

  function onScroll() {
    const el = scrollEl;
    if (!el) return;
    const top = el.scrollTop;
    const dist = el.scrollHeight - top - el.clientHeight;
    // Direction-based so the programmatic follow (which only scrolls DOWN)
    // never disengages itself: a real upward scroll releases the follow;
    // returning to the bottom re-engages it.
    if (top < lastTop - 2) stick = false;
    else if (dist < 40) stick = true;
    lastTop = top;
  }

  $effect(() => {
    // Re-run on each streamed token / new message / narration update.
    void messages.length;
    void (messages.at(-1)?.content?.length ?? 0);
    void narration.length;
    nudgeFollow();
  });
</script>

<div class="chat">
  <header>
    <button class="back" onclick={onback} aria-label="Back to conversations">
      <span aria-hidden="true">←</span>
    </button>
  </header>
  <div
    class="scroll"
    role="log"
    aria-live="polite"
    aria-label="Conversation"
    bind:this={scrollEl}
    onscroll={onScroll}
  >
    {#each messages as m (m.id)}
      {#if m.role === "assistant"}
        <AssistantMessage content={m.content} metadata={m.metadata} />
      {:else}
        <div class="user">{m.content}</div>
      {/if}
    {/each}
    {#if narration.length}
      <NarrationChip entries={narration} />
    {/if}
  </div>
  <form
    class="composer"
    onsubmit={(e) => {
      e.preventDefault();
      void submit();
    }}
  >
    <input
      bind:value={input}
      placeholder="Type a message…"
      aria-label="Message"
      enterkeyhint="send"
    />
    <button type="submit" disabled={!input.trim()} aria-label="Send message">Send</button>
  </form>
</div>

<style>
  .chat {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    /* Centered reading column — capped on tablets, full-width (with
       gutters) on a phone. */
    width: 100%;
    max-width: var(--measure);
    margin-inline: auto;
  }
  header {
    display: flex;
    align-items: center;
    padding: 0.35rem var(--pad-r) 0.35rem var(--pad-l);
    border-bottom: 1px solid var(--border);
  }
  .back {
    color: var(--text-secondary);
    font-size: 1.4rem;
    line-height: 1;
    padding: 0.35rem 0.65rem;
    border-radius: var(--radius);
    transition: color 0.15s, background 0.15s;
  }
  .back:active {
    color: var(--lavender);
    background: var(--bg-surface);
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 1rem var(--pad-r) 1rem var(--pad-l);
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  .user {
    align-self: flex-end;
    max-width: 82%;
    background: var(--user-bubble);
    border: 1px solid var(--border-mid);
    color: var(--text-primary);
    padding: 0.62rem 0.85rem;
    border-radius: var(--radius-lg) var(--radius-lg) var(--radius) var(--radius-lg);
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .composer {
    display: flex;
    gap: 0.5rem;
    align-items: flex-end;
    padding: 0.6rem var(--pad-r) calc(0.6rem + env(safe-area-inset-bottom)) var(--pad-l);
    border-top: 1px solid var(--border);
    background: var(--bg-secondary);
  }
  .composer input {
    flex: 1 1 auto;
    /* min-width:0 lets the input shrink below its intrinsic size so the
       Send button is never pushed off-screen on a narrow device. */
    min-width: 0;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 0.62rem 0.8rem;
    color: var(--text-primary);
    font-size: 0.95rem;
    transition: border-color 0.15s;
  }
  .composer input::placeholder { color: var(--text-muted); }
  .composer input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 50%, transparent);
  }
  .composer button {
    flex: none;
    background: var(--accent);
    color: var(--text-on-accent);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    padding: 0.62rem 1rem;
    font-weight: 600;
    font-size: 0.9rem;
    white-space: nowrap;
    transition: background 0.15s, opacity 0.15s;
  }
  .composer button:active:not(:disabled) { background: var(--accent-hover); }
  .composer button:disabled { opacity: 0.4; }
</style>
