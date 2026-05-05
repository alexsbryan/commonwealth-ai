<script lang="ts">
  // Charter card. The charter is the partner's domain framing — what
  // the corpus is, who it's for, what's already settled. Read-only
  // for v1; partner edits go via the New Project dialog or directly
  // on disk through the FeatureRow.
  import Card from "./Card.svelte";

  let { title, charterMd }: { title: string; charterMd: string } = $props();

  let expanded = $state(false);
  const COLLAPSED_LINES = 6;
  const lines = $derived(charterMd.split("\n"));
  const isLong = $derived(lines.length > COLLAPSED_LINES);
  const visible = $derived(
    expanded || !isLong ? charterMd : lines.slice(0, COLLAPSED_LINES).join("\n"),
  );
</script>

<Card title="Charter">
  <p class="project-title">{title}</p>
  {#if charterMd.trim().length === 0}
    <p class="empty">No charter text yet.</p>
  {:else}
    <pre class="md">{visible}</pre>
    {#if isLong}
      <button
        type="button"
        class="toggle"
        onclick={() => (expanded = !expanded)}
      >
        {expanded ? "Collapse" : `Show all ${lines.length} lines`}
      </button>
    {/if}
  {/if}
</Card>

<style>
  .project-title {
    margin: 0 0 0.4rem;
    font-weight: 600;
    font-size: 0.95rem;
  }
  .md {
    margin: 0;
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.78rem;
    white-space: pre-wrap;
    color: var(--fg, #e6e6e8);
    background: transparent;
  }
  .empty {
    color: var(--muted, #8a8c93);
    font-style: italic;
    margin: 0;
  }
  .toggle {
    margin-top: 0.4rem;
    background: transparent;
    border: none;
    color: rgba(120, 200, 240, 0.85);
    cursor: pointer;
    padding: 0;
    font-size: 0.78rem;
    text-decoration: underline;
  }
</style>
