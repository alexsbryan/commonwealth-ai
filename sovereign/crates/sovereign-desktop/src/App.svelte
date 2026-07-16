<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { initEventListeners } from "./lib/events";
  import {
    detectBootstrap,
    isSetupComplete,
    isBackendReady,
    startDefaultCorpusInstall,
  } from "./lib/api";
  import type { BootstrapSnapshot, StarterQuestion } from "./lib/types";
  import { approvalStore } from "./lib/stores/approval.svelte";
  import { chatSeedStore } from "./lib/stores/chatSeed.svelte";
  import { outerWorkScopeStore } from "./lib/stores/outerWorkScope.svelte";
  import { joinLinkStore } from "./lib/stores/joinLink.svelte";
  import { meshMembership } from "./lib/stores/meshMembership.svelte";
  import { toastStore } from "./lib/stores/toast.svelte";
  import type {
    StepDonePayload,
    TaskStep,
  } from "./lib/types";
  import NavRail from "./lib/components/NavRail.svelte";
  import ConversationList from "./lib/components/ConversationList.svelte";
  import ChatView from "./lib/components/ChatView.svelte";
  import WorkshopView from "./lib/components/workshop/WorkshopView.svelte";
  import InnerWorkSurface from "./lib/components/inner_work/InnerWorkSurface.svelte";
  import SettingsPanel from "./lib/components/SettingsPanel.svelte";
  import InsightsPanel from "./lib/components/InsightsPanel.svelte";
  import ReadingSurface from "./lib/components/reading/ReadingSurface.svelte";
  import AtomPanel from "./lib/components/reading/AtomPanel.svelte";
  import AtlasSurface from "./lib/components/atlas/AtlasSurface.svelte";
  import LibraryView from "./lib/components/library/LibraryView.svelte";
  import BrandMark from "./lib/components/BrandMark.svelte";
  import { readingSession } from "./lib/stores/readingSession.svelte";
  import { atlasNavigation } from "./lib/stores/atlasNavigation.svelte";
  import { readingNavigation } from "./lib/stores/readingNavigation.svelte";
  import MeshJoinDialog from "./lib/components/MeshJoinDialog.svelte";
  import SetupFlow from "./lib/setup/SetupFlow.svelte";
  import WelcomeThreshold from "./lib/setup/WelcomeThreshold.svelte";
  import SetupPlan from "./lib/setup/SetupPlan.svelte";
  import type { PrimarySource } from "./lib/setup/setupTypes";
  import ConsentGate from "./lib/setup/ConsentGate.svelte";
  import ReconnectBanner from "./lib/components/ReconnectBanner.svelte";
  import ModelNoticeBanner from "./lib/components/ModelNoticeBanner.svelte";
  import { getFirstMeshConsent } from "./lib/api";
  import { ensureSeededConversations } from "./lib/setup/seededConversations";
  import ToastHost from "./lib/components/ToastHost.svelte";

  type AppView =
    | "loading"
    | "welcome"
    | "setup_plan"
    | "setup"
    | "consent"
    | "chat"
    | "library"
    | "settings"
    | "inner_work"
    // `atlas` is no longer a rail destination — the Library's per-notebook
    // Explore tab owns the atlas surface. It survives as a view only as the
    // reading-surface "Open in atlas" deep-link target (see the bridge below).
    | "atlas"
    | "workshop";

  type RailMode = "chat" | "library" | "inner_work" | "workshop" | "settings";

  // `let view: AppView = $state("loading")` would narrow `view` to the
  // literal type `"loading"`, breaking every later `view === "chat"`
  // comparison. The generic form keeps the full union.
  let view = $state<AppView>("loading");

  // Rail mirrors view for every top-level destination. The deep-linked
  // `atlas` view (reading-surface "Open in atlas") has no rail slot of its
  // own anymore — it highlights Library, which is where exploring a
  // notebook's map now lives.
  let railMode: RailMode = $derived(
    view === "library" ? "library"
    : view === "atlas" ? "library"
    : view === "inner_work" ? "inner_work"
    : view === "workshop" ? "workshop"
    : view === "settings" ? "settings"
    : "chat"
  );

  let showNavRail = $derived(
    view !== "loading" && view !== "welcome" && view !== "setup_plan"
      && view !== "setup" && view !== "consent"
  );

  // The user's starter-corpus choice from the Setup Plan screen. Defaults
  // to true (batteries-included) but is only honored after explicit consent
  // — `handleSetupComplete` gates the background install on it, so the
  // Wikipedia download never happens unconsented.
  let installStarterCorpus = $state(true);

  // The user's "Customize" primary-model choice from the Setup Plan screen
  // (a catalog GGUF filename); undefined = the hardware-recommended default.
  let chosenPrimaryFile = $state<string | undefined>(undefined);
  let chosenPrimarySource = $state<PrimarySource | undefined>(undefined);

  let backendReady = $state(false);
  let backendError: string | null = $state(null);
  // Boot watchdog: `backend-ready`/`backend-error` are push-only Tauri
  // events with no replay, so a missed one would strand us on the splash
  // forever. `bootStalled` flips after a generous window with no readiness,
  // turning the infinite spinner into a self-reporting state + retry.
  let bootStalled = $state(false);
  let bootPoll: ReturnType<typeof setInterval> | undefined;
  let selectedConversationId: string | null = $state(null);
  let showInsights = $state(false);

  // Conversation list collapse — toggled by Cmd+[ when in chat view.
  let convListCollapsed = $state(false);
  // Signal counter for the inner-work history drawer — increment to toggle.
  let innerWorkHistoryToggle = $state(0);

  // InnerWorkSurface mounts once on first visit and stays alive so
  // subsequent toggles are instant (CSS show/hide, no re-mount).
  // Without this, every navigation to inner_work re-runs the full
  // mount lifecycle: skill snapshot, conversation lookup, API calls.
  let innerWorkMounted = $state(false);
  $effect(() => {
    if (view === "inner_work") innerWorkMounted = true;
  });

  // Atlas bridge — when the ReadingSurface's AtomPanel requests
  // "Open in atlas", the chat view can't switch its own host's
  // view. Watch the store here, flip the rail to atlas, and let
  // AtlasSurface consume the pending atom on mount. The store
  // self-clears via `take()` on the receiver side.
  $effect(() => {
    if (atlasNavigation.pendingAtom && view !== "atlas") {
      view = "atlas";
    }
  });

  // Reading bridge — the symmetric hop in the other direction.
  // When AtomDetail (atlas view) clicks "Open in reading" on an
  // evidence row, this effect flips back to chat and asks the
  // readingSession to open the citation. Consumes the pending
  // request so re-entering atlas → chat doesn't replay it.
  $effect(() => {
    const pending = readingNavigation.pendingChunk;
    if (!pending) return;
    readingNavigation.take();
    view = "chat";
    void readingSession.openCitation(
      pending.corpusId,
      pending.chunkId,
      pending.originLabel,
    );
  });

  // Reading surface visibility — driven entirely by the
  // readingSession store. Mutually exclusive with InsightsPanel
  // (the right rail can host at most one).
  let readingOpen = $derived(readingSession.isOpen);
  let atomPanelOpen = $derived(readingSession.isAtomPanelOpen);

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

  // Dismiss the attach-badge after 4 s when everything is fine.
  // The badge stays visible for the initial glance so the user
  // understands the connection mode, then fades out to top-right.
  let badgeDismissed = $state(false);
  $effect(() => {
    if (attachedToDaemon) {
      badgeDismissed = false;
      const t = setTimeout(() => { badgeDismissed = true; }, 4000);
      return () => clearTimeout(t);
    }
  });

  function handleSettingsStarterPick(question: StarterQuestion) {
    chatSeedStore.set(question);
    view = "chat";
  }

  // Workshop sub-tab + deep-link: the recipe-author dashboard's "Run it" sets a
  // recipe name, switches to Workshop, and selects the Run tab (preselected).
  let workshopTab = $state<"build" | "run" | "test" | "connect" | "apps">("build");
  let runWorkflowPreselect = $state<string | null>(null);
  function handleRunWorkflow(name: string) {
    runWorkflowPreselect = name;
    workshopTab = "run";
    view = "workshop";
  }
  // The use→make bridge (D9): a notebook's Settings → open the recipe in
  // the Workshop's Build facet.
  function handleOpenWorkshop() {
    workshopTab = "build";
    view = "workshop";
  }

  /// Called by FolderDropFlow inside Settings → Knowledge when the
  /// user clicks "Start chatting — atlas keeps building". The sample
  /// atlas is still running; the toast will fire when it completes,
  /// and ChatView's empty-state chips will populate from
  /// `enrich_get_starter_questions` in the meantime.
  function handleDropToChat() {
    view = "chat";
  }

  onMount(async () => {
    // Reading surface "View conversation" → switch the chat sidebar.
    // Wired here (not in the store) so the store stays free of
    // direct chat coupling. The opener is reset on destroy below
    // to avoid the closure outliving its state owner.
    readingSession.setConversationOpener((conversationId: string) => {
      handleConversationSelect(conversationId);
    });

    await initEventListeners({
      onBackendReady: () => {
        backendReady = true;
        backendError = null;
        if (view === "loading") {
          view = "chat";
        }
      },
      onBackendError: (error) => {
        backendError = error;
      },
      onSetupRequired: () => {
        // Backend signals setup is needed — go to the welcome
        // threshold rather than straight into the wizard.
        view = "welcome";
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
      onOpenOuterWork: (payload) => {
        // A mesh-app Door card (Wrapped) asked for "ask your past self":
        // fresh conversation, retrieval scoped to the app's corpus.
        // ChatView consumes the scope once the empty pane mounts.
        outerWorkScopeStore.set([payload.corpus_id]);
        handleConversationSelect(null);
        view = "chat";
      },
    });

    // Check if setup is already complete.
    try {
      const complete = await isSetupComplete();
      if (!complete) {
        // First launch: show the threshold screen before the wizard.
        view = "welcome";
      } else if (view === "loading" && (await isBackendReady())) {
        // Race guard for a MISSED `backend-ready`. That event is a
        // push-only Tauri emit with no replay (the sticky buffer in
        // command_bridge.rs only serves the Playwright harness). In
        // Attach mode bootstrap completes in ~1.4s and can fire the
        // emit before this webview finished subscribing (the
        // `initEventListeners` await just above) — the event is then
        // lost and we hang on the loading splash forever. Now that our
        // listener IS wired, re-probe readiness directly; if the backend
        // is already up, adopt the ready state the missed event would
        // have delivered. Ordering is fully covered: if the emit hasn't
        // happened yet, `state.runtime` is still None here (returns
        // false) and the wired listener catches the imminent emit.
        backendReady = true;
        backendError = null;
        view = "chat";
      }
      // If complete but not yet ready, the backend-ready listener wired
      // above flips us to chat when bootstrap finishes.
    } catch {
      // Backend not ready yet — stay on loading (the event will arrive).
    }

    // Boot readiness safety net. The one-shot re-probe above closes the
    // common fast-boot race; this poll covers everything else. Because
    // `backend-ready`/`backend-error` are push-only with no replay, a
    // missed event would otherwise hang the splash indefinitely — polling
    // bounds that to one interval. It self-cancels the instant we leave
    // `loading` (event won the race, re-probe caught it, or setup routed
    // us to welcome). After STALL_MS with no readiness, flag `bootStalled`
    // so the splash offers a manual retry instead of a forever spinner;
    // the poll keeps running, so a late-arriving backend still promotes us.
    const POLL_MS = 2000;
    const STALL_MS = 45000;
    let waited = 0;
    bootPoll = setInterval(async () => {
      if (view !== "loading") {
        clearInterval(bootPoll);
        bootPoll = undefined;
        return;
      }
      try {
        if (await isBackendReady()) {
          backendReady = true;
          backendError = null;
          bootStalled = false;
          view = "chat";
          clearInterval(bootPoll);
          bootPoll = undefined;
          return;
        }
      } catch {
        // Command not reachable yet — keep waiting.
      }
      waited += POLL_MS;
      if (waited >= STALL_MS) bootStalled = true;
    }, POLL_MS);

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
    // complete_setup_auto already bootstrapped the backend before
    // returning, so we can go directly to chat rather than waiting
    // for backend-ready. (backend-ready fires before the command
    // returns, so it would be missed if we flipped to "loading" here.)
    backendReady = true;
    // Seed two starter conversations on truly-first launch. The
    // helper is a no-op if the user already has any conversations.
    try {
      await ensureSeededConversations();
    } catch (e) {
      // Non-fatal — chat still opens, just without seed conversations.
      console.warn("ensureSeededConversations failed:", e);
    }
    // First-mesh-join consent gate (W4). When the user hasn't yet
    // recorded a decision, surface the ConsentGate before chat. Any
    // failure (daemon briefly unreachable) falls through to chat —
    // the gate will re-appear on the next launch if consent is
    // genuinely missing.
    let needsConsent = false;
    try {
      const consent = await getFirstMeshConsent();
      needsConsent = consent === null;
    } catch (e) {
      console.warn("getFirstMeshConsent failed:", e);
    }
    view = needsConsent ? "consent" : "chat";
    // Background install of the default Wikipedia Core corpus — ONLY when
    // the user opted in on the Setup Plan screen. Never a silent,
    // unconsented download (that was the old behaviour this redesign fixes).
    // Idempotent on the daemon; its progress is surfaced in chat via the
    // corpus-progress store. Errors are non-fatal — Knowledge is in Settings.
    if (installStarterCorpus) {
      void startDefaultCorpusInstall().catch(() => {});
    }
  }

  function handleConsentRecorded() {
    view = "chat";
  }

  // Manual escape from a stalled boot. Re-probe once; if the backend came
  // up (we just missed the event), promote to chat. Otherwise reload the
  // webview to re-run the whole mount + listener-wiring path — the cheapest
  // way to recover a webview that lost the handshake.
  async function handleBootRetry() {
    bootStalled = false;
    try {
      if (await isBackendReady()) {
        backendReady = true;
        backendError = null;
        view = "chat";
        return;
      }
    } catch {
      // fall through to reload
    }
    window.location.reload();
  }

  function handleConversationSelect(id: string | null) {
    selectedConversationId = id;
    if (view === "settings") view = "chat";
  }

  function clearTaskState() {
    taskSteps = [];
    // Approval + input cards clear themselves when the user actually
    // submits/skips — leaving them pending across a task switch
    // (e.g. user navigates away mid-approval) is the right default.
    // If we later want "conversation switch cancels pending" we can
    // add a CLEAR_ALL event to approvalMachine.
  }

  // bind:this requires `$state` in Svelte 5 runes mode — without
  // it, the bound reference doesn't propagate and
  // `conversationListRef?.loadConversations?.()` becomes a silent
  // no-op (the sidebar would never refresh after a chat-side
  // auto-bind). Vite logs `non_reactive_update` when this is wrong.
  let conversationListRef: ConversationList | null = $state(null);

  function handleConversationCreated(id: string) {
    // ChatView auto-created a conversation — update the sidebar
    // and select it so the user can navigate back to it.
    selectedConversationId = id;
    conversationListRef?.loadConversations?.();
  }

  function handleRailNavigate(mode: RailMode) {
    // Close reading surface when leaving outer work to keep layout clean.
    if (mode !== "chat" && readingSession.isOpen) {
      readingSession.closeReading();
    }
    // Tapping the already-active inner work icon toggles the history drawer.
    if (mode === "inner_work" && view === "inner_work") {
      innerWorkHistoryToggle++;
      return;
    }
    view = mode;
  }

  function handleGlobalKeydown(e: KeyboardEvent) {
    if (!showNavRail) return;
    if (!e.metaKey && !e.ctrlKey) return;
    switch (e.key) {
      case "1":
        e.preventDefault();
        handleRailNavigate("chat");
        break;
      case "2":
        e.preventDefault();
        handleRailNavigate("library");
        break;
      case "3":
        e.preventDefault();
        handleRailNavigate("inner_work");
        break;
      case "4":
        e.preventDefault();
        handleRailNavigate("workshop");
        break;
      case "5":
        e.preventDefault();
        handleRailNavigate("settings");
        break;
      case "[":
        e.preventDefault();
        if (view === "inner_work") {
          innerWorkHistoryToggle++;
        } else {
          convListCollapsed = !convListCollapsed;
        }
        break;
    }
  }

  // Drop the opener on destroy so a stale closure can't survive
  // teardown (matters most under HMR — without this, repeated
  // component swaps stack a chain of dead callbacks that all fire).
  onDestroy(() => {
    readingSession.setConversationOpener(null);
    if (bootPoll) clearInterval(bootPoll);
  });
</script>

<svelte:window onkeydown={handleGlobalKeydown} />

{#if view === "loading"}
  <div class="loading-screen">
    <div class="loading-ambient"></div>
    <div class="loading-content">
      <div class="mark-wrap" aria-hidden="true">
        <!-- Pentagon-shaped pulse rings, matching the brand mark's
             actual silhouette (V1-V5 vertices from icon-source.svg).
             Replaces the prior circular rings, which felt borrowed
             when the mark itself is asymmetric. Three staggered
             outlines expand from the icon's centroid, fading as
             they reach ~2.2× scale. Stroke is lavender (the brand's
             "signal travels" hue) at low alpha so the gold mark
             stays the eye's anchor. -->
        <svg class="pulse-pent" viewBox="0 0 1024 1024" preserveAspectRatio="xMidYMid meet">
          <polygon class="pent pent-1" points="490,160 830,350 700,830 240,860 170,410" />
          <polygon class="pent pent-2" points="490,160 830,350 700,830 240,860 170,410" />
          <polygon class="pent pent-3" points="490,160 830,350 700,830 240,860 170,410" />
        </svg>
        <div class="loading-mark">
          <BrandMark size={72} />
        </div>
      </div>
      <h1>SVRNMESH</h1>
      <p class="loading-tagline">ai for the rest of us</p>
      {#if backendError}
        <p class="error">{backendError}</p>
      {:else if bootStalled}
        <p class="loading-text stalled">
          Still initializing — this is taking longer than usual.
        </p>
        <button class="boot-retry" onclick={handleBootRetry}>Retry</button>
      {:else}
        <div class="loading-progress">
          <div class="loading-bar"></div>
        </div>
        <p class="loading-text">Initializing</p>
      {/if}
    </div>
  </div>
{:else if view === "welcome"}
  <WelcomeThreshold onBegin={() => (view = "setup_plan")} />
{:else if view === "setup_plan"}
  <SetupPlan
    onConfirm={({ installStarterCorpus: optIn, primaryFile, primarySource }) => {
      installStarterCorpus = optIn;
      chosenPrimaryFile = primaryFile;
      chosenPrimarySource = primarySource;
      view = "setup";
    }}
    onBack={() => (view = "welcome")}
  />
{:else if view === "setup"}
  <SetupFlow
    onComplete={handleSetupComplete}
    primaryFile={chosenPrimaryFile}
    primarySource={chosenPrimarySource}
  />
{:else if view === "consent"}
  <ConsentGate onChoice={handleConsentRecorded} />
{:else}
  <!-- Post-onboarding chrome shell: rail + content area side by side -->
  <div class="app-chrome">
    <NavRail active={railMode} onNavigate={handleRailNavigate} />
    <div class="app-chrome-content">
      <!-- InnerWork keep-alive layer: mounted on first visit, shown/hidden
           via CSS so the mount lifecycle (skill snapshot, conversation
           lookup) only runs once regardless of how many times the user
           toggles between modes. -->
      {#if innerWorkMounted}
        <div
          class="inner-work-layer"
          class:active={view === "inner_work"}
          aria-hidden={view !== "inner_work"}
        >
          <InnerWorkSurface
            historyToggle={innerWorkHistoryToggle}
            active={view === "inner_work"}
          />
        </div>
      {/if}

      {#if view === "workshop"}
        <WorkshopView
          tab={workshopTab}
          onTabChange={(t) => (workshopTab = t)}
          onExit={() => (view = "chat")}
          onUseInChat={handleSettingsStarterPick}
          onOpenChat={handleDropToChat}
          onRunWorkflow={handleRunWorkflow}
          runPreselect={runWorkflowPreselect}
        />
      {:else if view === "library"}
        <div class="library-surface">
          <LibraryView
            onOpenChatWithSeed={handleSettingsStarterPick}
            onDropToChat={handleDropToChat}
            onOpenWorkshop={handleOpenWorkshop}
          />
        </div>
      {:else if view === "atlas"}
        <div class="atlas-surface">
          <AtlasSurface />
        </div>
      {:else if view === "settings"}
        <div class="settings-surface">
          <SettingsPanel
            onClose={() => {
              view = "chat";
            }}
          />
        </div>
      {:else}
        <div
          class="app-layout"
          class:reading-open={readingOpen}
          class:atom-open={atomPanelOpen}
          class:convlist-collapsed={convListCollapsed}
        >
          <aside class="sidebar">
            <ConversationList
              bind:this={conversationListRef}
              {selectedConversationId}
              onSelect={handleConversationSelect}
            />
          </aside>
          <main class="main-content">
            <ChatView
              conversationId={selectedConversationId}
              {taskSteps}
              onClearTask={clearTaskState}
              onOpenLibrary={() => handleRailNavigate("library")}
              onConversationCreated={handleConversationCreated}
            />
          </main>
          {#if readingOpen}
            <ReadingSurface />
            {#if atomPanelOpen}
              <AtomPanel />
            {/if}
          {:else if showInsights}
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
    </div>
  </div>
{/if}

<ToastHost />

<!-- Global supervisor banner. Listens for `supervisor-state` events
     from the Rust supervisor and renders only when the daemon is
     restarting / unhealthy / failed (silent for healthy + starting).
     Visible across every view, including setup/welcome — a daemon
     crash mid-setup deserves the same recovery surface. -->
<ReconnectBanner />

<!-- Boot-time notice when the configured chat model can't run on this
     machine's CPU and a dense model was substituted (see model_compat.rs).
     Informational + dismissible — the graceful alternative to a first-query
     crash on an incompatible architecture. -->
<ModelNoticeBanner />

{#if attachedToDaemon}
  <!-- Pill anchored top-right; shows briefly on startup then fades out.
       Re-appears if the user triggers a recheck (future: daemon health poll). -->
  <button
    class="attach-badge"
    class:dismissed={badgeDismissed}
    title="Using daemon started by 'svrn daemon run' on :{bootstrap?.client_port ?? 9741}. Click to re-show."
    aria-live="polite"
    onclick={() => { badgeDismissed = false; setTimeout(() => { badgeDismissed = true; }, 4000); }}
  >
    <span class="attach-dot" aria-hidden="true"></span>
    connected to daemon · :{bootstrap?.client_port ?? 9741}
  </button>
{/if}

{#if pendingJoinLink}
  <MeshJoinDialog
    link={pendingJoinLink}
    onClose={() => joinLinkStore.clear()}
    onJoined={(meshName) => {
      joinLinkStore.clear();
      // Tell the settings surface the membership changed: an
      // already-mounted MeshSettings re-pulls state immediately, and
      // SettingsPanel lands on the Mesh tab instead of its default.
      meshMembership.noteJoined();
      view = "settings";
      toastStore.notify({
        title: `Joined "${meshName}"`,
        body: "You're connected — mesh members appear as they come online.",
      });
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

  /* ── Pentagon-shaped pulse — mesh signal broadcast.
        Sized to host BrandMark at 72px plus enough headroom for
        the outline to expand ~2.2× without clipping. Overflow
        visible by default; left commented for posterity. ──     */
  .mark-wrap {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 96px;
    height: 96px;
    margin-bottom: 22px;
  }

  /* Inline SVG holding the three staggered pentagon outlines.
     viewBox matches icon-source.svg so the polygon coordinates
     line up with the gold mark sitting on top. Sized to the
     wrap; outlines transform-origin against the icon's centroid
     (470,540) — the visual center of the asymmetric pentagon. */
  .pulse-pent {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
    pointer-events: none;
  }
  .pent {
    fill: none;
    stroke: rgba(155, 135, 196, 0.42);
    stroke-width: 14;
    stroke-linejoin: round;
    /* transform-box: fill-box re-anchors the polygon's
       transform-origin to its own bbox; combined with
       transform-origin: center, the pentagon scales from its
       geometric center rather than the SVG viewBox origin. */
    transform-box: fill-box;
    transform-origin: center;
    animation: pent-expand 3s ease-out infinite;
  }
  .pent-2 { animation-delay: 1s; }
  .pent-3 { animation-delay: 2s; }

  @keyframes pent-expand {
    0%   { transform: scale(1);   opacity: 0.55; }
    100% { transform: scale(2.2); opacity: 0; }
  }

  /* ── Attach-mode badge ── */
  .attach-badge {
    position: fixed;
    top: 12px;
    right: 12px;
    z-index: 40;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 5px 11px;
    font-size: 0.7rem;
    font-family: var(--font-mono);
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: 999px;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.25);
    cursor: pointer;
    user-select: none;
    opacity: 1;
    transform: translateY(0);
    transition: opacity 0.5s ease, transform 0.5s ease;
  }

  .attach-badge.dismissed {
    opacity: 0;
    transform: translateY(-6px);
    pointer-events: none;
  }

  .attach-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--growth);
    box-shadow: 0 0 6px rgba(121, 196, 120, 0.8);
  }

  .loading-mark {
    /* Hosts the BrandMark SVG. The bare SVG carries its own
       drop-shadow; we layer a stronger gold glow here for the
       splash hero and run the breathe animation on the wrapper so
       the rings + mark pulse together. */
    display: inline-flex;
    line-height: 1;
    filter: drop-shadow(0 0 22px rgba(201, 168, 76, 0.45));
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

  .loading-text.stalled {
    text-transform: none;
    letter-spacing: 0.02em;
    color: var(--text-secondary);
    max-width: 320px;
    text-align: center;
    line-height: 1.5;
    margin-bottom: 16px;
  }

  .boot-retry {
    padding: 7px 18px;
    font-size: 0.78rem;
    font-family: var(--font-mono);
    letter-spacing: 0.04em;
    color: var(--text-primary);
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: 999px;
    cursor: pointer;
    transition: border-color 0.2s ease, background 0.2s ease;
  }

  .boot-retry:hover {
    border-color: var(--accent);
    background: var(--bg-secondary);
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

  /* ── Chrome shell ──
     NavRail (60px) sits as a fixed left column in a flex row. The
     content area takes the rest of the viewport and is `position:
     relative` so InnerWorkSurface (position: absolute) fills it
     correctly — viewport minus the rail — without needing fixed
     coordinates. */
  .app-chrome {
    display: flex;
    height: 100vh;
    overflow: hidden;
  }

  .app-chrome-content {
    flex: 1;
    overflow: hidden;
    position: relative;
    min-width: 0;
  }

  /* ── App shell ──
     Three-column grid for the glass-box reading layout. The
     reading column collapses to 0 when no citation is open, so
     the chat column expands to fill the available width — same
     behavior as the previous flex layout. When `reading-open` is
     toggled, the chat column shrinks and the reading column
     slides in. Animation is on grid-template-columns; modern
     Chromium / Safari interpolates between fr units smoothly. */
  .app-layout {
    display: grid;
    grid-template-columns: 262px 1fr 0 0;
    height: 100%;
    transition: grid-template-columns 220ms cubic-bezier(.2, .8, .2, 1);
  }

  .app-layout.reading-open {
    grid-template-columns: 262px minmax(360px, 1fr) minmax(440px, 2fr) 0;
  }

  /* Four-column state — atom panel slides in as the rightmost
     column; the reading column shrinks to make room. */
  .app-layout.reading-open.atom-open {
    grid-template-columns:
      262px
      minmax(320px, 1fr)
      minmax(360px, 1.4fr)
      minmax(300px, 1fr);
  }

  /* Conversation list collapse — zero the sidebar column */
  .app-layout.convlist-collapsed {
    grid-template-columns: 0 1fr 0 0;
  }

  .app-layout.convlist-collapsed > .sidebar {
    display: none;
  }

  .app-layout.convlist-collapsed.reading-open {
    grid-template-columns: 0 minmax(360px, 1fr) minmax(440px, 2fr) 0;
  }

  .app-layout.convlist-collapsed.reading-open.atom-open {
    grid-template-columns:
      0
      minmax(320px, 1fr)
      minmax(360px, 1.4fr)
      minmax(300px, 1fr);
  }

  @media (max-width: 1280px) {
    /* Smaller windows: atom panel becomes an overlay over the
       reading column instead of displacing it. The AtomPanel is
       absolutely positioned via the inline shadow and right-rail
       border; CSS Grid still tracks 0-width but visually it
       overlays. */
    .app-layout.reading-open.atom-open {
      grid-template-columns: 262px minmax(320px, 1fr) minmax(380px, 1.4fr) 0;
    }
  }

  @media (max-width: 1100px) {
    .app-layout.reading-open {
      grid-template-columns: 220px minmax(320px, 1fr) minmax(380px, 1.4fr) 0;
    }
  }

  /* ── Narrow-window collapse ──
     Below 880px there isn't room for chat + reading side-by-side
     comfortably. The reading surface becomes a full-width slide-
     over above the chat column; chat stays mounted underneath
     (state preserved) but visually hidden. The atom panel collapses
     into a bottom sheet within the overlay so it's still
     reachable. */
  @media (max-width: 880px) {
    .app-layout.reading-open,
    .app-layout.reading-open.atom-open {
      grid-template-columns: 262px 1fr 0 0;
    }
    /* Anchor the reading column outside the grid flow so it
       overlays chat without breaking the grid track widths. */
    .app-layout.reading-open > :global(.reading-surface) {
      position: fixed;
      top: 0;
      right: 0;
      bottom: 0;
      left: 262px;
      z-index: 30;
      background: var(--bg-primary);
    }
    /* Atom panel sheet — anchored to the bottom-right inside the
       overlay so the user can dismiss it without losing the
       reading context. */
    .app-layout.reading-open.atom-open > :global(.atom-panel) {
      position: fixed;
      bottom: 0;
      right: 0;
      width: min(420px, 100vw - 16px);
      max-height: 60vh;
      z-index: 40;
      box-shadow: -4px -4px 18px rgba(0, 0, 0, 0.32);
      border-radius: 12px 0 0 0;
    }
  }

  /* Very narrow (mobile-like) — collapse the sidebar too. The
     conversation list is reachable via the back action; reading
     surface takes the full window. */
  @media (max-width: 600px) {
    .app-layout {
      grid-template-columns: 0 1fr 0 0;
    }
    .app-layout > .sidebar {
      display: none;
    }
    .app-layout.reading-open > :global(.reading-surface) {
      left: 0;
    }
    /* Hide the rail at very narrow widths too */
    .app-chrome > :global(.nav-rail) {
      display: none;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .app-layout {
      transition: none;
    }
  }

  .sidebar {
    width: 262px;
    min-width: 0;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border-mid);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .main-content {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-primary);
    min-width: 0;
  }

  /* InnerWork keep-alive: sits as an absolute layer inside the
     content area. Hidden by default; `.active` makes it visible.
     `display: none` means zero layout/paint cost when not active. */
  .inner-work-layer {
    position: absolute;
    inset: 0;
    display: none;
    z-index: 1;
  }

  .inner-work-layer.active {
    display: block;
  }

  /* Settings renders outside the chat grid so it gets full width
     with no sidebar. Provides the same flex column context that
     SettingsPanel expects from its former main-content parent. */
  .settings-surface {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    background: var(--bg-primary);
  }

  /* Library surface — full-width knowledge home, no chat sidebar. The
     shelf, Add sheet, and notebook detail own their own internal layout
     and scrolling, so this is a plain full-height flex host. */
  .library-surface {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    background: var(--bg-primary);
  }

  /* Atlas surface — full-width inspection view, no sidebar. AtlasIndex
     and its descendants (corpus list in Step 2, browse view in Step 3,
     atom detail in Step 4) own their own internal layout. Scrolls
     internally so the header stays in view. */
  .atlas-surface {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow-y: auto;
    background: var(--bg-primary);
  }

</style>
