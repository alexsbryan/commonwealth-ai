<script lang="ts">
  import AssistantMessage from "./AssistantMessage.svelte";

  interface Props {
    role: string;
    content: string;
    messageId: string;
    conversationId: string;
    metadata?: Record<string, unknown>;
    isStreaming?: boolean;
  }

  let { role, content, messageId, conversationId, metadata, isStreaming }: Props =
    $props();
</script>

{#if role === "user"}
  <div class="bubble user">
    <div class="role-label">You</div>
    <div class="content">{content}</div>
  </div>
{:else}
  <AssistantMessage
    {content}
    {messageId}
    {conversationId}
    {metadata}
    {isStreaming}
  />
{/if}

<style>
  .bubble {
    max-width: 82%;
    margin-bottom: 18px;
    word-wrap: break-word;
    white-space: pre-wrap;
  }

  .user {
    background: var(--user-bubble);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg) var(--radius-lg) var(--radius) var(--radius-lg);
    padding: 12px 16px;
    align-self: flex-end;
    margin-left: auto;
  }

  .user .role-label {
    text-align: right;
    color: var(--text-muted);
    font-size: 0.7rem;
    font-weight: 500;
    letter-spacing: 0.05em;
    margin-bottom: 5px;
    text-transform: uppercase;
  }

  .content {
    line-height: 1.65;
    color: var(--text-primary);
  }
</style>
