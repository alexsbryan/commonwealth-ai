<script lang="ts">
  interface Props {
    role: string;
    content: string;
  }

  let { role, content }: Props = $props();
</script>

<div class="bubble" class:user={role === "user"} class:assistant={role !== "user"}>
  <div class="role-label">{role === "user" ? "You" : "Sovereign"}</div>
  <div class="content">{content}</div>
</div>

<style>
  .bubble {
    max-width: 80%;
    padding: 12px 16px;
    border-radius: var(--radius-lg);
    margin-bottom: 12px;
    word-wrap: break-word;
    white-space: pre-wrap;
  }

  .user {
    background: var(--user-bubble);
    align-self: flex-end;
    margin-left: auto;
    border-bottom-right-radius: 4px;
  }

  .assistant {
    background: var(--assistant-bubble);
    align-self: flex-start;
    border: 1px solid var(--border);
    border-bottom-left-radius: 4px;
  }

  .role-label {
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-bottom: 4px;
    font-weight: 500;
  }

  .content {
    line-height: 1.6;
  }
</style>
