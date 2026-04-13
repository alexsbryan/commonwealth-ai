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
  import InsightsPanel from "./lib/components/InsightsPanel.svelte";
  import MeshJoinDialog from "./lib/components/MeshJoinDialog.svelte";
  import DocumentLibrary from "./lib/components/DocumentLibrary.svelte";
  import DocumentConversation from "./lib/components/DocumentConversation.svelte";
  import SetupWizard from "./lib/setup/SetupWizard.svelte";
  import type { DocumentAsset } from "./lib/types";

  type AppView = "loading" | "setup" | "chat" | "settings";

  let view: AppView = $state("loading");
  let backendReady = $state(false);
  let backendError: string | null = $state(null);
  let selectedConversationId: string | null = $state(null);
  let showSettings = $state(false);
  let showInsights = $state(false);

  // Document asset view state.
  let activeDocument: DocumentAsset | null = $state(null);

  // Deep-link join dialog state.
  let pendingJoinLink: string | null = $state(null);

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
      onDeepLink: (url: string) => {
        if (url.startsWith("sovereign://join/")) {
          pendingJoinLink = url;
        }
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
    // complete_setup already bootstrapped the backend before returning,
    // so we can go directly to chat rather than waiting for backend-ready.
    // (backend-ready fires before complete_setup returns, so it would be
    // missed if we set view = "loading" here.)
    backendReady = true;
    view = "chat";
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

  let conversationListRef: ConversationList;

  function handleConversationCreated(id: string) {
    // ChatView auto-created a conversation — update the sidebar
    // and select it so the user can navigate back to it.
    selectedConversationId = id;
    conversationListRef?.loadConversations?.();
  }

  function handleOpenDocument(asset: DocumentAsset) {
    activeDocument = asset;
    showSettings = false;
  }

  function handleCloseDocument() {
    activeDocument = null;
  }
</script>

{#if view === "loading"}
  <div class="loading-screen">
    <div class="loading-ambient"></div>
    <div class="loading-content">
      <div class="mark-wrap" aria-hidden="true">
        <div class="ring ring-1"></div>
        <div class="ring ring-2"></div>
        <div class="ring ring-3"></div>
        <div class="loading-mark">◈</div>
      </div>
      <h1>SOVEREIGN</h1>
      <p class="loading-tagline">your ai · your data · your mesh</p>
      {#if backendError}
        <p class="error">{backendError}</p>
      {:else}
        <div class="loading-progress">
          <div class="loading-bar"></div>
        </div>
        <p class="loading-text">Initializing</p>
      {/if}
    </div>
  </div>
{:else if view === "setup"}
  <SetupWizard onComplete={handleSetupComplete} />
{:else}
  <div class="app-layout">
    <aside class="sidebar">
      <ConversationList
        bind:this={conversationListRef}
        {selectedConversationId}
        onSelect={handleConversationSelect}
        onToggleSettings={handleToggleSettings}
      />
      <DocumentLibrary onOpen={handleOpenDocument} />
    </aside>
    <main class="main-content">
      {#if activeDocument}
        <DocumentConversation
          asset={activeDocument}
          onBack={handleCloseDocument}
        />
      {:else if showSettings}
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
          onOpenSettings={() => (showSettings = true)}
          onToggleInsights={() => (showInsights = !showInsights)}
          onConversationCreated={handleConversationCreated}
        />
      {/if}
    </main>
    {#if showInsights && !showSettings}
      <InsightsPanel
        conversationId={selectedConversationId}
        onNavigate={(id) => {
          selectedConversationId = id;
          showInsights = false;
        }}
        onClose={() => (showInsights = false)}
      />
    {/if}
  </div>
{/if}

{#if pendingJoinLink}
  <MeshJoinDialog
    link={pendingJoinLink}
    onClose={() => (pendingJoinLink = null)}
    onJoined={() => {
      pendingJoinLink = null;
      showSettings = true;
    }}
  />
{/if}

<style>
  /* ── Loading screen ── */
  .loading-screen {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    position: relative;
    overflow: hidden;
  }

  .loading-ambient {
    position: absolute;
    inset: 0;
    background:
      radial-gradient(ellipse 55% 45% at 50% 50%, rgba(155, 135, 196, 0.10) 0%, transparent 65%),
      radial-gradient(ellipse 35% 30% at 25% 70%, rgba(201, 168, 76,  0.07) 0%, transparent 60%);
    pointer-events: none;
  }

  .loading-content {
    display: flex;
    flex-direction: column;
    align-items: center;
    z-index: 1;
  }

  /* ── Expanding rings — mesh signal broadcast ── */
  .mark-wrap {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 90px;
    height: 90px;
    margin-bottom: 22px;
  }

  /* Lavender rings expanding from the gold center mark */
  .ring {
    position: absolute;
    border-radius: 50%;
    border: 1px solid rgba(155, 135, 196, 0.40);
    width: 50px;
    height: 50px;
    animation: ring-expand 3s ease-out infinite;
  }

  .ring-2 { animation-delay: 1s; }
  .ring-3 { animation-delay: 2s; }

  @keyframes ring-expand {
    0%   { transform: scale(1);   opacity: 0.55; }
    100% { transform: scale(3.2); opacity: 0; }
  }

  .loading-mark {
    font-size: 2.8rem;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 16px rgba(201, 168, 76, 0.55));
    animation: mark-breathe 2.8s ease-in-out infinite;
    position: relative;
    z-index: 1;
  }

  .loading-content h1 {
    font-size: 1.35rem;
    font-weight: 700;
    letter-spacing: 0.24em;
    color: var(--text-secondary);
    margin-bottom: 6px;
  }

  .loading-tagline {
    font-size: 0.68rem;
    color: var(--text-muted);
    letter-spacing: 0.1em;
    margin-bottom: 30px;
  }

  .loading-progress {
    width: 160px;
    height: 1px;
    background: var(--border-mid);
    border-radius: 1px;
    overflow: hidden;
    margin-bottom: 14px;
  }

  .loading-bar {
    width: 38%;
    height: 100%;
    background: linear-gradient(90deg, transparent, var(--accent), var(--accent-light));
    border-radius: 1px;
    animation: sweep 2s cubic-bezier(0.4, 0, 0.2, 1) infinite;
  }

  .loading-text {
    font-size: 0.68rem;
    color: var(--text-muted);
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }

  .error {
    color: var(--error);
    font-size: 0.85rem;
    max-width: 380px;
    text-align: center;
    line-height: 1.5;
  }

  @keyframes mark-breathe {
    0%, 100% {
      transform: scale(1);
      filter: drop-shadow(0 0 10px rgba(201, 168, 76, 0.42));
    }
    50% {
      transform: scale(1.06);
      filter: drop-shadow(0 0 28px rgba(201, 168, 76, 0.68));
    }
  }

  @keyframes sweep {
    0%   { transform: translateX(-200%); }
    100% { transform: translateX(520%); }
  }

  /* ── App shell ── */
  .app-layout {
    display: flex;
    height: 100vh;
  }

  .sidebar {
    width: 262px;
    min-width: 262px;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border-mid);
    display: flex;
    flex-direction: column;
  }

  .main-content {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-primary);
  }
</style>
