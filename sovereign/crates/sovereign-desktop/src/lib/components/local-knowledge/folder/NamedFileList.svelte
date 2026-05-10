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

<ul class="list">
  {#each visible as f (f.path)}
    <li class="row">{f.display_name}</li>
  {/each}
  {#if !expanded && hidden > 0}
    <li>
      <button class="show-all" onclick={() => (expanded = true)}>
        Show {hidden} more
      </button>
    </li>
  {/if}
</ul>

<style>
  .list {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row {
    font-family: var(--lk-font-mono);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    padding: 1px 0;
  }
  .show-all {
    background: transparent;
    border: 0;
    padding: 4px 0;
    font-family: var(--lk-font-body);
    font-size: 12px;
    color: var(--lk-crown-light);
    cursor: pointer;
  }
  .show-all:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
</style>
