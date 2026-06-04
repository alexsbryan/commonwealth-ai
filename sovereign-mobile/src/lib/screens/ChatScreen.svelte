<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { useMachine } from "@xstate/svelte";
  import { chatMachine } from "../machines/chat.machine";
  import { attachStreamListeners } from "../events";
  import { getConversation, sendMessageStream } from "../api";
  import type { MessageEntry } from "../types";
  import AssistantMessage from "../components/AssistantMessage.svelte";

  let { conversationId, onback }: { conversationId: string; onback: () => void } = $props();

  const { snapshot, send } = useMachine(chatMachine);
  let input = $state("");
  let unlisten: (() => void) | null = null;

  onMount(async () => {
    // Same event contract the desktop FSM consumes — the Rust core
    // re-emits message-start/chunk/complete/error.
    unlisten = await attachStreamListeners(send);
    send({ type: "CONVERSATION_BOUND", conversationId });
    // Cache-first hydrate (offline-read / instant relaunch).
    const convo = await getConversation(conversationId);
    if (convo) {
      send({
        type: "HYDRATE",
        conversationId,
        messages: convo.messages as unknown as MessageEntry[],
      });
    }
  });

  onDestroy(() => unlisten?.());

  async function submit() {
    const text = input.trim();
    if (!text) return;
    input = "";
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
</script>

<div class="chat">
  <header><button class="back" onclick={onback}>←</button></header>
  <div class="scroll">
    {#each messages as m (m.id)}
      {#if m.role === "assistant"}
        <AssistantMessage content={m.content} metadata={m.metadata} />
      {:else}
        <div class="user">{m.content}</div>
      {/if}
    {/each}
  </div>
  <form
    class="composer"
    onsubmit={(e) => {
      e.preventDefault();
      void submit();
    }}
  >
    <input bind:value={input} placeholder="Type a message…" />
    <button type="submit" disabled={!input.trim()}>Send</button>
  </form>
</div>

<style>
  .chat {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }
  header {
    display: flex;
    align-items: center;
    padding: 0.35rem 0.5rem;
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
    padding: 1rem 0.9rem;
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
    word-wrap: break-word;
  }
  .composer {
    display: flex;
    gap: 0.5rem;
    align-items: flex-end;
    padding: 0.6rem 0.7rem calc(0.6rem + env(safe-area-inset-bottom));
    border-top: 1px solid var(--border);
    background: var(--bg-secondary);
  }
  .composer input {
    flex: 1;
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
    background: var(--accent);
    color: var(--text-on-accent);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    padding: 0.62rem 1rem;
    font-weight: 600;
    font-size: 0.9rem;
    transition: background 0.15s, opacity 0.15s;
  }
  .composer button:active:not(:disabled) { background: var(--accent-hover); }
  .composer button:disabled { opacity: 0.4; }
</style>
