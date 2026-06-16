<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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
  import type { StarterQuestion } from "../../types";
  import RecipeProjectList from "./RecipeProjectList.svelte";
  import RecipeChatSurface from "./RecipeChatSurface.svelte";
  import ProjectDashboard from "./ProjectDashboard.svelte";
  import NewProjectDialog from "./NewProjectDialog.svelte";
  import RecipeAuthorWelcome from "./RecipeAuthorWelcome.svelte";
  import AuthoringTutorial from "./tutorial/AuthoringTutorial.svelte";
  import TutorialArtifacts from "./tutorial/TutorialArtifacts.svelte";
  import {
    FEDERALIST_TUTORIAL,
    revealThrough,
  } from "./tutorial/federalistTutorial";
  import {
    installStarterCorpus,
    openCorpusExplorer,
    recordMeshAppInstall,
    openMeshApp,
  } from "../../api";

  // `onUseInChat` (seed a mined question + leave the workspace for chat)
  // and `onOpenChat` (just leave for chat) are the build-complete handoff
  // — host-provided so the workspace stays free of view-routing state.
  let {
    onExit,
    onUseInChat,
    onOpenChat,
  }: {
    onExit: () => void;
    onUseInChat?: (question: StarterQuestion) => void;
    onOpenChat?: () => void;
  } = $props();

  let showNewProject = $state(false);

  // Seeded authoring walkthrough — the primary first-run path. When active it
  // takes over the center + right panes (the left rail stays for orientation);
  // currentStep lives here so the replay player and the artifacts panel sync.
  let tutorialActive = $state(false);
  let tutorialStep = $state(0);
  const tutorialReveal = $derived(revealThrough(FEDERALIST_TUTORIAL, tutorialStep));
  const tutorialHighlight = $derived(FEDERALIST_TUTORIAL[tutorialStep]?.highlight);

  function startTutorial() {
    tutorialStep = 0;
    tutorialActive = true;
  }
  function nextTutorialStep() {
    if (tutorialStep < FEDERALIST_TUTORIAL.length - 1) tutorialStep += 1;
  }
  function backTutorialStep() {
    if (tutorialStep > 0) tutorialStep -= 1;
  }
  function exitTutorial() {
    tutorialActive = false;
  }
  function finishTutorial() {
    // "Start your own" — drop straight into authoring a real project.
    tutorialActive = false;
    showNewProject = true;
  }
  async function launchExplorer() {
    // The demo finale: this isn't a mockup — restore the REAL Federalist
    // corpus (idempotent snapshot restore) and open the bundled Federalist
    // mesh app over it (a real, copy-paste-able app — public/meshapp/federalist/),
    // so the walkthrough ends in a running thing with real data.
    tutorialActive = false;
    try {
      await installStarterCorpus();
      await recordMeshAppInstall("federalist", "The Federalist Papers", {
        mesh_store_read: true,
        mesh_store_write: false,
        inference_access: false,
        knowledge_access: false,
      });
      await openMeshApp("federalist");
    } catch (e) {
      console.error("launch federalist app:", e);
    }
  }
  /// Open the generic Atlas Explorer over a corpus the user just built — the
  /// same feature the demo previews, now on their own data.
  function openExplorerForCorpus(corpusId: string) {
    void openCorpusExplorer(corpusId);
  }

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
      {#if tutorialActive}
        <AuthoringTutorial
          steps={FEDERALIST_TUTORIAL}
          currentStep={tutorialStep}
          onNext={nextTutorialStep}
          onBack={backTutorialStep}
          onExit={exitTutorial}
          onFinish={finishTutorial}
          onLaunchExplorer={launchExplorer}
        />
      {:else if selectedFeatureId && dashboard}
        <RecipeChatSurface
          featureId={selectedFeatureId}
          projectTitle={dashboard.title}
        />
      {:else}
        <RecipeAuthorWelcome
          hasProjects={projects.length > 0}
          onNewProject={() => (showNewProject = true)}
          onStartTutorial={startTutorial}
        />
      {/if}
    </main>

    <aside class="dashboard">
      {#if tutorialActive}
        <TutorialArtifacts reveal={tutorialReveal} highlight={tutorialHighlight} />
      {:else if dashboard}
        <ProjectDashboard
          {dashboard}
          {onUseInChat}
          {onOpenChat}
          onOpenExplorer={openExplorerForCorpus}
        />
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
