<script lang="ts">
  import type { FileMeta } from "../../../types";

  interface Props {
    files: FileMeta[];
    showUpTo?: number;
  }

  let { files, showUpTo = 2 }: Props = $props();
  let expanded = $state(false);

  let visible = $derived(expanded ? files : files.slice(0, showUpTo));
  let hidden = $derived(Math.max(0, files.length - showUpTo));
</script>

<div class="named-file-list">
  {#each visible as f (f.path)}
    <div class="file-row">{f.display_name}</div>
  {/each}
  {#if !expanded && hidden > 0}
    <button class="show-all" onclick={() => (expanded = true)}>
      Show {hidden} more
    </button>
  {/if}
</div>

<style>
  .file-row {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    font-family: var(--font-mono, ui-monospace, monospace);
    padding: 2px 0;
  }
  .show-all {
    background: transparent;
    border: none;
    color: var(--color-accent, #3a5fc9);
    font-size: 13px;
    cursor: pointer;
    padding: 4px 0;
    text-align: left;
  }
  .show-all:hover {
    text-decoration: underline;
  }
</style>
