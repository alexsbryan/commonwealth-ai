<script lang="ts">
  import { onMount } from "svelte";
  import { initEventListeners } from "./lib/events";
  import { isSetupComplete } from "./lib/api";
  import type {
    ApprovalRequestPayload,
    UserInputRequestPayload,
    StepDonePayload,
    TaskStep,
  } from "./lib/types";
  import ConversationList from "./lib/components/ConversationList.svelte";
  import ChatView from "./lib/components/ChatView.svelte";
  import SettingsPanel from "./lib/components/SettingsPanel.svelte";
  import SetupWizard from "./lib/setup/SetupWizard.svelte";

  type AppView = "loading" | "setup" | "chat" | "settings";

  let view: AppView = $state("loading");
  let backendReady = $state(false);
  let backendError: string | null = $state(null);
  let selectedConversationId: string | null = $state(null);
  let showSettings = $state(false);

  // Task progress state (shared across chat).
  let taskSteps: TaskStep[] = $state([]);
  let pendingApproval: ApprovalRequestPayload | null = $state(null);
  let pendingInput: UserInputRequestPayload | null = $state(null);

  onMount(async () => {
    await initEventListeners({
      onBackendReady: () => {
        backendReady = true;
        backendError = null;
        if (view === "loading") view = "chat";
      },
      onBackendError: (error) => {
        backendError = error;
      },
      onSetupRequired: () => {
        view = "setup";
      },
      onStepDone: (payload: StepDonePayload) => {
        const existing = taskSteps.find((s) => s.id === payload.step_id);
        if (existing) {
          existing.status = payload.status === "done" ? "done" : "skipped";
        } else {
          taskSteps = [
            ...taskSteps,
            {
              id: payload.step_id,
              description: payload.description,
              status: payload.status === "done" ? "done" : "skipped",
            },
          ];
        }
      },
      onApprovalRequest: (payload: ApprovalRequestPayload) => {
        pendingApproval = payload;
      },
      onUserInputRequest: (payload: UserInputRequestPayload) => {
        pendingInput = payload;
      },
      onError: (payload) => {
        console.error("Backend error:", payload.message);
      },
    });

    // Check if setup is already complete.
    try {
      const complete = await isSetupComplete();
      if (!complete) {
        view = "setup";
      }
      // If complete, wait for backend-ready event (async bootstrap).
    } catch {
      // Backend not ready yet — stay on loading.
    }
  });

  function handleSetupComplete() {
    view = "loading";
    // Backend will emit "backend-ready" after bootstrap.
  }

  function handleConversationSelect(id: string | null) {
    selectedConversationId = id;
    showSettings = false;
  }

  function handleToggleSettings() {
    showSettings = !showSettings;
  }

  function clearTaskState() {
    taskSteps = [];
    pendingApproval = null;
    pendingInput = null;
  }
</script>

{#if view === "loading"}
  <div class="loading-screen">
    <div class="loading-content">
      <h1>Sovereign</h1>
      {#if backendError}
        <p class="error">{backendError}</p>
      {:else}
        <p class="loading-text">Loading model...</p>
        <div class="spinner"></div>
      {/if}
    </div>
  </div>
{:else if view === "setup"}
  <SetupWizard onComplete={handleSetupComplete} />
{:else}
  <div class="app-layout">
    <aside class="sidebar">
      <ConversationList
        {selectedConversationId}
        onSelect={handleConversationSelect}
        onToggleSettings={handleToggleSettings}
      />
    </aside>
    <main class="main-content">
      {#if showSettings}
        <SettingsPanel onClose={() => (showSettings = false)} />
      {:else}
        <ChatView
          conversationId={selectedConversationId}
          {taskSteps}
          {pendingApproval}
          {pendingInput}
          onClearTask={clearTaskState}
          onApprovalHandled={() => (pendingApproval = null)}
          onInputHandled={() => (pendingInput = null)}
        />
      {/if}
    </main>
  </div>
{/if}

<style>
  .loading-screen {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    background: var(--bg-primary);
  }

  .loading-content {
    text-align: center;
  }

  .loading-content h1 {
    font-size: 2rem;
    font-weight: 300;
    margin-bottom: 1rem;
    color: var(--text-primary);
  }

  .loading-text {
    color: var(--text-secondary);
    margin-bottom: 1.5rem;
  }

  .error {
    color: var(--error);
    max-width: 400px;
    margin: 0 auto;
  }

  .spinner {
    width: 32px;
    height: 32px;
    border: 3px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    margin: 0 auto;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .app-layout {
    display: flex;
    height: 100vh;
  }

  .sidebar {
    width: 260px;
    min-width: 260px;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
  }

  .main-content {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
</style>
