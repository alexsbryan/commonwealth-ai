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
    <input bind:value={input} placeholder="Ask your host…" />
    <button type="submit">Send</button>
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
    padding: 0.5rem;
  }
  .back {
    background: transparent;
    color: var(--text);
    font-size: 1.2rem;
    padding: 0.3rem 0.6rem;
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .user {
    align-self: flex-end;
    background: var(--accent);
    color: #0b1020;
    padding: 0.5rem 0.75rem;
    border-radius: 12px 12px 2px 12px;
    max-width: 80%;
  }
  .composer {
    display: flex;
    gap: 0.5rem;
    padding: 0.6rem;
    border-top: 1px solid #232833;
  }
  .composer input {
    flex: 1;
  }
</style>
