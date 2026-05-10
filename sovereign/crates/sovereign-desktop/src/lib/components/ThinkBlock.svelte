<script lang="ts">
  interface Props {
    content: string;
  }

  let { content }: Props = $props();

  // Glass box: expanded by default in dev builds for full visibility.
  const isDev = (import.meta as any).env?.DEV ?? false;
  let expanded = $state(isDev);

  // Derive a short preview from the first substantive line.
  let preview = $derived.by(() => {
    const firstLine = content.split("\n").find((l) => l.trim().length > 0) ?? "";
    const trimmed = firstLine.trim();
    return trimmed.length > 60 ? trimmed.slice(0, 57) + "..." : trimmed;
  });
</script>

<div class="think-block">
  <button
    class="think-toggle"
    onclick={() => (expanded = !expanded)}
    aria-expanded={expanded}
  >
    <span class="think-arrow" class:expanded>&#x25B6;</span>
    <span class="think-label">Reasoning</span>
    {#if !expanded && preview}
      <span class="think-preview">{preview}</span>
    {/if}
  </button>
  {#if expanded}
    <pre class="think-body">{content}</pre>
  {/if}
</div>

<style>
  .think-block {
    margin: 8px 0;
    border: 0.5px solid var(--border-mid);
    border-radius: var(--radius);
    background: var(--bg-surface);
    overflow: hidden;
  }

  .think-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 6px 10px;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-muted);
    font-family: var(--font-sans);
    font-size: 11px;
    text-align: left;
  }

  .think-toggle:hover {
    color: var(--text-secondary);
  }

  .think-arrow {
    font-size: 9px;
    transition: transform 0.15s ease;
    display: inline-block;
  }

  .think-arrow.expanded {
    transform: rotate(90deg);
  }

  .think-label {
    letter-spacing: 0.05em;
    text-transform: uppercase;
    font-weight: 600;
  }

  .think-preview {
    color: var(--text-muted);
    font-style: italic;
    font-weight: 400;
    text-transform: none;
    letter-spacing: normal;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .think-body {
    font-family: var(--font-mono);
    font-size: 11.5px;
    line-height: 1.65;
    color: var(--text-secondary);
    padding: 0 10px 10px;
    margin: 0;
    white-space: pre-wrap;
    word-wrap: break-word;
    border-top: 0.5px solid var(--border);
  }
</style>
