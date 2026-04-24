<script lang="ts">
  import { onMount } from "svelte";
  import { initEventListeners } from "./lib/events";
  import {
    detectBootstrap,
    isFirstRun,
    isSetupComplete,
  } from "./lib/api";
  import type { BootstrapSnapshot, StarterQuestion } from "./lib/types";
  import { approvalStore } from "./lib/stores/approval.svelte";
  import { chatSeedStore } from "./lib/stores/chatSeed.svelte";
  import { joinLinkStore } from "./lib/stores/joinLink.svelte";
  import type {
    StepDonePayload,
    TaskStep,
  } from "./lib/types";
  import ConversationList from "./lib/components/ConversationList.svelte";
  import ChatView from "./lib/components/ChatView.svelte";
  import SettingsPanel from "./lib/components/SettingsPanel.svelte";
  import InsightsPanel from "./lib/components/InsightsPanel.svelte";
  import MeshJoinDialog from "./lib/components/MeshJoinDialog.svelte";
  import SetupWizard from "./lib/setup/SetupWizard.svelte";
  import FirstCorpusFlow from "./lib/components/onboarding/FirstCorpusFlow.svelte";
  import ToastHost from "./lib/components/ToastHost.svelte";

  type AppView = "loading" | "setup" | "first_corpus" | "chat" | "settings";

  let view: AppView = $state("loading");
  let backendReady = $state(false);
  let backendError: string | null = $state(null);
  let selectedConversationId: string | null = $state(null);
  let showSettings = $state(false);
  let showInsights = $state(false);

  // Deep-link join dialog state — sourced from `joinLinkStore`. Two
  // writers: the Tauri `deep-link-received` listener (release builds)
  // and the MeshSettings paste-link input (dev builds where the OS
  // scheme isn't registered).
  let pendingJoinLink = $derived(joinLinkStore.pending);

  // Task progress state (shared across chat). Approval + input state
  // moved to the `approvalStore` singleton — every consumer reads it
  // directly, so no prop drilling.
  let taskSteps: TaskStep[] = $state([]);

  // Bootstrap snapshot drives the "attached to external daemon"
  // badge. Probed once after mount; we don't re-probe because the
  // app either holds an in-process daemon Arc or an HTTP provider
  // for its whole lifetime — if the CLI daemon dies while we're
  // attached, inference 503s will surface it through the chat UI.
  let bootstrap = $state<BootstrapSnapshot | null>(null);
  let attachedToDaemon = $derived(bootstrap?.daemon_running === true);

  async function shouldRouteToFirstCorpus(): Promise<boolean> {
    // The first-run marker (`~/.sovereign/first_run_complete`) is
    // the single source of truth. It's written by
    // `markFirstRunComplete()` at the end of the onboarding flow
    // (whether the user built an atlas or skipped). Absent marker =
    // user hasn't been through onboarding yet.
    //
    // An earlier version also checked `enrichListCorpora().length`,
    // on the theory that anyone with enriched corpora had clearly
    // onboarded. That gave false negatives for developers/testers
    // with prior SEP/book corpora from manual CLI runs — they'd be
    // skipped past onboarding despite having never seen it. The
    // marker is a cleaner contract.
    try {
      return await isFirstRun();
    } catch (e) {
      console.warn("shouldRouteToFirstCorpus probe failed:", e);
      // Fail open: land on chat rather than stranding the user on
      // a broken onboarding gate.
      return false;
    }
  }

  function handleFirstCorpusComplete(seed: StarterQuestion | null) {
    if (seed) chatSeedStore.set(seed);
    view = "chat";
  }

  function handleSettingsStarterPick(question: StarterQuestion) {
    chatSeedStore.set(question);
    showSettings = false;
  }

  /// Called by FolderDropFlow (inside FirstCorpusFlow or Settings)
  /// when the user clicks "Start chatting — atlas keeps building".
  /// The sample atlas is still running; the toast will fire when it
  /// completes, and ChatView's empty-state chips will populate from
  /// `enrich_get_starter_questions` in the meantime.
  function handleDropToChat() {
    view = "chat";
    showSettings = false;
  }

  onMount(async () => {
    await initEventListeners({
      onBackendReady: () => {
        backendReady = true;
        backendError = null;
        if (view === "loading") {
          // First-corpus probe runs async; default to chat and
          // upgrade if the probe says onboarding is warranted.
          view = "chat";
          void (async () => {
            if (await shouldRouteToFirstCorpus()) {
              view = "first_corpus";
            }
          })();
        }
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
      onApprovalRequest: (payload) => {
        approvalStore.send({ type: "APPROVAL_REQUEST_ARRIVED", payload });
      },
      onUserInputRequest: (payload) => {
        approvalStore.send({ type: "INPUT_REQUEST_ARRIVED", payload });
      },
      onError: (payload) => {
        console.error("Backend error:", payload.message);
      },
      onDeepLink: (url: string) => {
        if (url.startsWith("sovereign://join/")) {
          joinLinkStore.set(url);
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

    // Fire-and-forget bootstrap probe. Failure leaves `bootstrap`
    // null, which hides the badge — acceptable: the badge is
    // informational, not functional.
    try {
      bootstrap = await detectBootstrap();
    } catch {
      bootstrap = null;
    }
  });

  async function handleSetupComplete() {
    // complete_setup already bootstrapped the backend before returning,
    // so we can go directly to chat rather than waiting for backend-ready.
    // (backend-ready fires before complete_setup returns, so it would be
    // missed if we set view = "loading" here.)
    backendReady = true;
    // One-time gate: first launch with no enriched corpora → route
    // to the onboarding corpus flow. Returning users (marker exists
    // OR they already have an atlas) land on chat as today.
    if (await shouldRouteToFirstCorpus()) {
      view = "first_corpus";
    } else {
      view = "chat";
    }
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
    // Approval + input cards clear themselves when the user actually
    // submits/skips — leaving them pending across a task switch
    // (e.g. user navigates away mid-approval) is the right default.
    // If we later want "conversation switch cancels pending" we can
    // add a CLEAR_ALL event to approvalMachine.
  }

  let conversationListRef: ConversationList;

  function handleConversationCreated(id: string) {
    // ChatView auto-created a conversation — update the sidebar
    // and select it so the user can navigate back to it.
    selectedConversationId = id;
    conversationListRef?.loadConversations?.();
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
{:else if view === "first_corpus"}
  <FirstCorpusFlow
    onComplete={handleFirstCorpusComplete}
    onDropToChat={handleDropToChat}
  />
{:else}
  <div class="app-layout">
    <aside class="sidebar">
      <ConversationList
        bind:this={conversationListRef}
        {selectedConversationId}
        onSelect={handleConversationSelect}
        onToggleSettings={handleToggleSettings}
      />
    </aside>
    <main class="main-content">
      {#if showSettings}
        <SettingsPanel
          onClose={() => (showSettings = false)}
          onOpenChatWithSeed={handleSettingsStarterPick}
          onDropToChat={handleDropToChat}
        />
      {:else}
        <ChatView
          conversationId={selectedConversationId}
          {taskSteps}
          onClearTask={clearTaskState}
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

<ToastHost />

{#if attachedToDaemon}
  <!-- Small pill anchored bottom-left; informational only. Lets
       the user understand why stopping the CLI daemon would break
       inference, and where to look for logs. -->
  <div
    class="attach-badge"
    title="This desktop is using the daemon started by `sovereign daemon run`. Stopping that service will break inference until it restarts."
  >
    <span class="attach-dot" aria-hidden="true"></span>
    connected to daemon · :{bootstrap?.client_port ?? 9741}
  </div>
{/if}

{#if pendingJoinLink}
  <MeshJoinDialog
    link={pendingJoinLink}
    onClose={() => joinLinkStore.clear()}
    onJoined={() => {
      joinLinkStore.clear();
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

  /* ── Attach-mode badge ── */
  .attach-badge {
    position: fixed;
    bottom: 12px;
    left: 12px;
    z-index: 40;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    font-size: 0.72rem;
    font-family: 'Syne Mono', monospace;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: 999px;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.25);
    pointer-events: auto;
    user-select: none;
  }

  .attach-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--growth);
    box-shadow: 0 0 6px rgba(121, 196, 120, 0.8);
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
