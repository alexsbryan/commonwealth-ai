<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Project checkpoints. The only mutating control on the dashboard:
  // a "Restore" button per row. Restore calls back into the store
  // which lays down a restore-anchor checkpoint; the dashboard
  // refreshes immediately so the new entry appears at the top.
  import Card from "./Card.svelte";
  import { recipeProjectStore } from "../../stores/recipeProject.svelte";
  import type { RecipeCheckpointMeta } from "../../types";

  let {
    featureId: _featureId,
    checkpoints,
  }: {
    featureId: string;
    checkpoints: RecipeCheckpointMeta[];
  } = $props();

  let pendingRestore: string | null = $state(null);
  let restoreError: string | null = $state(null);

  // Render newest first.
  const ordered = $derived([...checkpoints].reverse());

  async function restore(checkpointId: string) {
    if (
      !confirm(
        `Restore project to checkpoint "${checkpointId}"? This rewinds the recipe.toml; decision and research logs are preserved.`,
      )
    ) {
      return;
    }
    pendingRestore = checkpointId;
    restoreError = null;
    try {
      await recipeProjectStore.restoreCheckpoint(checkpointId);
    } catch (e) {
      restoreError = String(e);
    } finally {
      pendingRestore = null;
    }
  }
</script>

<Card title="Checkpoints" counter={checkpoints.length}>
  {#if checkpoints.length === 0}
    <p class="muted">No checkpoints yet.</p>
  {:else}
    <ul>
      {#each ordered as c (c.checkpoint_id)}
        <li>
          <div class="row-head">
            <span class="name">{c.name}</span>
            <button
              type="button"
              class="restore"
              disabled={pendingRestore !== null}
              onclick={() => restore(c.checkpoint_id)}
              data-testid="recipe-author-restore-btn"
            >
              {pendingRestore === c.checkpoint_id ? "…" : "Restore"}
            </button>
          </div>
          <div class="meta">
            <span class="trigger">{c.trigger}</span>
            <span class="when">{c.created_at.slice(0, 19).replace("T", " ")}</span>
            {#if c.restored_from}
              <span class="restored">↳ restored from {c.restored_from}</span>
            {/if}
          </div>
          {#if c.summary}
            <p class="summary">{c.summary}</p>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if restoreError}
    <p class="error">{restoreError}</p>
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
  }
  li {
    border-left: 2px solid var(--border-mid);
    padding: 0.1rem 0 0.1rem 0.55rem;
  }
  .row-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.4rem;
    font-size: 0.84rem;
  }
  .name {
    font-weight: 500;
  }
  .restore {
    background: transparent;
    border: 1px solid var(--border, #2a2c33);
    color: inherit;
    font-size: 0.72rem;
    padding: 1px 8px;
    border-radius: 4px;
    cursor: pointer;
  }
  .restore:hover:not(:disabled) {
    background: var(--bg-elevated);
  }
  .restore:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    font-size: 0.72rem;
    color: var(--muted, #8a8c93);
    margin-top: 2px;
  }
  .trigger {
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .restored {
    color: var(--lavender-light);
  }
  .summary {
    margin: 0.25rem 0 0;
    font-size: 0.78rem;
    color: var(--muted-bright, #b8bac1);
  }
  .error {
    margin-top: 0.4rem;
    color: var(--coral);
    font-size: 0.78rem;
  }
</style>
