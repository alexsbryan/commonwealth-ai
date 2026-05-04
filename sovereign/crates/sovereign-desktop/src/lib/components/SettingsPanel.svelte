<script lang="ts">
  import { onMount } from "svelte";
  import {
    detectBootstrap,
    getConfig,
    saveConfig,
    getIngestBudget,
    setIngestBudget,
    getMeshQuiesced,
    setMeshQuiesced,
    getStorageBudget,
    setStorageBudget,
  } from "../api";
  import type { StorageBudgetState } from "../api";
  import type {
    BootstrapSnapshot,
    DesktopConfig,
    StarterQuestion,
  } from "../types";
  import EnrichmentPanel from "./EnrichmentPanel.svelte";
  import KnowledgeStatus from "./KnowledgeStatus.svelte";
  import LocalKnowledgeSection from "./local-knowledge/LocalKnowledgeSection.svelte";
  import MeshSettings from "./MeshSettings.svelte";
  import SkillManager from "./SkillManager.svelte";
  import ModelSelector from "../setup/ModelSelector.svelte";
  import RecipeTestingPanel from "./RecipeTestingPanel.svelte";

  interface Props {
    onClose: () => void;
    /// Piped from App.svelte: when LocalKnowledge's atlas-complete
    /// screen fires a starter-chip click, close Settings + seed chat.
    onOpenChatWithSeed?: (question: StarterQuestion) => void;
    /// Piped from App.svelte: "Start chatting — atlas keeps
    /// building" from the sample-atlas progress screen.
    onDropToChat?: () => void;
  }

  let { onClose, onOpenChatWithSeed, onDropToChat }: Props = $props();

  type Tab =
    | "models"
    | "knowledge"
    | "enrichment"
    | "local-knowledge"
    | "mesh"
    | "tools"
    | "paths"
    | "recipes";
  let activeTab: Tab = $state("models");

  let config: DesktopConfig | null = $state(null);
  let saving = $state(false);
  let saveMessage = $state("");
  let dirty = $state(false);
  // Bootstrap snapshot surfaces whether we're attached to an
  // externally-managed daemon. When true, the Models tab shows a
  // note that port/data-dir changes need `sovereign setup` (not
  // the in-process Settings panel).
  let bootstrap = $state<BootstrapSnapshot | null>(null);
  let attachedToDaemon = $derived(bootstrap?.daemon_running === true);

  // ── Ingest pressure controls ──────────────────────────────────
  // Live values are read from the daemon and pushed back via
  // `/internal/ingest/budget` and `/internal/mesh/quiesce`. They are
  // NOT part of `config` (no Save needed) — the daemon is the source
  // of truth and the slider applies on release.
  let ingestThrottle = $state<number>(1.0);
  let meshQuiesced = $state<boolean>(false);
  let ingestStatusMessage = $state<string>("");

  // ── Storage budget ────────────────────────────────────────────
  // Same pattern: daemon owns the live value, the desktop persists
  // the user choice in `desktop.toml` so it survives restart. The
  // daemon-side `get_storage_budget` Tauri command auto-seeds a
  // recommended default on first launch (when neither the config
  // nor the running daemon has a budget) so the user never sees a
  // blank "no budget" state without an explicit choice.
  let storageBudget = $state<StorageBudgetState | null>(null);
  // Pending GiB the user has typed but not yet applied. `null` means
  // "use whatever budget says" (no pending edit). Apply happens
  // explicitly via the button so an in-progress number doesn't push
  // a value the user is still typing.
  let storageDraftGib = $state<number | null>(null);
  let storageStatusMessage = $state<string>("");
  // Bytes ↔ GiB helpers. Use binary GiB throughout so the math
  // matches the daemon's `1_073_741_824` divisor.
  const BYTES_PER_GIB = 1_073_741_824;
  function bytesToGib(b: number): number {
    return b / BYTES_PER_GIB;
  }
  function gibToBytes(g: number): number {
    return Math.round(g * BYTES_PER_GIB);
  }
  function fmtGib(b: number, digits = 1): string {
    return `${bytesToGib(b).toFixed(digits)} GiB`;
  }
  // Usage / budget percent. Capped at 100 so the bar saturates rather
  // than overflowing when used > budget (which can happen if the
  // user dropped the budget below current usage — the daemon will
  // then refuse new shards but the existing data stays put).
  let usagePercent = $derived.by(() => {
    if (!storageBudget?.budget_bytes) return 0;
    return Math.min(100, (storageBudget.used_bytes / storageBudget.budget_bytes) * 100);
  });
  // What the bar means colour-wise. ≥95% = "near limit" — the
  // scheduler is about to refuse new shards. We don't make this
  // an error because the user *chose* the limit; it's a heads-up.
  let usageState = $derived.by(() => {
    if (usagePercent >= 100) return "over";
    if (usagePercent >= 95) return "near";
    return "ok";
  });

  onMount(async () => {
    try {
      config = await getConfig();
    } catch (e) {
      console.error("Failed to load config:", e);
    }
    try {
      bootstrap = await detectBootstrap();
    } catch {
      bootstrap = null;
    }
    try {
      const budget = await getIngestBudget();
      ingestThrottle = budget.throttle_factor;
    } catch (e) {
      console.warn("Failed to load ingest budget (daemon offline?):", e);
    }
    try {
      const q = await getMeshQuiesced();
      meshQuiesced = q.quiesced;
    } catch (e) {
      console.warn("Failed to load mesh quiesce state:", e);
    }
    try {
      storageBudget = await getStorageBudget();
    } catch (e) {
      console.warn("Failed to load storage budget (daemon offline?):", e);
    }
  });

  async function applyStorageBudget(bytes: number | null) {
    storageStatusMessage = "";
    try {
      storageBudget = await setStorageBudget(bytes);
      storageDraftGib = null;
    } catch (e) {
      storageStatusMessage = `Could not update budget: ${e}`;
    }
  }
  async function applyRecommendedStorageBudget() {
    if (!storageBudget) return;
    await applyStorageBudget(storageBudget.recommended_bytes);
  }
  async function clearStorageBudget() {
    await applyStorageBudget(null);
  }
  async function applyDraftStorageBudget() {
    if (storageDraftGib === null) return;
    if (!Number.isFinite(storageDraftGib) || storageDraftGib < 1) {
      storageStatusMessage = "Budget must be at least 1 GiB.";
      return;
    }
    await applyStorageBudget(gibToBytes(storageDraftGib));
  }

  /// Discrete slider positions. Off (1.0) is full speed — the
  /// daemon's default and what most users want when they're not
  /// actively using the machine. The lower stops are for "share the
  /// machine over a long ingest." Granularity beyond 4 stops is
  /// noise; the GPU's batch latency on a real Wikipedia ingest
  /// drives the practical floor.
  const THROTTLE_PRESETS: Array<{ value: number; label: string; desc: string }> = [
    { value: 1.00, label: "Off",    desc: "Full speed. The default — ingest uses every available cycle." },
    { value: 0.75, label: "Light",  desc: "75% duty cycle. Barely noticeable; small headroom for other work." },
    { value: 0.50, label: "Balanced", desc: "50% duty cycle. Ingest takes about twice as long; the machine stays usable." },
    { value: 0.25, label: "Quiet",  desc: "25% duty cycle. Ingest runs slowly in the background while you do other things." },
  ];

  let throttlePreset = $derived.by(() => {
    const exact = THROTTLE_PRESETS.find((p) => Math.abs(p.value - ingestThrottle) < 0.02);
    return exact?.value ?? null;
  });

  async function applyThrottle(value: number) {
    ingestStatusMessage = "";
    try {
      const result = await setIngestBudget(value);
      ingestThrottle = result.throttle_factor;
    } catch (e) {
      ingestStatusMessage = `Could not update throttle: ${e}`;
    }
  }

  async function applyQuiesce(value: boolean) {
    ingestStatusMessage = "";
    try {
      const result = await setMeshQuiesced(value);
      meshQuiesced = result.quiesced;
    } catch (e) {
      ingestStatusMessage = `Could not update mesh participation: ${e}`;
    }
  }

  function markDirty() {
    dirty = true;
    saveMessage = "";
  }

  async function handleSave() {
    if (!config || saving) return;
    saving = true;
    saveMessage = "";
    try {
      await saveConfig(config);
      saveMessage = "Saved. Sovereign will use these settings on the next message.";
      dirty = false;
    } catch (e) {
      saveMessage = `Could not save: ${e}`;
    }
    saving = false;
  }

  let needsSave = $derived(
    activeTab === "models" || activeTab === "paths" || activeTab === "knowledge",
  );

  // ── Semantic preset detection ──────────────────────────────────

  type CreativityPreset  = "precise" | "balanced" | "exploratory" | "custom";
  type ReasoningPreset   = "quick" | "balanced" | "thorough" | "exhaustive" | "custom";
  type LengthPreset      = "concise" | "standard" | "detailed" | "exhaustive" | "custom";

  let creativityPreset = $derived.by((): CreativityPreset => {
    if (!config) return "balanced";
    const { temperature: t, top_k: k } = config;
    if (t === 0.3 && k === 10)  return "precise";
    if (t === 0.6 && k === 20)  return "balanced";
    if (t === 1.0 && k === 40)  return "exploratory";
    return "custom";
  });

  let reasoningPreset = $derived.by((): ReasoningPreset => {
    if (!config) return "balanced";
    const b = config.think_budget;
    if (b === 0)     return "quick";
    if (b === 4096)  return "balanced";
    if (b === 16384) return "thorough";
    if (b === 38000) return "exhaustive";
    return "custom";
  });

  let lengthPreset = $derived.by((): LengthPreset => {
    if (!config) return "standard";
    const m = config.max_tokens;
    if (m === 512)   return "concise";
    if (m === 2048)  return "standard";
    if (m === 6144)  return "detailed";
    if (m === 16384) return "exhaustive";
    return "custom";
  });

  function setCreativity(preset: Exclude<CreativityPreset, "custom">) {
    if (!config) return;
    const map = {
      precise:     [0.3,  10 ] as [number, number],
      balanced:    [0.6,  20 ] as [number, number],
      exploratory: [1.0,  40 ] as [number, number],
    };
    [config.temperature, config.top_k] = map[preset];
    markDirty();
  }

  function setReasoning(preset: Exclude<ReasoningPreset, "custom">) {
    if (!config) return;
    const map = { quick: 0, balanced: 4096, thorough: 16384, exhaustive: 38000 };
    config.think_budget = map[preset];
    markDirty();
  }

  function setLength(preset: Exclude<LengthPreset, "custom">) {
    if (!config) return;
    const map = { concise: 512, standard: 2048, detailed: 6144, exhaustive: 16384 };
    config.max_tokens = map[preset];
    markDirty();
  }

  const CREATIVITY_OPTS = [
    { id: "precise"     as const, label: "Precise",     desc: "Consistent, deterministic. Best for facts, code, structured output.", tech: "temp 0.3 · top_k 10" },
    { id: "balanced"    as const, label: "Balanced",    desc: "Coherent but not mechanical. Natural variation in phrasing.",          tech: "temp 0.6 · top_k 20" },
    { id: "exploratory" as const, label: "Exploratory", desc: "More surprising angles. Higher hallucination risk on factual tasks.",  tech: "temp 1.0 · top_k 40" },
  ];

  const REASONING_OPTS = [
    { id: "quick"      as const, label: "Quick",      desc: "Direct answer. No extended thinking. Lowest latency.",        tech: "budget 0 (disabled)" },
    { id: "balanced"   as const, label: "Balanced",   desc: "Brief reasoning on hard questions, direct on simple ones.",   tech: "budget 4 096 tok" },
    { id: "thorough"   as const, label: "Thorough",   desc: "Extended deliberation before answering. Noticeably slower.",  tech: "budget 16 384 tok" },
    { id: "exhaustive" as const, label: "Exhaustive", desc: "Maximum reasoning. For genuinely hard problems.",             tech: "budget 38 000 tok" },
  ];

  const LENGTH_OPTS = [
    { id: "concise"   as const, label: "Concise",   desc: "Gets to the point. Best for quick questions.",              tech: "max 512 tok" },
    { id: "standard"  as const, label: "Standard",  desc: "Full answers without padding.",                             tech: "max 2 048 tok" },
    { id: "detailed"  as const, label: "Detailed",  desc: "Room for nuance, examples, caveats.",                       tech: "max 6 144 tok" },
    { id: "exhaustive" as const, label: "Exhaustive", desc: "No length constraints. Writes as much as needed.",        tech: "max 16 384 tok" },
  ];

  let activeSlot: "fast" | "reasoning" | "embed" | "code" | null = $state(null);

  function modelFileName(path: string): string {
    return path.split(/[\\/]/).pop() ?? path;
  }

  let slotSelectedPath = $derived.by((): string => {
    if (!config || !activeSlot) return "";
    if (activeSlot === "fast") return config.model_path ?? "";
    if (activeSlot === "reasoning") return config.primary_model_path ?? "";
    if (activeSlot === "code") return config.code_model_path ?? "";
    return config.embed_model_path ?? "";
  });

  function handleSlotSelect(path: string) {
    if (!config || !activeSlot) return;
    if (activeSlot === "fast") config.model_path = path;
    else if (activeSlot === "reasoning") config.primary_model_path = path || null;
    else if (activeSlot === "code") config.code_model_path = path || null;
    else config.embed_model_path = path || null;
    markDirty();
  }

  const tabs: { id: Tab; label: string }[] = [
    { id: "models",          label: "Models"          },
    { id: "knowledge",       label: "Knowledge"       },
    { id: "enrichment",      label: "Enrichment"      },
    { id: "local-knowledge", label: "Local Knowledge" },
    { id: "mesh",            label: "Mesh"            },
    { id: "tools",           label: "Skills"          },
    { id: "paths",           label: "Paths"           },
    { id: "recipes",         label: "Recipes"         },
  ];
</script>

<div class="settings-panel">

  <!-- ── Header ── -->
  <header class="settings-header">
    <span class="settings-title">Settings</span>
    <button class="close-btn" onclick={onClose} aria-label="Close settings">
      <svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true">
        <path d="M1 1l12 12M13 1L1 13" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
      </svg>
    </button>
  </header>

  <div class="settings-layout">

    <!-- ── Left nav ── -->
    <nav class="settings-nav" aria-label="Settings sections">
      {#each tabs as tab}
        <button
          class="nav-item"
          class:active={activeTab === tab.id}
          onclick={() => { activeTab = tab.id; saveMessage = ""; }}
          aria-current={activeTab === tab.id ? "page" : undefined}
        >
          <!-- Tab icons -->
          {#if tab.id === "models"}
            <svg width="15" height="15" viewBox="0 0 15 15" fill="none" aria-hidden="true">
              <rect x="2" y="2" width="11" height="11" rx="1.5" stroke="currentColor" stroke-width="1.3"/>
              <path d="M5 5.5h5M5 7.5h5M5 9.5h3" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/>
            </svg>
          {:else if tab.id === "knowledge"}
            <svg width="15" height="15" viewBox="0 0 15 15" fill="none" aria-hidden="true">
              <path d="M3 2h7l3 3v8H3V2z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
              <path d="M10 2v3h3" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/>
              <path d="M5 7h5M5 9.5h3.5" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/>
            </svg>
          {:else if tab.id === "mesh"}
            <svg width="15" height="15" viewBox="0 0 15 15" fill="none" aria-hidden="true">
              <circle cx="7.5" cy="3"   r="1.8" stroke="currentColor" stroke-width="1.2"/>
              <circle cx="2.5" cy="12"  r="1.8" stroke="currentColor" stroke-width="1.2"/>
              <circle cx="12.5" cy="12" r="1.8" stroke="currentColor" stroke-width="1.2"/>
              <path d="M7.5 4.8L2.5 10.2M7.5 4.8L12.5 10.2" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/>
            </svg>
          {:else if tab.id === "tools"}
            <svg width="15" height="15" viewBox="0 0 15 15" fill="none" aria-hidden="true">
              <circle cx="6" cy="6" r="3.5" stroke="currentColor" stroke-width="1.3"/>
              <path d="M8.5 8.5l4.5 4.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
            </svg>
          {:else if tab.id === "recipes"}
            <svg width="15" height="15" viewBox="0 0 15 15" fill="none" aria-hidden="true">
              <path d="M5 2h5v2l1 1v7H4V5l1-1V2z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
              <path d="M6 2v2h3V2" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/>
              <path d="M6 8h3M6 10.5h2" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/>
            </svg>
          {:else}
            <svg width="15" height="15" viewBox="0 0 15 15" fill="none" aria-hidden="true">
              <path d="M2 4h4l1.5 2H13v7H2V4z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
            </svg>
          {/if}
          {tab.label}
        </button>
      {/each}
    </nav>

    <!-- ── Content ── -->
    <div class="settings-content">
      <div class="tab-body">

        <!-- ──────────────── MODELS ──────────────── -->
        {#if activeTab === "models" && config}

          {#if attachedToDaemon}
            <div class="attach-note" role="note">
              <svg width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
                <circle cx="6.5" cy="6.5" r="5.5" stroke="currentColor" stroke-width="1.2"/>
                <path d="M6.5 4v3.5M6.5 9.5v.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
              </svg>
              <span>
                Daemon managed externally. Model-path changes hot-reload the
                running <code>sovereign daemon</code> in place. Port and data
                directory changes require a daemon restart —
                run <code>sovereign setup</code> in a terminal.
              </span>
            </div>
          {/if}

          <!-- ── Role-based model grid ──────────────────────
               Copy frames each model by what it does for the user,
               not by its internal slot name. "Quick" / "Main" /
               "Knowledge" map 1:1 to the Fast / Primary / Embed
               slots but the user never sees the slot vocabulary. -->
          <div class="model-slots-grid">

            <div class="slot-card" class:slot-card--active={activeSlot === "fast"}>
              <div class="slot-card-head">
                <span class="slot-card-title">Quick responder</span>
                <span class="slot-status-badge">Always on</span>
              </div>
              <p class="slot-card-desc">For short, fast responses — classifications, routing, quick drafts. Stays in memory so it's there the moment you hit send.</p>
              <div class="slot-current">
                {#if config.model_path}
                  <span class="slot-file">{modelFileName(config.model_path)}</span>
                  <div class="slot-btns">
                    <button class="slot-btn" onclick={() => activeSlot = "fast"}>Change</button>
                    <button class="slot-btn slot-btn--clear" onclick={() => { config!.model_path = ""; markDirty(); }}>Clear</button>
                  </div>
                {:else}
                  <span class="slot-empty">No model chosen</span>
                  <button class="slot-btn slot-btn--add" onclick={() => activeSlot = "fast"}>Choose a model</button>
                {/if}
              </div>
            </div>

            <div class="slot-card" class:slot-card--active={activeSlot === "reasoning"}>
              <div class="slot-card-head">
                <span class="slot-card-title">Main responder</span>
                <span class="slot-status-badge slot-status-badge--opt">Loads on demand</span>
              </div>
              <p class="slot-card-desc">Your primary model for substantive work — research, writing, analysis. Loads when you ask something substantive and unloads after ~60 s idle so it's not taking up memory all day.</p>
              <div class="slot-current">
                {#if config.primary_model_path}
                  <span class="slot-file">{modelFileName(config.primary_model_path)}</span>
                  <div class="slot-btns">
                    <button class="slot-btn" onclick={() => activeSlot = "reasoning"}>Change</button>
                    <button class="slot-btn slot-btn--clear" onclick={() => { config!.primary_model_path = null; markDirty(); }}>Clear</button>
                  </div>
                {:else}
                  <span class="slot-empty">No model chosen</span>
                  <button class="slot-btn slot-btn--add" onclick={() => activeSlot = "reasoning"}>Choose a model</button>
                {/if}
              </div>
            </div>

            <div class="slot-card" class:slot-card--active={activeSlot === "embed"}>
              <div class="slot-card-head">
                <span class="slot-card-title">Knowledge embedder</span>
                <span class="slot-status-badge slot-status-badge--req">For your library</span>
              </div>
              <p class="slot-card-desc">Converts text into vectors so your knowledge base and notes become searchable. Runs in the background whenever you ingest documents.</p>
              <div class="slot-current">
                {#if config.embed_model_path}
                  <span class="slot-file">{modelFileName(config.embed_model_path)}</span>
                  <div class="slot-btns">
                    <button class="slot-btn" onclick={() => activeSlot = "embed"}>Change</button>
                    <button class="slot-btn slot-btn--clear" onclick={() => { config!.embed_model_path = null; markDirty(); }}>Clear</button>
                  </div>
                {:else}
                  <span class="slot-empty">No model chosen</span>
                  <button class="slot-btn slot-btn--add" onclick={() => activeSlot = "embed"}>Choose a model</button>
                {/if}
              </div>
            </div>

            <div class="slot-card" class:slot-card--active={activeSlot === "code"}>
              <div class="slot-card-head">
                <span class="slot-card-title">Code specialist</span>
                <span class="slot-status-badge slot-status-badge--opt">Optional</span>
              </div>
              <p class="slot-card-desc">A dedicated coding model (e.g. Qwen-Coder, DeepSeek-Coder). When set, programming questions route here instead of the Main responder. Shares memory with the Main responder — whichever one you need loads on demand.</p>
              <div class="slot-current">
                {#if config.code_model_path}
                  <span class="slot-file">{modelFileName(config.code_model_path)}</span>
                  <div class="slot-btns">
                    <button class="slot-btn" onclick={() => activeSlot = "code"}>Change</button>
                    <button class="slot-btn slot-btn--clear" onclick={() => { config!.code_model_path = null; markDirty(); }}>Clear</button>
                  </div>
                {:else}
                  <span class="slot-empty">No model chosen</span>
                  <button class="slot-btn slot-btn--add" onclick={() => activeSlot = "code"}>Choose a model</button>
                {/if}
              </div>
            </div>

          </div>

          {#if !config.embed_model_path}
            <div class="inline-notice">
              <svg width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
                <circle cx="6.5" cy="6.5" r="5.5" stroke="currentColor" stroke-width="1.2"/>
                <path d="M6.5 4v3.5M6.5 9.5v.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
              </svg>
              No knowledge embedder chosen. Your library can't be searched until one is set.
            </div>
          {/if}

          <!-- ── Full-width model picker ── -->
          {#if activeSlot}
            <div class="model-picker-row">
              <div class="picker-head">
                <span class="picker-label">
                  {#if activeSlot === "fast"}Quick responder{:else if activeSlot === "reasoning"}Main responder{:else if activeSlot === "code"}Code specialist{:else}Knowledge embedder{/if}
                </span>
                <button class="picker-done" onclick={() => activeSlot = null}>Done</button>
              </div>
              <div class="picker-body">
                <ModelSelector
                  selectedPath={slotSelectedPath}
                  onSelect={handleSlotSelect}
                  showRawInput={true}
                  embedMode={activeSlot === "embed"}
                />
              </div>
            </div>
          {/if}

          <!-- ── Creativity ── -->
          <p class="section-label">Creativity</p>
          <p class="axis-question">How predictable should responses be?</p>
          <div class="preset-row">
            {#each CREATIVITY_OPTS as opt}
              <button
                class="preset-btn"
                class:preset-btn--active={creativityPreset === opt.id}
                onclick={() => setCreativity(opt.id)}
              >{opt.label}</button>
            {/each}
            {#if creativityPreset === "custom"}
              <span class="preset-custom">Custom</span>
            {/if}
          </div>
          {#each CREATIVITY_OPTS as opt}
            {#if creativityPreset === opt.id}
              <p class="preset-desc">{opt.desc}</p>
              <p class="preset-tech">{opt.tech}</p>
            {/if}
          {/each}
          {#if creativityPreset === "exploratory"}
            <div class="inline-notice" style="margin-top: 8px;">
              <svg width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
                <circle cx="6.5" cy="6.5" r="5.5" stroke="currentColor" stroke-width="1.2"/>
                <path d="M6.5 4v3.5M6.5 9.5v.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
              </svg>
              More creative, but more likely to confidently say wrong things. Not recommended for research.
            </div>
          {/if}

          <!-- ── Reasoning Effort ── -->
          <p class="section-label" style="margin-top: 22px;">Reasoning Effort</p>
          <p class="axis-question">How carefully should the model think before answering? <span class="axis-scope">Complex questions only.</span></p>
          <div class="preset-row">
            {#each REASONING_OPTS as opt}
              <button
                class="preset-btn"
                class:preset-btn--active={reasoningPreset === opt.id}
                onclick={() => setReasoning(opt.id)}
              >{opt.label}</button>
            {/each}
            {#if reasoningPreset === "custom"}
              <span class="preset-custom">Custom</span>
            {/if}
          </div>
          {#each REASONING_OPTS as opt}
            {#if reasoningPreset === opt.id}
              <p class="preset-desc">{opt.desc}</p>
              <p class="preset-tech">{opt.tech}</p>
            {/if}
          {/each}

          <!-- ── Response Length ── -->
          <p class="section-label" style="margin-top: 22px;">Response Length</p>
          <p class="axis-question">How thorough vs. concise should responses be? <span class="axis-scope">Complex questions only.</span></p>
          <div class="preset-row">
            {#each LENGTH_OPTS as opt}
              <button
                class="preset-btn"
                class:preset-btn--active={lengthPreset === opt.id}
                onclick={() => setLength(opt.id)}
              >{opt.label}</button>
            {/each}
            {#if lengthPreset === "custom"}
              <span class="preset-custom">Custom</span>
            {/if}
          </div>
          {#each LENGTH_OPTS as opt}
            {#if lengthPreset === opt.id}
              <p class="preset-desc">{opt.desc}</p>
              <p class="preset-tech">{opt.tech}</p>
            {/if}
          {/each}

          <!-- ── Context Window (infrastructure setting, no preset) ── -->
          <p class="section-label" style="margin-top: 22px;">Context Window</p>
          <div class="param-card">
            <div class="param-row">
              <div class="param-top">
                <span class="param-name">Size</span>
                <input
                  class="param-input"
                  type="number"
                  bind:value={config.context_size}
                  oninput={markDirty}
                />
              </div>
              <p class="param-hint">How far back the model reads in a long conversation. Higher values improve coherence in long sessions at the cost of memory.</p>
            </div>
          </div>

        {:else if activeTab === "models"}
          <p class="loading-msg">Loading…</p>
        {/if}

        <!-- ──────────────── KNOWLEDGE ──────────────── -->
        {#if activeTab === "knowledge"}
          <p class="tab-intro">
            Knowledge bases are indexed locally. Sovereign searches them privately, without sending your queries anywhere.
          </p>

          {#if config}
            <p class="section-label">KnowledgeView</p>
            <p class="slot-desc" style="margin-bottom: 12px;">
              When on, Sovereign builds a compact map of what you return to, tensions that keep surfacing, and questions you haven't resolved — across your memories, conversations, and project notes. The model reads this map before answering. All enrichment stays on this machine. See <code>docs/knowledge-view.md</code> for the full picture.
            </p>

            <div class="param-card" style="margin-bottom: 24px;">
              <label class="toggle-row">
                <input
                  type="checkbox"
                  bind:checked={config.knowledge_view_enabled}
                  onchange={markDirty}
                />
                <span class="toggle-label">
                  <span class="toggle-title">Enable KnowledgeView</span>
                  <span class="toggle-sub">
                    Requires a desktop restart to take effect. When off, Sovereign starts every session from zero, as it did before this feature existed.
                  </span>
                </span>
              </label>
            </div>
          {/if}

          <!-- ── Storage budget ──────────────────────────────── -->
          <p class="section-label">Storage budget</p>
          <p class="slot-desc" style="margin-bottom: 12px;">
            How much disk Sovereign is allowed to use for installed corpora.
            The scheduler — both for your own installs and for shards peers
            ask this node to host — stops accepting new work once the budget
            is reached. Existing corpora stay put either way.
          </p>

          {#if storageBudget}
            <div class="param-card" style="margin-bottom: 16px;">
              <div class="storage-summary">
                <div class="storage-line">
                  {#if storageBudget.budget_bytes !== null}
                    <span class="storage-used">{fmtGib(storageBudget.used_bytes)}</span>
                    <span class="storage-of">of</span>
                    <span class="storage-budget">{fmtGib(storageBudget.budget_bytes)}</span>
                    <span class="storage-meta">
                      ({fmtGib(storageBudget.free_disk_bytes, 0)} free on disk)
                    </span>
                  {:else}
                    <span class="storage-used">{fmtGib(storageBudget.used_bytes)}</span>
                    <span class="storage-meta">
                      used · no budget — Sovereign uses whatever the disk has
                    </span>
                  {/if}
                </div>
                {#if storageBudget.budget_bytes !== null}
                  <div class="storage-bar" aria-hidden="true">
                    <div
                      class="storage-bar-fill"
                      class:storage-bar-fill--near={usageState === "near"}
                      class:storage-bar-fill--over={usageState === "over"}
                      style="width: {usagePercent.toFixed(1)}%"
                    ></div>
                  </div>
                  {#if usageState === "over"}
                    <p class="storage-near-msg">
                      Over budget. Sovereign won't accept new corpora or peer shards
                      until usage drops or you raise the budget.
                    </p>
                  {:else if usageState === "near"}
                    <p class="storage-near-msg">
                      Near the budget. New shards will be deferred soon.
                    </p>
                  {/if}
                {/if}
              </div>

              <div class="storage-controls">
                <label class="storage-input-row">
                  <span class="param-name">Budget (GiB)</span>
                  <input
                    class="param-input"
                    type="number"
                    min="1"
                    step="1"
                    placeholder={storageBudget.budget_bytes !== null
                      ? bytesToGib(storageBudget.budget_bytes).toFixed(0)
                      : "—"}
                    value={storageDraftGib ?? ""}
                    oninput={(e) => {
                      const v = (e.target as HTMLInputElement).value;
                      storageDraftGib = v === "" ? null : Number(v);
                    }}
                  />
                  <button
                    type="button"
                    class="slot-btn"
                    disabled={storageDraftGib === null}
                    onclick={applyDraftStorageBudget}
                  >Apply</button>
                </label>
                <div class="storage-actions">
                  <button
                    type="button"
                    class="slot-btn"
                    onclick={applyRecommendedStorageBudget}
                  >Use recommended ({fmtGib(storageBudget.recommended_bytes, 0)})</button>
                  {#if storageBudget.budget_bytes !== null}
                    <button
                      type="button"
                      class="slot-btn slot-btn--clear"
                      onclick={clearStorageBudget}
                    >Clear budget</button>
                  {/if}
                </div>
              </div>

              {#if storageStatusMessage}
                <p class="slot-desc" style="color: var(--color-error, #c44); margin: 8px 0 0;">
                  {storageStatusMessage}
                </p>
              {/if}
            </div>
          {:else}
            <p class="slot-desc" style="margin-bottom: 16px;">
              Loading storage budget…
            </p>
          {/if}

          <!-- ── Ingest pressure ─────────────────────────────── -->
          <p class="section-label">Ingest pressure</p>
          <p class="slot-desc" style="margin-bottom: 12px;">
            Long ingests (Wikipedia, Stack Exchange) can occupy the GPU for hours.
            Use these controls to share the machine — distinct from the foreground-yield
            window, which fully pauses ingest only while you're actively chatting.
          </p>

          <div class="param-card" style="margin-bottom: 16px;">
            <p class="slot-desc" style="margin: 0 0 8px 0; color: var(--text-base);">Throttle</p>
            <div class="preset-row" role="radiogroup" aria-label="Ingest throttle">
              {#each THROTTLE_PRESETS as preset (preset.value)}
                <button
                  type="button"
                  class="preset-btn"
                  class:preset-btn--active={throttlePreset === preset.value}
                  role="radio"
                  aria-checked={throttlePreset === preset.value}
                  onclick={() => applyThrottle(preset.value)}
                >
                  {preset.label}
                </button>
              {/each}
            </div>
            <p class="preset-desc" style="margin: 0;">
              {THROTTLE_PRESETS.find((p) => p.value === throttlePreset)?.desc
                ?? `Custom: ${(ingestThrottle * 100).toFixed(0)}% duty cycle.`}
            </p>
          </div>

          <div class="param-card" style="margin-bottom: 16px;">
            <label class="toggle-row">
              <input
                type="checkbox"
                checked={meshQuiesced}
                onchange={(e) => applyQuiesce((e.target as HTMLInputElement).checked)}
              />
              <span class="toggle-label">
                <span class="toggle-title">Stop participating in shared ingests</span>
                <span class="toggle-sub">
                  When on, this node won't pull work from peer coordinators or dispatch its own queue.
                  Local installs already in progress keep going — pause those individually below.
                  Re-enable to rejoin the mesh without restarting the daemon.
                </span>
              </span>
            </label>
          </div>

          {#if ingestStatusMessage}
            <p class="slot-desc" style="color: var(--color-error, #c44); margin-bottom: 12px;">
              {ingestStatusMessage}
            </p>
          {/if}

          <p class="section-label">Installed corpora</p>
          <KnowledgeStatus />
        {/if}

        <!-- ──────────────── ENRICHMENT (atlas) ──────────────── -->
        {#if activeTab === "enrichment"}
          <p class="tab-intro">
            Atlas enrichment produces a typed knowledge graph from a corpus —
            entities, events, states, relations, claims, questions,
            configurations. Run one article or book at a time; errors surface
            with remediation commands you can copy-paste.
          </p>
          <EnrichmentPanel />
        {/if}

        <!-- ──────────────── LOCAL KNOWLEDGE ──────────────── -->
        {#if activeTab === "local-knowledge"}
          <p class="tab-intro">
            Point Sovereign at a folder of documents or an Obsidian vault.
            Files stay on your computer. Nothing is uploaded.
          </p>
          <LocalKnowledgeSection {onOpenChatWithSeed} {onDropToChat} />
        {/if}

        <!-- ──────────────── MESH ──────────────── -->
        {#if activeTab === "mesh"}
          <p class="tab-intro">
            Pool compute and knowledge with people you trust. Everyone in a mesh can use each other's spare resources.
          </p>
          <MeshSettings />
        {/if}

        <!-- ──────────────── TOOLS ──────────────── -->
        {#if activeTab === "tools" && config}
          <p class="section-label">Web search</p>
          <p class="slot-desc" style="margin-bottom: 12px;">
            Used when Sovereign needs information beyond its local knowledge. Your queries go to the provider you choose — not to Sovereign's servers.
          </p>

          <div class="param-card" style="margin-bottom: 24px;">
            <div class="param-row">
              <div class="param-top">
                <span class="param-name">Provider</span>
                <select
                  class="param-select"
                  bind:value={config.search_backend.provider}
                  onchange={markDirty}
                >
                  <option value="duckduckgo">DuckDuckGo — free, no key needed</option>
                  <option value="brave">Brave Search</option>
                  <option value="tavily">Tavily</option>
                </select>
              </div>
            </div>
            {#if config.search_backend.provider !== "duckduckgo"}
              <div class="param-row">
                <div class="param-top">
                  <span class="param-name">API key</span>
                  <input
                    class="param-input"
                    type="password"
                    style="width: 160px;"
                    value={config.search_backend.api_key ?? ""}
                    oninput={(e) => {
                      config!.search_backend.api_key = (e.target as HTMLInputElement).value || null;
                      markDirty();
                    }}
                  />
                </div>
              </div>
            {/if}
          </div>

          <p class="section-label">Skills</p>
          <p class="slot-desc" style="margin-bottom: 12px;">
            Skills extend what Sovereign can do. Place skill folders in your skills directory to make them available here.
          </p>
          <SkillManager />

        {:else if activeTab === "tools"}
          <p class="loading-msg">Loading…</p>
        {/if}

        <!-- ──────────────── PATHS ──────────────── -->
        {#if activeTab === "paths" && config}
          <p class="tab-intro">
            These directories are created automatically on first run. Change them only if you want data stored somewhere specific.
          </p>

          <div class="param-card">
            <div class="param-row">
              <div class="param-top">
                <span class="param-name">Data directory</span>
              </div>
              <input
                class="path-input"
                type="text"
                bind:value={config.data_dir}
                oninput={markDirty}
                placeholder="Default: ~/.local/share/sovereign"
              />
            </div>

            <div class="param-row">
              <div class="param-top">
                <span class="param-name">Skills directory</span>
              </div>
              <input
                class="path-input"
                type="text"
                bind:value={config.skills_dir}
                oninput={markDirty}
                placeholder="Default: data_dir/skills"
              />
            </div>
          </div>

        {:else if activeTab === "paths"}
          <p class="loading-msg">Loading…</p>
        {/if}

        <!-- ──────────────── RECIPES ──────────────── -->
        {#if activeTab === "recipes"}
          <p class="tab-intro">
            Validate and test corpus recipe files before submitting them. Testing downloads a small sample and runs the full extraction pipeline locally.
          </p>
          <RecipeTestingPanel />
        {/if}

      </div><!-- /tab-body -->

      <!-- ── Save bar (only on tabs that need it) ── -->
      {#if needsSave}
        <div class="save-bar" class:save-bar--active={dirty}>
          <button class="save-btn" onclick={handleSave} disabled={saving || !dirty}>
            {saving ? "Saving…" : "Save & apply"}
          </button>
          {#if saveMessage}
            <span class="save-msg" class:save-msg--error={saveMessage.startsWith("Could")}>
              {saveMessage}
            </span>
          {:else if dirty}
            <span class="save-msg save-msg--pending">Unsaved changes</span>
          {/if}
        </div>
      {/if}

    </div><!-- /settings-content -->
  </div><!-- /settings-layout -->

</div>

<style>
  /* ── Shell ── */
  .settings-panel {
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-primary);
  }

  /* ── Header ── */
  .settings-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 16px 0 20px;
    height: 50px;
    flex-shrink: 0;
    border-bottom: 1px solid var(--border);
    background: var(--bg-secondary);
  }

  .settings-title {
    font-size: 0.88rem;
    font-weight: 600;
    color: var(--text-secondary);
    letter-spacing: 0.04em;
  }

  .close-btn {
    color: var(--text-muted);
    padding: 6px;
    border-radius: var(--radius);
    display: flex;
    align-items: center;
    justify-content: center;
    transition: color 0.15s, background 0.15s;
  }

  .close-btn:hover {
    color: var(--text-primary);
    background: var(--bg-surface);
  }

  /* ── Two-column layout ── */
  .settings-layout {
    flex: 1;
    display: flex;
    overflow: hidden;
  }

  /* ── Left nav ── */
  .settings-nav {
    width: 148px;
    flex-shrink: 0;
    border-right: 1px solid var(--border);
    background: var(--bg-secondary);
    display: flex;
    flex-direction: column;
    padding: 10px 8px;
    gap: 1px;
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px 10px;
    border-radius: var(--radius);
    border-left: 2px solid transparent;
    cursor: pointer;
    color: var(--text-muted);
    font-size: 0.8rem;
    font-weight: 500;
    letter-spacing: 0.02em;
    transition: color 0.15s, background 0.15s, border-color 0.15s;
    text-align: left;
    background: none;
    border-top: none;
    border-right: none;
    border-bottom: none;
  }

  .nav-item:hover {
    color: var(--text-secondary);
    background: var(--bg-surface);
  }

  .nav-item.active {
    color: var(--text-primary);
    background: var(--bg-elevated);
    border-left-color: var(--accent);
  }

  /* ── Content area ── */
  .settings-content {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .tab-body {
    flex: 1;
    overflow-y: auto;
    padding: 22px 22px 16px;
  }

  /* ── Shared typography ── */
  .tab-intro {
    font-size: 0.82rem;
    color: var(--text-muted);
    line-height: 1.55;
    margin-bottom: 18px;
  }

  .section-label {
    font-size: 0.67rem;
    font-weight: 700;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.1em;
    margin-bottom: 10px;
  }

  .loading-msg {
    color: var(--text-muted);
    font-size: 0.85rem;
    padding: 2rem 0;
    text-align: center;
  }

  /* ── Three-column slot grid ── */
  .model-slots-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
    margin-bottom: 14px;
  }

  .slot-card {
    background: var(--bg-secondary);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    padding: 14px 14px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    transition: border-color 0.2s, background 0.2s;
  }

  .slot-card--active {
    border-color: var(--accent);
    background: var(--bg-elevated);
  }

  .slot-card-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
  }

  .slot-card-title {
    font-size: 0.88rem;
    font-weight: 700;
    color: var(--text-primary);
    letter-spacing: -0.01em;
  }

  .slot-status-badge {
    font-size: 0.6rem;
    font-family: 'Syne Mono', monospace;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    border: 1px solid var(--border-mid);
    padding: 1px 6px;
    border-radius: 4px;
  }

  .slot-status-badge--opt {
    color: var(--sky);
    border-color: rgba(74, 186, 216, 0.25);
  }

  .slot-status-badge--req {
    color: var(--accent);
    border-color: rgba(201, 168, 76, 0.3);
  }

  .slot-card-desc {
    font-size: 0.73rem;
    color: var(--text-muted);
    line-height: 1.4;
    margin: 0;
  }

  .slot-current {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: auto;
    padding-top: 4px;
  }

  .slot-file {
    font-size: 0.7rem;
    font-family: 'Syne Mono', monospace;
    color: var(--success);
    word-break: break-all;
    line-height: 1.3;
  }

  .slot-empty {
    font-size: 0.73rem;
    color: var(--text-muted);
    font-style: italic;
  }

  .slot-btns {
    display: flex;
    gap: 6px;
  }

  .slot-btn {
    padding: 3px 10px;
    border-radius: var(--radius);
    font-size: 0.72rem;
    font-weight: 500;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    color: var(--text-secondary);
    transition: border-color 0.15s, color 0.15s, background 0.15s;
    cursor: pointer;
  }

  .slot-btn:hover {
    border-color: var(--accent);
    color: var(--text-primary);
  }

  .slot-btn--add {
    background: var(--accent-dim);
    border-color: rgba(201, 168, 76, 0.35);
    color: var(--accent-light);
  }

  .slot-btn--add:hover {
    background: rgba(201, 168, 76, 0.2);
    border-color: var(--accent);
  }

  .slot-btn--clear:hover {
    border-color: var(--error);
    color: var(--error);
  }

  /* ── Model picker row ── */
  .model-picker-row {
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    background: var(--bg-secondary);
    margin-bottom: 20px;
    overflow: hidden;
  }

  .picker-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 9px 14px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-surface);
  }

  .picker-label {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--text-secondary);
  }

  .picker-done {
    font-size: 0.72rem;
    color: var(--text-muted);
    padding: 3px 10px;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    background: none;
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
  }

  .picker-done:hover {
    color: var(--text-primary);
    border-color: var(--accent);
  }

  .picker-body {
    padding: 0 14px 14px;
  }

  /* ── Shared slot-desc (also used in Tools tab) ── */
  .slot-desc {
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.5;
    margin-bottom: 10px;
  }

  .inline-notice {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    margin-top: 10px;
    padding: 9px 12px;
    background: var(--accent-dim);
    border: 1px solid rgba(201, 168, 76, 0.25);
    border-radius: var(--radius);
    font-size: 0.76rem;
    color: var(--accent-light);
    line-height: 1.45;
  }

  /* Attach-mode heads-up above the model slots. Same visual weight
     as `.inline-notice` but distinct color so a user glancing at
     the Models tab sees "daemon managed externally" before the
     familiar embed-missing warning. */
  .attach-note {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin-bottom: 16px;
    padding: 10px 14px;
    background: rgba(121, 196, 120, 0.08);
    border: 1px solid rgba(121, 196, 120, 0.22);
    border-radius: var(--radius);
    font-size: 0.78rem;
    color: var(--text-secondary);
    line-height: 1.5;
  }
  .attach-note svg { flex-shrink: 0; margin-top: 2px; color: var(--growth); }
  .attach-note code {
    font-family: 'Syne Mono', monospace;
    background: var(--bg-surface);
    padding: 1px 6px;
    border-radius: 3px;
    font-size: 0.72rem;
  }

  .inline-notice svg {
    flex-shrink: 0;
    margin-top: 1px;
  }

  /* ── Param card ── */
  .param-card {
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    overflow: hidden;
  }

  .param-row {
    padding: 11px 14px;
    border-bottom: 1px solid var(--border);
  }

  .param-row:last-child {
    border-bottom: none;
  }

  .param-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 4px;
  }

  .param-name {
    font-size: 0.84rem;
    color: var(--text-secondary);
    font-weight: 500;
  }

  .param-input {
    width: 86px;
    padding: 4px 8px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    outline: none;
    text-align: right;
    font-size: 0.84rem;
    color: var(--text-primary);
    transition: border-color 0.15s;
    flex-shrink: 0;
    font-family: 'Syne Mono', monospace;
  }

  .param-input:focus {
    border-color: var(--accent);
  }

  .param-select {
    padding: 4px 8px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    outline: none;
    font-size: 0.82rem;
    color: var(--text-primary);
    cursor: pointer;
    appearance: none;
    flex-shrink: 0;
  }

  .param-select:focus {
    border-color: var(--accent);
  }

  .param-hint {
    font-size: 0.72rem;
    color: var(--text-muted);
    line-height: 1.45;
    margin: 0;
  }

  /* ── Path inputs ── */
  .path-input {
    width: 100%;
    padding: 7px 10px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    outline: none;
    font-size: 0.8rem;
    color: var(--text-primary);
    font-family: 'Syne Mono', ui-monospace, monospace;
    margin-top: 6px;
    transition: border-color 0.15s;
  }

  .path-input:focus {
    border-color: var(--accent);
  }

  .path-input::placeholder {
    color: var(--text-muted);
    opacity: 0.7;
  }

  /* ── Save bar ── */
  .save-bar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 22px;
    border-top: 1px solid var(--border);
    background: var(--bg-secondary);
    flex-shrink: 0;
  }

  .save-btn {
    padding: 8px 20px;
    background: var(--accent);
    color: var(--text-on-accent);
    border-radius: var(--radius);
    font-weight: 600;
    font-size: 0.82rem;
    letter-spacing: 0.02em;
    transition: background 0.2s, opacity 0.2s;
  }

  .save-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .save-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .save-msg {
    font-size: 0.78rem;
    color: var(--success);
    flex: 1;
  }

  .save-msg--error {
    color: var(--error);
  }

  .save-msg--pending {
    color: var(--text-muted);
    font-style: italic;
  }

  /* ── Semantic preset selectors ── */
  .axis-question {
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.45;
    margin-bottom: 10px;
  }

  .axis-scope {
    color: var(--lavender);
    font-style: italic;
    opacity: 0.85;
  }

  .preset-row {
    display: flex;
    flex-wrap: wrap;
    gap: 7px;
    margin-bottom: 10px;
    align-items: center;
  }

  .preset-btn {
    padding: 5px 14px;
    border-radius: 100px;
    font-size: 0.78rem;
    font-weight: 500;
    letter-spacing: 0.02em;
    border: 1px solid var(--border-mid);
    background: var(--bg-surface);
    color: var(--text-muted);
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s, color 0.15s;
  }

  .preset-btn:hover {
    border-color: var(--lavender);
    color: var(--lavender-light);
    background: var(--lavender-glow);
  }

  .preset-btn--active {
    background: var(--lavender-dim);
    border-color: var(--lavender);
    color: var(--lavender-light);
    font-weight: 600;
  }

  .preset-custom {
    font-size: 0.68rem;
    font-family: 'Syne Mono', monospace;
    color: var(--text-muted);
    letter-spacing: 0.06em;
    opacity: 0.7;
    padding: 3px 8px;
    border: 1px dashed var(--border-mid);
    border-radius: 100px;
  }

  .preset-desc {
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.45;
    margin-bottom: 3px;
  }

  .preset-tech {
    font-size: 0.67rem;
    font-family: 'Syne Mono', monospace;
    color: var(--text-muted);
    opacity: 0.55;
    letter-spacing: 0.04em;
    margin-bottom: 0;
  }

  /* KnowledgeView toggle row — lives inside a .param-card so its
     border + radius match adjacent settings controls. */
  .toggle-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 12px 14px;
    cursor: pointer;
  }

  .toggle-row input[type="checkbox"] {
    margin-top: 3px;
    flex-shrink: 0;
  }

  .toggle-label {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .toggle-title {
    font-size: 0.88rem;
    color: var(--text-primary);
    font-weight: 500;
  }

  .toggle-sub {
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.45;
  }

  /* Storage-budget summary — usage / budget bar plus controls. */
  .storage-summary {
    padding: 12px 14px;
    border-bottom: 1px solid var(--border);
  }
  .storage-line {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 6px;
    font-size: 0.85rem;
    color: var(--text-primary);
    margin-bottom: 8px;
  }
  .storage-used { font-weight: 600; }
  .storage-of { color: var(--text-muted); font-weight: 400; }
  .storage-budget { font-weight: 500; }
  .storage-meta {
    color: var(--text-muted);
    font-size: 0.75rem;
    margin-left: 4px;
  }
  .storage-bar {
    height: 6px;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: 3px;
    overflow: hidden;
  }
  .storage-bar-fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s ease, background 0.2s ease;
  }
  .storage-bar-fill--near {
    background: var(--accent-light, #d4a13a);
  }
  .storage-bar-fill--over {
    background: var(--error, #c44);
  }
  .storage-near-msg {
    margin: 6px 0 0;
    font-size: 0.74rem;
    color: var(--text-muted);
    line-height: 1.4;
  }
  .storage-controls {
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .storage-input-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .storage-input-row .param-name { flex: 1; }
  .storage-input-row .param-input { width: 100px; }
  .storage-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
</style>
