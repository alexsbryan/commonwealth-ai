<script lang="ts">
  // Top-level recipe-author workspace. Two-panel layout:
  //   left  — project sidebar (list + new project)
  //   center — slim chat surface (transcript + composer)
  //   right — project dashboard (cards) when a project is selected
  //
  // The conversation surface intentionally does NOT reuse `ChatView`
  // — that file is 1500+ LOC and carries the corpus / atlas /
  // insights baggage we don't want here. The slim `RecipeChatSurface`
  // talks to the same `send_message_stream` backend; the
  // recipe-author skill is activated by the store on mount so
  // primary_skill_id_for_conversation routes the turn through the
  // recipe-author system prompt.

  import { onMount, onDestroy } from "svelte";
  import { recipeProjectStore } from "../../stores/recipeProject.svelte";
  import RecipeProjectList from "./RecipeProjectList.svelte";
  import RecipeChatSurface from "./RecipeChatSurface.svelte";
  import ProjectDashboard from "./ProjectDashboard.svelte";
  import NewProjectDialog from "./NewProjectDialog.svelte";

  let { onExit }: { onExit: () => void } = $props();

  let showNewProject = $state(false);

  onMount(async () => {
    await recipeProjectStore.activate();
  });

  onDestroy(async () => {
    await recipeProjectStore.deactivate();
  });

  const dashboard = $derived(recipeProjectStore.dashboard);
  const projects = $derived(recipeProjectStore.projects);
  const selectedFeatureId = $derived(recipeProjectStore.selectedFeatureId);
  const lastError = $derived(recipeProjectStore.lastError);

  async function handleSelect(featureId: string) {
    await recipeProjectStore.select(featureId);
  }

  async function handleCreate(title: string, charterMd: string) {
    await recipeProjectStore.createProject(title, charterMd);
    showNewProject = false;
  }
</script>

<div class="workspace" data-testid="recipe-author-workspace">
  <header class="topbar">
    <div class="brand">
      <span class="mark" aria-hidden="true">◇</span>
      <span class="title">Recipe Author</span>
    </div>
    <div class="topbar-actions">
      <button
        type="button"
        class="exit-btn"
        onclick={onExit}
        title="Back to chat"
      >
        ← Back to chat
      </button>
    </div>
  </header>

  {#if lastError}
    <div class="error-banner" role="alert">{lastError}</div>
  {/if}

  <div class="panes">
    <aside class="sidebar">
      <RecipeProjectList
        {projects}
        {selectedFeatureId}
        onSelect={handleSelect}
        onNewProject={() => (showNewProject = true)}
      />
    </aside>

    <main class="conversation">
      {#if selectedFeatureId && dashboard}
        <RecipeChatSurface
          featureId={selectedFeatureId}
          projectTitle={dashboard.title}
        />
      {:else}
        <div class="empty-state">
          <p>Pick a project on the left, or start a new one.</p>
        </div>
      {/if}
    </main>

    <aside class="dashboard">
      {#if dashboard}
        <ProjectDashboard {dashboard} />
      {:else if selectedFeatureId}
        <div class="loading-card">Loading dashboard…</div>
      {:else}
        <div class="empty-card">No project selected.</div>
      {/if}
    </aside>
  </div>

  {#if showNewProject}
    <NewProjectDialog
      onCancel={() => (showNewProject = false)}
      onCreate={handleCreate}
    />
  {/if}
</div>

<style>
  .workspace {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: var(--bg, #0e0f12);
    color: var(--fg, #e6e6e8);
    font-family: var(--ui-font, system-ui, sans-serif);
  }
  .topbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.6rem 1rem;
    border-bottom: 1px solid var(--border, #2a2c33);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .mark {
    font-size: 1.1rem;
    opacity: 0.8;
  }
  .title {
    font-weight: 600;
    letter-spacing: 0.04em;
  }
  .exit-btn {
    background: transparent;
    border: 1px solid var(--border, #2a2c33);
    color: inherit;
    padding: 0.3rem 0.7rem;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .exit-btn:hover {
    background: var(--bg-elevated);
  }
  .error-banner {
    background: color-mix(in srgb, var(--error) 18%, transparent);
    color: var(--coral);
    padding: 0.5rem 1rem;
    font-size: 0.85rem;
    border-bottom: 1px solid color-mix(in srgb, var(--error) 35%, transparent);
  }
  .panes {
    display: grid;
    grid-template-columns: 280px minmax(0, 1fr) 360px;
    flex: 1 1 auto;
    min-height: 0;
  }
  .sidebar {
    border-right: 1px solid var(--border, #2a2c33);
    overflow-y: auto;
  }
  .conversation {
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
  }
  .dashboard {
    border-left: 1px solid var(--border, #2a2c33);
    overflow-y: auto;
    padding: 0.75rem;
    background: transparent;
  }
  .empty-state,
  .loading-card,
  .empty-card {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--muted, #8a8c93);
    font-size: 0.9rem;
    padding: 1rem;
    text-align: center;
  }
</style>
