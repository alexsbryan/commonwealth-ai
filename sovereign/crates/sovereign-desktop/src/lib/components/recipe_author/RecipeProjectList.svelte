<script lang="ts">
  // Sidebar list of recipe-author projects. Pure presentation —
  // mutations route through the parent (which talks to the store).
  import type { RecipeProjectListEntry } from "../../types";

  let {
    projects,
    selectedFeatureId,
    onSelect,
    onNewProject,
  }: {
    projects: RecipeProjectListEntry[];
    selectedFeatureId: string | null;
    onSelect: (featureId: string) => void;
    onNewProject: () => void;
  } = $props();

  function fmtRelative(ts: number): string {
    const ms = ts * 1000;
    const diff = Date.now() - ms;
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    if (days < 30) return `${days}d ago`;
    return new Date(ms).toLocaleDateString();
  }
</script>

<div class="list">
  <button
    type="button"
    class="new-btn"
    onclick={onNewProject}
    data-testid="recipe-author-new-project"
  >
    + New project
  </button>

  {#if projects.length === 0}
    <p class="empty">No recipe projects yet.</p>
  {:else}
    <ul role="list">
      {#each projects as p (p.feature_id)}
        <li>
          <button
            type="button"
            class="row"
            class:active={p.feature_id === selectedFeatureId}
            onclick={() => onSelect(p.feature_id)}
            title={p.charter_excerpt}
            data-testid="recipe-author-project-row"
          >
            <span class="row-title">{p.title}</span>
            <span class="row-meta">
              {#if p.last_test_status}
                <span
                  class="status"
                  class:pass={p.last_test_status === "pass"}
                  class:fail={p.last_test_status === "fail"}
                >
                  {p.last_test_status}
                </span>
              {/if}
              {#if p.current_sample_size}
                <span class="muted">n={p.current_sample_size}</span>
              {/if}
              <span class="muted">{fmtRelative(p.updated_at)}</span>
            </span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.6rem;
  }
  .new-btn {
    width: 100%;
    padding: 0.5rem 0.7rem;
    background: rgba(120, 200, 240, 0.12);
    border: 1px solid rgba(120, 200, 240, 0.3);
    color: inherit;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.9rem;
    text-align: left;
  }
  .new-btn:hover {
    background: rgba(120, 200, 240, 0.18);
  }
  .empty {
    color: var(--muted, #8a8c93);
    font-size: 0.85rem;
    padding: 0.4rem 0.2rem;
  }
  ul {
    list-style: none;
    padding: 0;
    margin: 0.4rem 0 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.2rem;
    width: 100%;
    padding: 0.5rem 0.6rem;
    background: transparent;
    border: 1px solid transparent;
    color: inherit;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.88rem;
    text-align: left;
  }
  .row:hover {
    background: rgba(255, 255, 255, 0.04);
  }
  .row.active {
    background: rgba(120, 200, 240, 0.12);
    border-color: rgba(120, 200, 240, 0.3);
  }
  .row-title {
    font-weight: 500;
  }
  .row-meta {
    display: flex;
    gap: 0.5rem;
    font-size: 0.75rem;
    align-items: center;
  }
  .status {
    text-transform: uppercase;
    font-size: 0.7rem;
    padding: 1px 6px;
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.08);
  }
  .status.pass {
    background: rgba(120, 220, 160, 0.18);
    color: #b9f0c9;
  }
  .status.fail {
    background: rgba(240, 130, 130, 0.18);
    color: #f6c0c0;
  }
  .muted {
    color: var(--muted, #8a8c93);
  }
</style>
