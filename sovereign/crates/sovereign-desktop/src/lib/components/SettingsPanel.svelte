<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import {
    detectBootstrap,
    detectHardware,
    getConfig,
    saveConfig,
    getSetupContextSize,
    setSetupContextSize,
    getIngestBudget,
    setIngestBudget,
    getMeshQuiesced,
    setMeshQuiesced,
    getStorageBudget,
    setStorageBudget,
    modelFileSize,
  } from "../api";
  import type { StorageBudgetState } from "../api";
  import {
    effectiveMemoryBytes,
    memorySourceLabel,
    peakMemoryBytes,
    budgetStateFor,
    fmtGiB,
    type SlotSizes,
  } from "./memoryBudget";
  import type {
    BootstrapSnapshot,
    DesktopConfig,
    HardwareInfo,
    SetupContextWindow,
  } from "../types";
  import MeshSettings from "./MeshSettings.svelte";
  import MeshAppsSection from "./MeshAppsSection.svelte";
  import SharingSection from "./SharingSection.svelte";
  import ModelSelector from "../setup/ModelSelector.svelte";
  import UpdatesSection from "./UpdatesSection.svelte";
  import SetupReportCard from "./SetupReportCard.svelte";
  import CrashRecordsPanel from "./CrashRecordsPanel.svelte";

  interface Props {
    onClose: () => void;
  }

  let { onClose }: Props = $props();

  type Tab =
    | "models"
    | "mesh"
    | "sharing"
    | "tools"
    | "paths"
    | "mobile"
    | "diagnostics"
    | "about";
  let activeTab: Tab = $state("models");

  let config: DesktopConfig | null = $state(null);
  let saving = $state(false);
  let saveMessage = $state("");
  let dirty = $state(false);
  let bootstrap = $state<BootstrapSnapshot | null>(null);

  // ── Mobile access (opt-in phone host) ─────────────────────────
  // Toggle serves the svrnmesh mobile app from this machine. The host
  // (`sovereign-server`) delegates all inference to the running daemon, so it
  // loads no second copy of the models. Pairing card = address + tenant +
  // token, plus the no-VPN iroh pairing code once the server reports one.
  let mobilePairing = $state<{
    address: string;
    tenant: string;
    token: string;
    iroh_dial: string | null;
  } | null>(null);
  let mobileError = $state("");
  let mobilePollTimer: ReturnType<typeof setTimeout> | null = null;
  // QR of the pairing deep link: `sovereign://pair#<base64url(JSON)>`.
  // The phone's camera shows "Open in svrnmesh" and the app fills
  // every pairing field. Deliberately NOT raw JSON — iOS Camera spots
  // the https relay URL inside and offers Safari instead, losing the
  // payload (observed 2026-06-10). The Copy button carries the plain
  // JSON for the shared-clipboard path; the app accepts both forms.
  let mobileQrUrl = $state<string | null>(null);
  let mobileCopied = $state(false);

  const pairingPayload = (p: NonNullable<typeof mobilePairing>) =>
    JSON.stringify({
      address: p.iroh_dial ?? p.address,
      tenant: p.tenant,
      token: p.token,
    });

  const pairingLink = (p: NonNullable<typeof mobilePairing>) =>
    "sovereign://pair#" +
    btoa(pairingPayload(p)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

  $effect(() => {
    const p = mobilePairing;
    if (p?.iroh_dial) {
      void import("qrcode").then((QRCode) =>
        QRCode.toDataURL(pairingLink(p), { width: 220, margin: 1 }).then(
          (url: string) => (mobileQrUrl = url),
        ),
      );
    } else {
      mobileQrUrl = null;
    }
  });

  async function copyPairing() {
    if (!mobilePairing) return;
    await navigator.clipboard.writeText(pairingPayload(mobilePairing));
    mobileCopied = true;
    setTimeout(() => (mobileCopied = false), 1500);
  }

  async function loadMobilePairing() {
    try {
      mobilePairing = await invoke("get_mobile_pairing");
      // The iroh pairing code is runtime state (the server's endpoint
      // identity + the relay it settled on) — null while the supervised
      // server is still starting or hasn't reached a relay. Re-poll
      // gently until it lands; the server's cold start can take ~20s.
      if (mobilePairing && !mobilePairing.iroh_dial) {
        if (mobilePollTimer) clearTimeout(mobilePollTimer);
        mobilePollTimer = setTimeout(() => {
          if (activeTab === "mobile" && config?.mobile_access_enabled) {
            void loadMobilePairing();
          }
        }, 3000);
      }
    } catch (e) {
      mobileError = String(e);
    }
  }

  async function onMobileToggle() {
    if (!config) return;
    mobileError = "";
    const enabled = config.mobile_access_enabled;
    try {
      await invoke("set_mobile_access", { enabled });
      markDirty("mobile_access");
      await handleSave(); // persist immediately, like the Recipe Author toggle
      if (enabled) await loadMobilePairing();
      else mobilePairing = null;
    } catch (e) {
      mobileError = String(e);
      config.mobile_access_enabled = !enabled; // revert on failure
    }
  }

  // Load the pairing card when the Mobile tab opens with access already
  // on — and RE-load when a cached fetch is missing the no-VPN code
  // (the supervised server reports it only once it has reached its
  // relay; leaving the tab kills the poll chain, so re-entry must
  // restart it rather than trust the stale null).
  $effect(() => {
    if (
      activeTab === "mobile" &&
      config?.mobile_access_enabled &&
      (!mobilePairing || !mobilePairing.iroh_dial)
    ) {
      void loadMobilePairing();
    }
  });

  // ── Canonical chat-slot context window ───────────────────────
  // Sourced from `~/.svrnmesh/config.toml` via the
  // `get_setup_context_size` Tauri command. Edits route through
  // `set_setup_context_size`, which writes the daemon config and
  // tears down + rebuilds the desktop-embedded inference Arc.
  // `null` until first load completes; `setupCtxBusy` covers the
  // 15-30s reload window so the UI can disable the Save button and
  // show progress.
  let setupCtx = $state<SetupContextWindow | null>(null);
  let setupCtxDraft = $state<number | null>(null);
  let setupCtxBusy = $state(false);
  let setupCtxMessage = $state("");
  let attachedToDaemon = $derived(bootstrap?.daemon_running === true);

  // ── Ingest pressure controls ──────────────────────────────────
  let ingestThrottle = $state<number>(1.0);
  let meshQuiesced = $state<boolean>(false);
  let ingestStatusMessage = $state<string>("");

  // ── Storage budget ────────────────────────────────────────────
  let storageBudget = $state<StorageBudgetState | null>(null);
  let storageDraftGib = $state<number | null>(null);
  let storageStatusMessage = $state<string>("");
  const BYTES_PER_GIB = 1_073_741_824;
  function bytesToGib(b: number): number { return b / BYTES_PER_GIB; }
  function gibToBytes(g: number): number { return Math.round(g * BYTES_PER_GIB); }
  function fmtGib(b: number, digits = 1): string {
    return `${bytesToGib(b).toFixed(digits)} GiB`;
  }
  let usagePercent = $derived.by(() => {
    if (!storageBudget?.budget_bytes) return 0;
    return Math.min(100, (storageBudget.used_bytes / storageBudget.budget_bytes) * 100);
  });
  let usageState = $derived.by(() => {
    if (usagePercent >= 100) return "over";
    if (usagePercent >= 95) return "near";
    return "ok";
  });

  // ── Search + read/edit mode state ─────────────────────────────
  let searchQuery = $state('');
  let editingCreativity = $state(false);
  let editingContextWindow = $state(false);
  let editingResponseLength = $state(false);
  let editingPersona = $state(false);
  let editingStorageBudget = $state(false);
  let editingPaths = $state(false);
  let editingIdleSecs = $state(false);
  let showAdvanced = $state(false);

  const EMBED_FAMILY_OPTS = [
    { value: "Unknown",        label: "Auto (mean pooling)",   desc: "Default — works for mxbai, BGE, and most open-weights embedders." },
    { value: "Qwen3Embedding", label: "Qwen3 Embedding",       desc: "Required for qwen3-embedding-* GGUFs. Last-token pooling + instruction prefix." },
  ] as const;
  const CODE_FAMILY_OPTS = [
    { value: "Unknown", label: "Auto",          desc: "Default — safe fallback for BYOM coders." },
    { value: "Qwen35",  label: "Qwen 3.5 / Coder", desc: "For Qwen-Coder / Qwopus lineage. Sets matching chat template + sampling." },
    { value: "Llama3",  label: "Llama 3",       desc: "DeepSeek-Coder-V2 and Llama-3-derived coders." },
  ] as const;

  const TOOL_OPTS = [
    { id: "shell",            label: "Shell",            desc: "Run shell commands on this machine. You approve each one before it runs." },
    { id: "search",           label: "Web search",       desc: "Direct queries to whichever search provider you chose under Web Search." },
    { id: "web_fetch",        label: "Web fetch",        desc: "Read a specific URL the model wants to cite." },
    { id: "document",         label: "Document",         desc: "Read attached files and ingested documents passage by passage." },
    { id: "knowledge_lookup", label: "Knowledge lookup", desc: "One deliberate sweep across your library, memory, and notes — plus web when Auto-escalate is on. Separate from the routine library search the assistant already does on every turn." },
  ] as const;
  function toggleTool(id: string) {
    if (!config) return;
    const has = config.enabled_tools.includes(id);
    config.enabled_tools = has
      ? config.enabled_tools.filter((t) => t !== id)
      : [...config.enabled_tools, id];
    markDirty(`tool-${id}`);
  }

  // ── Provenance tracking (session-level) ───────────────────────
  // A full implementation would persist this alongside config so
  // "Changed Mar 14" survives restarts. For now: "Default" until
  // changed in this session, then "Changed · [date]". This is honest
  // — it doesn't claim a date it doesn't know.
  let provenanceChanges = $state<Record<string, Date>>({});
  function provenance(key: string): string {
    const d = provenanceChanges[key];
    if (!d) return 'Default';
    return `Changed · ${d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })}`;
  }

  // ── Memory budgeting for the Models tab ───────────────────────
  // The pure math (effective device memory, peak resident bytes, the
  // budget thresholds, GiB formatting) lives in `./memoryBudget`
  // (unit-tested — picking a large model in every slot is a footgun, so
  // the meter is worth pinning). These reactive `$derived`s feed it the
  // live slot sizes + detected hardware.
  let hardware = $state<HardwareInfo | null>(null);
  let slotSizes = $state<SlotSizes>({
    fast: null,
    primary: null,
    embed: null,
    code: null,
  });

  let peakBytes = $derived(peakMemoryBytes(slotSizes));
  let effectiveBytes = $derived(hardware ? effectiveMemoryBytes(hardware) : 0);
  let budgetRatio = $derived(effectiveBytes > 0 ? peakBytes / effectiveBytes : 0);
  let budgetState = $derived(budgetStateFor(budgetRatio));

  async function refreshSlotSizes(cfg: DesktopConfig | null) {
    if (!cfg) return;
    const [fast, primary, embed, code] = await Promise.all([
      modelFileSize(cfg.model_path),
      modelFileSize(cfg.primary_model_path),
      modelFileSize(cfg.embed_model_path),
      modelFileSize(cfg.code_model_path),
    ]);
    slotSizes = { fast, primary, embed, code };
  }

  // Re-measure whenever any slot path changes. $effect runs after
  // the model-path mutation so the new size is fetched before the
  // user has a chance to read the budget meter.
  $effect(() => {
    if (!config) return;
    // Touch every path so Svelte tracks them as dependencies.
    void config.model_path;
    void config.primary_model_path;
    void config.embed_model_path;
    void config.code_model_path;
    refreshSlotSizes(config);
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
      hardware = await detectHardware();
    } catch (e) {
      console.warn("Hardware detection failed; budget meter will hide:", e);
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
    try {
      setupCtx = await getSetupContextSize();
      setupCtxDraft = setupCtx.configured;
    } catch (e) {
      console.warn("Failed to load context-window snapshot:", e);
    }
  });

  async function applyContextWindow() {
    if (!setupCtx || setupCtxDraft == null) return;
    if (setupCtxDraft === setupCtx.configured) {
      editingContextWindow = false;
      return;
    }
    if (!Number.isFinite(setupCtxDraft) || setupCtxDraft < 512) {
      setupCtxMessage = "Context window must be at least 512 tokens.";
      return;
    }
    setupCtxMessage = "";
    setupCtxBusy = true;
    try {
      await setSetupContextSize(setupCtxDraft);
      // Re-read after the rebuild settles so all three values reflect
      // the new state (effective + n_ctx_train read from the freshly-
      // loaded inference provider).
      setupCtx = await getSetupContextSize();
      setupCtxDraft = setupCtx.configured;
      editingContextWindow = false;
    } catch (e) {
      setupCtxMessage = `Failed to apply: ${e}`;
    } finally {
      setupCtxBusy = false;
    }
  }

  async function applyStorageBudget(bytes: number | null) {
    storageStatusMessage = "";
    try {
      storageBudget = await setStorageBudget(bytes);
      storageDraftGib = null;
      editingStorageBudget = false;
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

  const THROTTLE_PRESETS: Array<{ value: number; label: string; desc: string }> = [
    { value: 1.00, label: "Off",      desc: "Full speed. The default — ingest takes every cycle it can." },
    { value: 0.75, label: "Light",    desc: "75% duty cycle. Barely noticeable; leaves a little headroom for other work." },
    { value: 0.50, label: "Balanced", desc: "50% duty cycle. Ingest takes about twice as long; the machine stays usable." },
    { value: 0.25, label: "Quiet",    desc: "25% duty cycle. Ingest hums along in the background while you do other things." },
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

  function markDirty(key?: string) {
    dirty = true;
    saveMessage = "";
    if (key) provenanceChanges[key] = new Date();
  }

  async function handleSave() {
    if (!config || saving) return;
    saving = true;
    saveMessage = "";
    try {
      await saveConfig(config);
      saveMessage = "Saved.";
      dirty = false;
    } catch (e) {
      saveMessage = `Could not save: ${e}`;
    }
    saving = false;
  }

  let needsSave = $derived(
    activeTab === "models"
      || activeTab === "paths"
      || activeTab === "tools",
  );

  // ── Semantic preset detection ──────────────────────────────────
  // Creativity (temperature + top_k) wires through every outer-work
  // synthesis path. Response length (max_tokens) wires through the
  // KnowledgeQuery / DeepQuery / SimpleQuery paths (the big ones the
  // cutoff chip surfaces). think_budget still stays handler-tuned —
  // it interacts with the Speed selector in non-obvious ways.
  type CreativityPreset  = "precise" | "balanced" | "exploratory" | "custom";

  let creativityPreset = $derived.by((): CreativityPreset => {
    if (!config) return "balanced";
    const { temperature: t, top_k: k } = config;
    if (t === 0.3 && k === 10)  return "precise";
    if (t === 0.6 && k === 20)  return "balanced";
    if (t === 1.0 && k === 40)  return "exploratory";
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
    markDirty('creativity');
  }

  const CREATIVITY_OPTS = [
    { id: "precise"     as const, label: "Precise",     desc: "Steady and repeatable. Best for facts, code, structured output.",      tech: "temp 0.3 · top_k 10" },
    { id: "balanced"    as const, label: "Balanced",    desc: "Natural-sounding answers without surprises. The default for everyday work.", tech: "temp 0.6 · top_k 20" },
    { id: "exploratory" as const, label: "Exploratory", desc: "Surprising phrasings and angles. Higher risk of stating something wrong with confidence.", tech: "temp 1.0 · top_k 40" },
  ];

  let creativityLabel = $derived(CREATIVITY_OPTS.find(o => o.id === creativityPreset)?.label ?? 'Custom');
  let creativityTech  = $derived.by((): string => {
    const found = CREATIVITY_OPTS.find(o => o.id === creativityPreset);
    if (found) return found.tech;
    if (!config) return '';
    return `temp ${config.temperature} · top_k ${config.top_k}`;
  });

  let activeSlot: "fast" | "reasoning" | "embed" | "code" | null = $state(null);
  function modelFileName(path: string): string {
    return path.split(/[\\/]/).pop() ?? path;
  }
  let slotSelectedPath = $derived.by((): string => {
    if (!config || !activeSlot) return "";
    if (activeSlot === "fast")      return config.model_path ?? "";
    if (activeSlot === "reasoning") return config.primary_model_path ?? "";
    if (activeSlot === "code")      return config.code_model_path ?? "";
    return config.embed_model_path ?? "";
  });
  function handleSlotSelect(path: string) {
    if (!config || !activeSlot) return;
    if (activeSlot === "fast")           config.model_path = path;
    else if (activeSlot === "reasoning") config.primary_model_path = path || null;
    else if (activeSlot === "code")      config.code_model_path = path || null;
    else                                 config.embed_model_path = path || null;
    markDirty(`model-${activeSlot}`);
  }

  // Two clusters (D7): plain configuration plumbing, and an operator
  // cluster (mesh / sharing / mobile) for running this node on a mesh.
  // Order here = visual order in the TOC.
  const ALL_TABS: { id: Tab; label: string; keywords: string[]; group: "general" | "operator" }[] = [
    { id: "models",          label: "Models",          group: "general",  keywords: ["model", "creativity", "reasoning", "length", "context", "temperature", "token", "gguf"] },
    { id: "tools",           label: "Web Search",       group: "general",  keywords: ["tool", "search", "web", "duck", "brave", "tavily"] },
    { id: "paths",           label: "Paths",            group: "general",  keywords: ["path", "directory", "folder", "data dir", "skills dir"] },
    { id: "about",           label: "About",            group: "general",  keywords: ["about", "version", "update", "updates", "upgrade", "check", "release"] },
    { id: "mesh",            label: "Mesh",             group: "operator", keywords: ["mesh", "peer", "network", "share", "node", "collaborative"] },
    { id: "sharing",         label: "Activity & Sharing", group: "operator", keywords: ["activity", "usage", "tokens", "chunks", "embeddings", "queries", "ingest", "share", "ceiling", "pause", "contribution", "peer", "gpu", "mesh", "yield", "throttle", "reins"] },
    { id: "mobile",          label: "Mobile access",    group: "operator", keywords: ["mobile", "phone", "ios", "android", "app", "pair", "pairing", "tailnet", "tailscale", "token", "host"] },
    { id: "diagnostics",     label: "Diagnostics",      group: "operator", keywords: ["diagnostics", "crash", "panic", "error", "bug", "report", "log", "backtrace", "signal", "sigsegv", "incident"] },
  ];

  const TAB_GROUPS: { id: "general" | "operator"; label: string }[] = [
    { id: "general",  label: "General" },
    { id: "operator", label: "Operator" },
  ];

  let visibleTabs = $derived.by(() => {
    if (!searchQuery.trim()) return ALL_TABS;
    const q = searchQuery.toLowerCase();
    return ALL_TABS.filter(t =>
      t.label.toLowerCase().includes(q) ||
      t.keywords.some(k => k.includes(q))
    );
  });

  // Group the (filtered) tabs for the TOC. A group whose tabs are all
  // filtered out by search is dropped so its header never orphans.
  let visibleGroups = $derived(
    TAB_GROUPS
      .map((g) => ({ ...g, tabs: visibleTabs.filter((t) => t.group === g.id) }))
      .filter((g) => g.tabs.length > 0),
  );

  $effect(() => {
    if (visibleTabs.length === 1) {
      activeTab = visibleTabs[0].id;
    }
  });

  function toggleSlot(slot: "fast" | "reasoning" | "embed" | "code") {
    activeSlot = activeSlot === slot ? null : slot;
  }
</script>

<div class="cfg">

  <!-- ── Header ────────────────────────────────────────────────── -->
  <header class="cfg-head">
    <span class="cfg-wordmark">Configuration</span>
    <div class="cfg-search-wrap">
      <svg class="cfg-search-icon" width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <circle cx="5" cy="5" r="3.5" stroke="currentColor" stroke-width="1.2"/>
        <path d="M7.5 7.5L10 10" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
      </svg>
      <input
        class="cfg-search"
        type="search"
        bind:value={searchQuery}
        placeholder="Find a setting…"
        aria-label="Search settings"
      />
    </div>
    <button class="cfg-close" onclick={onClose} aria-label="Close configuration">
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <path d="M1 1l10 10M11 1L1 11" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
      </svg>
    </button>
  </header>

  <div class="cfg-body">

    <!-- ── Table of contents ──────────────────────────────────── -->
    <nav class="cfg-toc" aria-label="Configuration sections">
      {#if visibleTabs.length === 0}
        <p class="toc-empty">No matches</p>
      {:else}
        {#each visibleGroups as group}
          <div class="toc-group-header">{group.label}</div>
          {#each group.tabs as tab}
            <button
              class="toc-item"
              class:toc-item--active={activeTab === tab.id}
              onclick={() => { activeTab = tab.id; saveMessage = ""; }}
              aria-current={activeTab === tab.id ? "page" : undefined}
            >
              {tab.label}
            </button>
          {/each}
        {/each}
      {/if}

      {#if dirty && needsSave}
        <div class="toc-pending" aria-live="polite">
          <span class="toc-pending-dot"></span>
          Unsaved
        </div>
      {/if}
    </nav>

    <!-- ── Document ───────────────────────────────────────────── -->
    <div class="cfg-doc" role="main">

      <!-- ──────────── MODELS ──────────── -->
      {#if activeTab === "models" && config}

        <section class="doc-section">
          <span class="section-eyebrow">roles &middot; slots &middot; behaviour</span>
          <h2 class="doc-h2">Models</h2>
          <p class="doc-intro">Four jobs, up to four models. Pick a file for each — svrnmesh loads them only when something needs them.</p>

          {#if attachedToDaemon}
            <div class="doc-note">
              A daemon is already running outside this app. Swapping a model file takes effect immediately; changes to port or data directory need a restart — run <code>svrn daemon restart</code> in a terminal.
            </div>
          {/if}

          <!-- ── Memory budget meter ──────────────────────────────
               Sums the always-loaded slots plus the larger of the two
               lazy slots, compares against this device's effective
               memory, and warns before the user saves a combination
               that would crash on load. Hidden if hardware detection
               fails (we'd be guessing). -->
          {#if hardware}
            <div class="budget-meter budget-meter--{budgetState}" role="status">
              <div class="budget-meter-head">
                <div class="budget-meter-text">
                  <span class="budget-meter-label">Peak memory</span>
                  <span class="budget-meter-figure">
                    <strong>{fmtGiB(peakBytes)}</strong>
                    <span class="budget-meter-of">of {fmtGiB(effectiveBytes)} {memorySourceLabel(hardware)}</span>
                  </span>
                </div>
                <span class="budget-meter-pct">{Math.round(budgetRatio * 100)}%</span>
              </div>
              <div class="budget-bar-track" aria-hidden="true">
                <div class="budget-bar-fill" style="width: {Math.min(budgetRatio, 1) * 100}%"></div>
                {#if budgetRatio > 1}
                  <div class="budget-bar-over" style="width: {Math.min((budgetRatio - 1), 0.5) * 200}%"></div>
                {/if}
              </div>
              {#if budgetState === "crit"}
                <p class="budget-meter-msg">
                  This combination is likely to crash on load. The Main responder and Code specialist share a slot — drop one, choose a smaller quant, or pick a lighter Quick responder.
                </p>
              {:else if budgetState === "warn"}
                <p class="budget-meter-msg">
                  Close to the ceiling. Background tasks and the OS need headroom too — consider a smaller model in one slot.
                </p>
              {:else}
                <p class="budget-meter-msg budget-meter-msg--ok">
                  Fits comfortably. Quick responder and embedder stay resident; Main and Code load on demand and share one slot.
                </p>
              {/if}
            </div>
          {/if}

          <!-- Model slots as a readable list -->
          <div class="slot-list">

            <!-- Quick responder -->
            <div class="slot-item" class:slot-item--open={activeSlot === "fast"} data-role="fast">
              <button class="slot-item-row" onclick={() => toggleSlot("fast")} aria-expanded={activeSlot === "fast"}>
                <span class="slot-item-role">
                  Quick responder
                  <span class="slot-item-sub">Short turns, instant replies</span>
                </span>
                <span class="slot-item-file">
                  {#if config.model_path}
                    {modelFileName(config.model_path)}
                    {#if slotSizes.fast !== null}<span class="slot-item-size">{fmtGiB(slotSizes.fast)}</span>{/if}
                  {:else}
                    <span class="slot-item-unset">not set</span>
                  {/if}
                </span>
                <span class="slot-item-meta">Always on</span>
                <span class="slot-item-chevron" aria-hidden="true">{activeSlot === "fast" ? '↑' : '↓'}</span>
              </button>
              {#if activeSlot === "fast"}
                <div class="slot-item-body">
                  <p class="slot-item-desc">Handles the short turns — quick replies, drafts, follow-ups. Stays loaded so there's no wait when you hit send.</p>
                  <div class="slot-item-controls">
                    <ModelSelector selectedPath={slotSelectedPath} onSelect={handleSlotSelect} showRawInput={true} embedMode={false} allowManage={true} />
                    {#if config.model_path}
                      <button class="act-btn act-btn--ghost act-btn--danger" onclick={() => { config!.model_path = ""; markDirty('model-fast'); activeSlot = null; }}>
                        Clear
                      </button>
                    {/if}
                  </div>
                </div>
              {/if}
            </div>

            <!-- Main responder -->
            <div class="slot-item" class:slot-item--open={activeSlot === "reasoning"} data-role="primary">
              <button class="slot-item-row" onclick={() => toggleSlot("reasoning")} aria-expanded={activeSlot === "reasoning"}>
                <span class="slot-item-role">
                  Main responder
                  <span class="slot-item-sub">Research, long writing, deep analysis</span>
                </span>
                <span class="slot-item-file">
                  {#if config.primary_model_path}
                    {modelFileName(config.primary_model_path)}
                    {#if slotSizes.primary !== null}<span class="slot-item-size">{fmtGiB(slotSizes.primary)}</span>{/if}
                  {:else}
                    <span class="slot-item-unset">not set</span>
                  {/if}
                </span>
                <span class="slot-item-meta">On demand</span>
                <span class="slot-item-chevron" aria-hidden="true">{activeSlot === "reasoning" ? '↑' : '↓'}</span>
              </button>
              {#if activeSlot === "reasoning"}
                <div class="slot-item-body">
                  <p class="slot-item-desc">Your heaviest model. Comes out for research, long writing, and careful analysis, then steps back five minutes after the last question to free memory.</p>
                  <div class="slot-item-controls">
                    <ModelSelector selectedPath={slotSelectedPath} onSelect={handleSlotSelect} showRawInput={true} embedMode={false} allowManage={true} />
                    {#if config.primary_model_path}
                      <button class="act-btn act-btn--ghost act-btn--danger" onclick={() => { config!.primary_model_path = null; markDirty('model-reasoning'); activeSlot = null; }}>
                        Clear
                      </button>
                    {/if}
                  </div>
                </div>
              {/if}
            </div>

            <!-- Knowledge embedder -->
            <div class="slot-item" class:slot-item--open={activeSlot === "embed"} data-role="embed">
              <button class="slot-item-row" onclick={() => toggleSlot("embed")} aria-expanded={activeSlot === "embed"}>
                <span class="slot-item-role">
                  Knowledge embedder
                  <span class="slot-item-sub">Makes your library searchable</span>
                </span>
                <span class="slot-item-file">
                  {#if config.embed_model_path}
                    {modelFileName(config.embed_model_path)}
                    {#if slotSizes.embed !== null}<span class="slot-item-size">{fmtGiB(slotSizes.embed)}</span>{/if}
                  {:else}
                    <span class="slot-item-unset slot-item-unset--warn">not set — library unsearchable</span>
                  {/if}
                </span>
                <span class="slot-item-meta">For your library</span>
                <span class="slot-item-chevron" aria-hidden="true">{activeSlot === "embed" ? '↑' : '↓'}</span>
              </button>
              {#if activeSlot === "embed"}
                <div class="slot-item-body">
                  <p class="slot-item-desc">Indexes every document you add so the assistant can find passages by meaning, not just keywords. Runs in the background while ingest is going.</p>
                  <div class="slot-item-controls">
                    <ModelSelector selectedPath={slotSelectedPath} onSelect={handleSlotSelect} showRawInput={true} embedMode={true} allowManage={true} />
                    {#if config.embed_model_path}
                      <button class="act-btn act-btn--ghost act-btn--danger" onclick={() => { config!.embed_model_path = null; markDirty('model-embed'); activeSlot = null; }}>
                        Clear
                      </button>
                    {/if}
                  </div>
                  <div class="inline-field" style="margin-top: 12px;">
                    <span class="inline-field-label">Family</span>
                    <select
                      class="cfg-select"
                      bind:value={config.embed_family}
                      onchange={() => markDirty('embed-family')}
                      aria-label="Embedding model family"
                    >
                      {#each EMBED_FAMILY_OPTS as opt}
                        <option value={opt.value}>{opt.label}</option>
                      {/each}
                    </select>
                  </div>
                  <p class="preset-desc" style="margin-top: 6px;">
                    {EMBED_FAMILY_OPTS.find(o => o.value === config!.embed_family)?.desc ?? ''}
                  </p>
                  <p class="cfg-caution" style="margin-top: 4px;">
                    Picking the wrong family produces unsearchable indexes — match it to the embedder you chose above.
                  </p>
                </div>
              {/if}
            </div>

            <!-- Code specialist -->
            <div class="slot-item" class:slot-item--open={activeSlot === "code"} data-role="code">
              <button class="slot-item-row" onclick={() => toggleSlot("code")} aria-expanded={activeSlot === "code"}>
                <span class="slot-item-role">
                  Code specialist
                  <span class="slot-item-sub">Optional — routes programming turns</span>
                </span>
                <span class="slot-item-file">
                  {#if config.code_model_path}
                    {modelFileName(config.code_model_path)}
                    {#if slotSizes.code !== null}<span class="slot-item-size">{fmtGiB(slotSizes.code)}</span>{/if}
                  {:else}
                    <span class="slot-item-unset">not set</span>
                  {/if}
                </span>
                <span class="slot-item-meta">Optional</span>
                <span class="slot-item-chevron" aria-hidden="true">{activeSlot === "code" ? '↑' : '↓'}</span>
              </button>
              {#if activeSlot === "code"}
                <div class="slot-item-body">
                  <p class="slot-item-desc">A second model trained on code (Qwen-Coder, DeepSeek-Coder, etc.). When set, programming questions go here instead of the Main responder. The two share a memory slot — whichever you need loads on demand.</p>
                  <div class="slot-item-controls">
                    <ModelSelector selectedPath={slotSelectedPath} onSelect={handleSlotSelect} showRawInput={true} embedMode={false} allowManage={true} />
                    {#if config.code_model_path}
                      <button class="act-btn act-btn--ghost act-btn--danger" onclick={() => { config!.code_model_path = null; markDirty('model-code'); activeSlot = null; }}>
                        Clear
                      </button>
                    {/if}
                  </div>
                  <div class="inline-field" style="margin-top: 12px;">
                    <span class="inline-field-label">Family</span>
                    <select
                      class="cfg-select"
                      bind:value={config.code_family}
                      onchange={() => markDirty('code-family')}
                      aria-label="Code model family"
                    >
                      {#each CODE_FAMILY_OPTS as opt}
                        <option value={opt.value}>{opt.label}</option>
                      {/each}
                    </select>
                  </div>
                  <p class="preset-desc" style="margin-top: 6px;">
                    {CODE_FAMILY_OPTS.find(o => o.value === config!.code_family)?.desc ?? ''}
                  </p>
                </div>
              {/if}
            </div>

          </div><!-- /slot-list -->

          <div class="doc-divider"></div>
          <h3 class="doc-h3">Output style</h3>
          <p class="doc-intro">Shapes how the assistant writes its final answers. Everything happening behind the scenes — planning, retrieval, formatting — is already tuned for you.</p>

          <!-- Creativity -->
          <div class="cfg-entry" class:cfg-entry--open={editingCreativity}>
            <button class="cfg-entry-display" onclick={() => editingCreativity = !editingCreativity} aria-expanded={editingCreativity}>
              <span class="cfg-entry-name">Creativity</span>
              <span class="cfg-entry-current">
                <span class="cfg-entry-val">{creativityLabel}</span>
                <span class="cfg-entry-tech">{creativityTech}</span>
              </span>
              <span class="cfg-entry-prov">{provenance('creativity')}</span>
            </button>
            {#if editingCreativity}
              <div class="cfg-entry-edit">
                <p class="cfg-entry-question">How predictable should answers feel?</p>
                <div class="preset-row" role="radiogroup" aria-label="Creativity preset">
                  {#each CREATIVITY_OPTS as opt}
                    <button
                      class="preset-btn"
                      class:preset-btn--active={creativityPreset === opt.id}
                      role="radio"
                      aria-checked={creativityPreset === opt.id}
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
                  <p class="cfg-caution">Lively, but more willing to state wrong things with confidence. Skip this when accuracy matters.</p>
                {/if}
                <button class="edit-done" onclick={() => editingCreativity = false}>Done</button>
              </div>
            {/if}
          </div>

          <!-- Context window — sourced from ~/.svrnmesh/config.toml
               via the dedicated Tauri commands. Three values shown:
                 • configured (editable, daemon-canonical)
                 • effective (post-llama-pad, read-only)
                 • n_ctx_train (gguf ceiling, read-only)
               Edits route through `set_setup_context_size`, which
               tears down + rebuilds the inference Arc — UI shows a
               busy state during the 15-30s reload. -->
          <div class="cfg-entry" class:cfg-entry--open={editingContextWindow}>
            <button class="cfg-entry-display" onclick={() => editingContextWindow = !editingContextWindow} aria-expanded={editingContextWindow}>
              <span class="cfg-entry-name">Context window</span>
              <span class="cfg-entry-current">
                <span class="cfg-entry-val">{setupCtx?.configured?.toLocaleString() ?? '—'}</span>
                <span class="cfg-entry-tech">tokens</span>
              </span>
              <span class="cfg-entry-prov">
                {#if setupCtx?.n_ctx_train}
                  up to {setupCtx.n_ctx_train.toLocaleString()} in this model
                {:else}
                  ~/.svrnmesh/config.toml
                {/if}
              </span>
            </button>
            {#if editingContextWindow}
              <div class="cfg-entry-edit">
                <p class="cfg-entry-question">How much of a long conversation the model can see at once. Larger windows hold long threads coherent; they also use more RAM (KV cache scales linearly).</p>
                {#if setupCtx}
                  <dl class="ctx-readout">
                    <div>
                      <dt>Currently loaded</dt>
                      <dd>{setupCtx.effective?.toLocaleString() ?? '—'} tokens</dd>
                    </div>
                    <div>
                      <dt>Model ceiling</dt>
                      <dd>
                        {#if setupCtx.n_ctx_train}
                          {setupCtx.n_ctx_train.toLocaleString()} tokens (gguf <code>n_ctx_train</code>)
                        {:else}
                          —
                        {/if}
                      </dd>
                    </div>
                  </dl>
                {/if}
                <div class="inline-field">
                  <input
                    class="cfg-number-input"
                    type="number"
                    min={512}
                    max={setupCtx?.n_ctx_train ?? 1048576}
                    bind:value={setupCtxDraft}
                    disabled={setupCtxBusy}
                    aria-label="Context window size in tokens"
                  />
                  <span class="inline-field-unit">tokens</span>
                </div>
                {#if setupCtxMessage}
                  <p class="cfg-entry-error">{setupCtxMessage}</p>
                {/if}
                {#if setupCtxBusy}
                  <p class="cfg-entry-progress">Reloading model with new context window… (15-30s)</p>
                {/if}
                <button
                  class="edit-done"
                  onclick={applyContextWindow}
                  disabled={setupCtxBusy || setupCtxDraft == null || setupCtxDraft === setupCtx?.configured}
                >
                  {setupCtxBusy ? 'Reloading…' : 'Apply'}
                </button>
              </div>
            {/if}
          </div>

          <!-- Response length (max_tokens) — knob the cutoff chip points
               at. Wired into the KnowledgeQuery / DeepQuery / SimpleQuery
               synthesis prompts so the model knows its budget AND we
               surface a chip when it hits the limit. Both halves
               matter — without the chip the user can't tell why a
               reply ends mid-sentence; without the budget hint the
               model walks into the cutoff with eyes closed. -->
          <div class="cfg-entry" class:cfg-entry--open={editingResponseLength}>
            <button class="cfg-entry-display" onclick={() => editingResponseLength = !editingResponseLength} aria-expanded={editingResponseLength}>
              <span class="cfg-entry-name">Response length</span>
              <span class="cfg-entry-current">
                <span class="cfg-entry-val">{config.max_tokens?.toLocaleString() ?? '—'}</span>
                <span class="cfg-entry-tech">tokens</span>
              </span>
              <span class="cfg-entry-prov">{provenance('max_tokens')}</span>
            </button>
            {#if editingResponseLength}
              <div class="cfg-entry-edit">
                <p class="cfg-entry-question">How long answers can run before they're cut off. The model is told this budget up front so it shapes the answer to fit; if it still runs out, you'll see a chip on the reply offering to continue.</p>
                <div class="preset-row" role="radiogroup" aria-label="Response length preset">
                  <button
                    class="preset-btn"
                    class:preset-btn--active={config.max_tokens === 2048}
                    role="radio"
                    aria-checked={config.max_tokens === 2048}
                    onclick={() => { if (config) { config.max_tokens = 2048; markDirty('max_tokens'); } }}
                  >Concise</button>
                  <button
                    class="preset-btn"
                    class:preset-btn--active={config.max_tokens === 4096}
                    role="radio"
                    aria-checked={config.max_tokens === 4096}
                    onclick={() => { if (config) { config.max_tokens = 4096; markDirty('max_tokens'); } }}
                  >Standard</button>
                  <button
                    class="preset-btn"
                    class:preset-btn--active={config.max_tokens === 8192}
                    role="radio"
                    aria-checked={config.max_tokens === 8192}
                    onclick={() => { if (config) { config.max_tokens = 8192; markDirty('max_tokens'); } }}
                  >Long</button>
                  <button
                    class="preset-btn"
                    class:preset-btn--active={config.max_tokens === 16384}
                    role="radio"
                    aria-checked={config.max_tokens === 16384}
                    onclick={() => { if (config) { config.max_tokens = 16384; markDirty('max_tokens'); } }}
                  >Essay</button>
                </div>
                <div class="inline-field" style="margin-top: 10px;">
                  <input
                    class="cfg-number-input"
                    type="number"
                    min="256"
                    step="256"
                    bind:value={config.max_tokens}
                    oninput={() => markDirty('max_tokens')}
                    aria-label="Maximum response length in tokens"
                  />
                  <span class="inline-field-unit">tokens</span>
                </div>
                <p class="preset-tech">Rule of thumb: ~4 chars per token. 2048 ≈ a tight summary; 4096 ≈ a typical long reply; 16384 ≈ a multi-section deep dive. Bigger budgets use more RAM during generation.</p>
                <button class="edit-done" onclick={() => editingResponseLength = false}>Done</button>
              </div>
            {/if}
          </div>

          <!-- Custom instructions / persona — global standing guidance
               appended as the OUTERMOST layer of every system prompt
               (sovereign_core InferenceConfig.custom_instructions).
               Append-only: it never replaces svrnmesh's situated
               context. Visible verbatim in the Inner Work
               ProvenancePanel so the user can always see what was sent. -->
          <div class="cfg-entry" class:cfg-entry--open={editingPersona}>
            <button class="cfg-entry-display" onclick={() => editingPersona = !editingPersona} aria-expanded={editingPersona}>
              <span class="cfg-entry-name">Custom instructions</span>
              <span class="cfg-entry-current">
                <span class="cfg-entry-val">{config.custom_instructions?.trim() ? 'Set' : 'Off'}</span>
                <span class="cfg-entry-tech">persona</span>
              </span>
            </button>
            {#if editingPersona}
              <div class="cfg-entry-edit">
                <p class="cfg-entry-question">Standing guidance for how the assistant should respond — tone, format, things to always or never do. It's layered on top of svrnmesh's own instructions (never replacing them) and applies to every conversation. Leave empty for default behaviour.</p>
                <textarea
                  class="cfg-textarea"
                  rows="5"
                  maxlength="4000"
                  placeholder="e.g. Be concise and prefer bullet points. Always show your reasoning for any numeric claim. Never start a reply with &quot;Certainly&quot;."
                  value={config.custom_instructions ?? ''}
                  oninput={(e) => {
                    if (!config) return;
                    const v = e.currentTarget.value;
                    config.custom_instructions = v.length ? v : null;
                    markDirty('custom_instructions');
                  }}
                ></textarea>
                <p class="cfg-entry-question">Visible verbatim in Inner Work → provenance (Cmd+?), so you can always see exactly what the model was told.</p>
                <button class="edit-done" onclick={() => editingPersona = false}>Done</button>
              </div>
            {/if}
          </div>

          <!-- Advanced disclosure -->
          <div class="doc-divider"></div>
          <button
            class="adv-toggle"
            onclick={() => showAdvanced = !showAdvanced}
            aria-expanded={showAdvanced}
          >
            <span class="adv-toggle-chev" aria-hidden="true">{showAdvanced ? '▾' : '▸'}</span>
            <span class="adv-toggle-label">Advanced</span>
            <span class="adv-toggle-hint">Self-checking, idle behaviour, available tools</span>
          </button>

          {#if showAdvanced}
            <!-- Epistemic-humility audit -->
            <div class="cfg-entry">
              <div class="cfg-entry-display cfg-entry-display--static">
                <span class="cfg-entry-name">Epistemic humility</span>
                <span class="cfg-entry-current">
                  <span class="cfg-entry-val">{config.auto_collaborate ? 'On' : 'Off'}</span>
                  <span class="cfg-entry-tech">auto_collaborate</span>
                </span>
              </div>
              <div class="cfg-entry-edit cfg-entry-edit--always">
                <label class="cfg-toggle-row">
                  <input
                    type="checkbox"
                    bind:checked={config.auto_collaborate}
                    onchange={() => markDirty('auto_collaborate')}
                  />
                  <span class="cfg-toggle-label">When about to answer on thin evidence, ask you for a source instead of guessing.</span>
                </label>
              </div>
            </div>

            <!-- Raw model (naked mode) — model with no svrnmesh affordances -->
            <div class="cfg-entry">
              <div class="cfg-entry-display cfg-entry-display--static">
                <span class="cfg-entry-name">Raw model</span>
                <span class="cfg-entry-current">
                  <span class="cfg-entry-val">{config.naked_mode ? 'On' : 'Off'}</span>
                  <span class="cfg-entry-tech">naked_mode</span>
                </span>
              </div>
              <div class="cfg-entry-edit cfg-entry-edit--always">
                <label class="cfg-toggle-row">
                  <input
                    type="checkbox"
                    bind:checked={config.naked_mode}
                    onchange={() => markDirty('naked_mode')}
                  />
                  <span class="cfg-toggle-label">Run the model raw — no retrieval, routing, grounding checks, or tools. Just the model and your chat history (plus your custom instructions). Best for A/B-ing a model's own behaviour.</span>
                </label>
              </div>
            </div>

            <!-- Auto-escalate to web -->
            <div class="cfg-entry">
              <div class="cfg-entry-display cfg-entry-display--static">
                <span class="cfg-entry-name">Auto-escalate to web</span>
                <span class="cfg-entry-current">
                  <span class="cfg-entry-val">{config.auto_escalate_to_web ? 'On' : 'Off'}</span>
                  <span class="cfg-entry-tech">auto_escalate_to_web</span>
                </span>
              </div>
              <div class="cfg-entry-edit cfg-entry-edit--always">
                <label class="cfg-toggle-row">
                  <input
                    type="checkbox"
                    bind:checked={config.auto_escalate_to_web}
                    onchange={() => markDirty('auto_escalate_to_web')}
                  />
                  <span class="cfg-toggle-label">If your local library can't answer, let the assistant search the web on its own — no permission prompt each time.</span>
                </label>
              </div>
            </div>

            <!-- Primary idle seconds -->
            <div class="cfg-entry" class:cfg-entry--open={editingIdleSecs}>
              <button class="cfg-entry-display" onclick={() => editingIdleSecs = !editingIdleSecs} aria-expanded={editingIdleSecs}>
                <span class="cfg-entry-name">Lazy slot idle timeout</span>
                <span class="cfg-entry-current">
                  <span class="cfg-entry-val">{config.primary_idle_secs ?? 300}</span>
                  <span class="cfg-entry-tech">seconds</span>
                </span>
                <span class="cfg-entry-prov">{provenance('primary_idle_secs')}</span>
              </button>
              {#if editingIdleSecs}
                <div class="cfg-entry-edit">
                  <p class="cfg-entry-question">How long to keep the Main responder loaded after the last question. Shorter frees memory faster; longer keeps follow-up turns instant.</p>
                  <div class="inline-field">
                    <input
                      class="cfg-number-input"
                      type="number"
                      min="30"
                      step="30"
                      bind:value={config.primary_idle_secs}
                      oninput={() => markDirty('primary_idle_secs')}
                      aria-label="Primary slot idle timeout in seconds"
                    />
                    <span class="inline-field-unit">seconds</span>
                  </div>
                  <p class="preset-tech">Default 300 (five minutes). Push to 1800+ if you're hammering the model in batches and don't want it to reload between calls.</p>
                  <button class="edit-done" onclick={() => editingIdleSecs = false}>Done</button>
                </div>
              {/if}
            </div>

            <!-- Enabled tools -->
            <div class="cfg-entry">
              <div class="cfg-entry-display cfg-entry-display--static">
                <span class="cfg-entry-name">Enabled tools</span>
                <span class="cfg-entry-current">
                  <span class="cfg-entry-val">{config.enabled_tools.length} of {TOOL_OPTS.length}</span>
                  <span class="cfg-entry-tech">enabled_tools</span>
                </span>
              </div>
              <div class="cfg-entry-edit cfg-entry-edit--always">
                <p class="cfg-entry-question">The full set of tools the assistant is allowed to invoke deliberately. The assistant still picks which ones make sense each turn — this is the outer fence. Library search the assistant does on its own (the "Wikipedia answered me" path) is separate and not gated here.</p>
                {#each TOOL_OPTS as opt}
                  <label class="cfg-toggle-row">
                    <input
                      type="checkbox"
                      checked={config.enabled_tools.includes(opt.id)}
                      onchange={() => toggleTool(opt.id)}
                    />
                    <span class="cfg-toggle-label"><strong>{opt.label}</strong> — {opt.desc}</span>
                  </label>
                {/each}
              </div>
            </div>
          {/if}

        </section>

      {:else if activeTab === "models"}
        <section class="doc-section">
          <p class="doc-loading">Loading…</p>
        </section>
      {/if}

      <!-- ──────────── MESH ──────────── -->
      {#if activeTab === "mesh"}
        <section class="doc-section">
          <h2 class="doc-h2">Mesh</h2>
          <p class="doc-intro">Pool compute and knowledge with people you trust. Spare cycles and shared libraries — no central server, no broker.</p>
          <MeshSettings />
          <MeshAppsSection />
        </section>
      {/if}


      <!-- ──────────── ACTIVITY & SHARING ──────────── -->
      {#if activeTab === "sharing"}
        <section class="doc-section">
          <h2 class="doc-h2">Activity &amp; Sharing</h2>
          <p class="doc-intro">See exactly what svrnmesh has been doing on this machine — tokens, chunks, embeddings, all local — then take the reins on how hard it works.</p>
          <SharingSection />
        </section>
      {/if}

      <!-- ──────────── WEB SEARCH ──────────── -->
      <!-- Tab used to host both the Skills picker and Web Search
           config; the skills-as-menu UI was retired (intent-keyed
           policy in sovereign_core::intent_policy now drives tool
           selection). Web Search settings remain because the
           provider + API key are operator concerns that don't
           belong to any single mode. -->
      {#if activeTab === "tools" && config}
        {@const provider = config.search_backend.provider}
        {@const needsKey = provider !== "duckduckgo"}
        {@const hasKey = !!config.search_backend.api_key}
        <section class="doc-section">
          <h2 class="doc-h2">Web search</h2>
          <p class="doc-intro">
            Queries go straight to whichever provider you pick — never through us. Comes up when the model needs something it can't find locally.
          </p>

          <h3 class="doc-h3">Provider</h3>
          <div class="provider-grid" role="radiogroup" aria-label="Search provider">
            {#each [
              { id: "duckduckgo", name: "DuckDuckGo", tag: "free",      hint: "Zero-config. No account, no key." },
              { id: "brave",      name: "Brave Search", tag: "needs key", hint: "Independent index. Generous free tier." },
              { id: "tavily",     name: "Tavily",     tag: "needs key", hint: "LLM-tuned results with snippets." },
            ] as opt}
              {@const selected = provider === opt.id}
              <button
                type="button"
                class="provider-card"
                class:provider-card--active={selected}
                role="radio"
                aria-checked={selected}
                onclick={() => {
                  config!.search_backend.provider = opt.id;
                  markDirty('search_provider');
                }}
              >
                <span class="provider-card-head">
                  <span class="provider-card-name">{opt.name}</span>
                  <span class="provider-card-tag provider-card-tag--{opt.tag === 'free' ? 'free' : 'paid'}">
                    {opt.tag}
                  </span>
                </span>
                <span class="provider-card-hint">{opt.hint}</span>
              </button>
            {/each}
          </div>

          {#if needsKey}
            <h3 class="doc-h3">API key</h3>
            <div class="surface-card">
              <div class="cfg-entry">
                <div class="cfg-entry-edit cfg-entry-edit--always">
                  <div class="inline-field">
                    <input
                      class="cfg-text-input"
                      type="password"
                      placeholder="paste your {provider === 'brave' ? 'Brave' : 'Tavily'} API key"
                      value={config.search_backend.api_key ?? ""}
                      oninput={(e) => {
                        config!.search_backend.api_key = (e.target as HTMLInputElement).value || null;
                        markDirty('search_api_key');
                      }}
                      aria-label="Search API key"
                    />
                    {#if hasKey}
                      <span class="provider-card-tag provider-card-tag--free" aria-hidden="true">saved</span>
                    {/if}
                  </div>
                  <p class="cfg-caution" style="margin-top: 8px;">
                    Saved in plain text inside <code class="path-inline">config.toml</code>. Use a key you can rotate if it ever gets exposed.
                  </p>
                </div>
              </div>
            </div>
          {/if}
        </section>

      {:else if activeTab === "tools"}
        <section class="doc-section">
          <p class="doc-loading">Loading…</p>
        </section>
      {/if}

      <!-- ──────────── PATHS ──────────── -->
      {#if activeTab === "paths" && config}
        <section class="doc-section">
          <h2 class="doc-h2">Paths</h2>
          <p class="doc-intro">Created automatically on first run. Change them only if you want data stored somewhere specific.</p>

          <div class="cfg-entry" class:cfg-entry--open={editingPaths}>
            <button class="cfg-entry-display" onclick={() => editingPaths = !editingPaths} aria-expanded={editingPaths}>
              <span class="cfg-entry-name">Data directory</span>
              <span class="cfg-entry-current">
                <span class="cfg-entry-val">{config.data_dir || 'default'}</span>
                {#if !config.data_dir}
                  <span class="cfg-entry-tech">~/.local/share/svrnmesh</span>
                {/if}
              </span>
              <span class="cfg-entry-prov">{provenance('data_dir')}</span>
            </button>
            {#if editingPaths}
              <div class="cfg-entry-edit">
                <div class="path-field-group">
                  <label class="path-label">
                    <span class="cfg-entry-name">Data directory</span>
                    <input
                      class="cfg-path-input"
                      type="text"
                      bind:value={config.data_dir}
                      oninput={() => markDirty('data_dir')}
                      placeholder="~/.local/share/svrnmesh"
                      aria-label="Data directory path"
                    />
                  </label>
                  <label class="path-label">
                    <span class="cfg-entry-name">Skills directory</span>
                    <input
                      class="cfg-path-input"
                      type="text"
                      bind:value={config.skills_dir}
                      oninput={() => markDirty('skills_dir')}
                      placeholder="data_dir/skills"
                      aria-label="Skills directory path"
                    />
                  </label>
                </div>
                <button class="edit-done" onclick={() => editingPaths = false}>Done</button>
              </div>
            {/if}
          </div>
        </section>

      {:else if activeTab === "paths"}
        <section class="doc-section">
          <p class="doc-loading">Loading…</p>
        </section>
      {/if}

      <!-- ──────────── MOBILE ACCESS (opt-in phone host) ──────────── -->
      {#if activeTab === "mobile" && config}
        <section class="doc-section">
          <h2 class="doc-h2">Mobile access</h2>
          <p class="doc-intro">Serve the svrnmesh mobile app from this machine. The app pairs with a lightweight host here that forwards every chat and search to your already-running models — it loads no second copy of them. Pair with the no-VPN code (works from any network), or over your Tailscale tailnet.</p>

          <div class="cfg-entry cfg-entry--toggle">
            <label class="cfg-toggle-row">
              <input
                type="checkbox"
                bind:checked={config.mobile_access_enabled}
                onchange={onMobileToggle}
                class="cfg-checkbox"
                data-testid="settings-mobile-access-toggle"
              />
              <span class="cfg-toggle-body">
                <span class="cfg-toggle-label">Enable mobile access</span>
                <span class="cfg-toggle-sub">Starts a phone-facing host on this node, supervised + auto-restarted. Requires your daemon to be running — the host delegates all inference to it.</span>
              </span>
            </label>
          </div>

          {#if config.mobile_access_enabled && mobilePairing}
            <div class="doc-divider"></div>
            <h3 class="doc-h3">Pair your phone</h3>
            <p class="doc-body">Fastest path: scan the QR with the phone camera (or hit Copy and use the shared clipboard), then paste into the app's address field — it fills everything. Or enter the values by hand:</p>
            {#if mobileQrUrl}
              <div style="display:flex; gap:1.1rem; align-items:center; margin:0.6rem 0;">
                <img src={mobileQrUrl} alt="Pairing QR code" width="150" height="150"
                     style="border-radius:8px; background:white; padding:6px;" />
                <button class="connect-copy" onclick={copyPairing}
                        style="padding:0.5rem 0.9rem; border-radius:6px; border:1px solid var(--border-mid, #444); background:transparent; color:inherit; cursor:pointer;">
                  {mobileCopied ? "Copied ✓" : "Copy pairing code"}
                </button>
              </div>
            {/if}
            <dl style="display:grid; gap:0.5rem; margin:0.6rem 0 0.4rem;">
              <div style="display:flex; gap:0.9rem; align-items:baseline;">
                <dt style="min-width:5.5rem; opacity:0.66;">No-VPN code</dt>
                {#if mobilePairing.iroh_dial}
                  <dd style="font-family:var(--font-mono, ui-monospace, monospace); user-select:all; word-break:break-all;" data-testid="settings-mobile-iroh-dial">{mobilePairing.iroh_dial}</dd>
                {:else}
                  <dd style="opacity:0.55;">preparing… (the host is starting or reaching its relay)</dd>
                {/if}
              </div>
              <div style="display:flex; gap:0.9rem; align-items:baseline;">
                <dt style="min-width:5.5rem; opacity:0.66;">Tailnet</dt>
                <dd style="font-family:var(--font-mono, ui-monospace, monospace); user-select:all;">{mobilePairing.address}</dd>
              </div>
              <div style="display:flex; gap:0.9rem; align-items:baseline;">
                <dt style="min-width:5.5rem; opacity:0.66;">Tenant</dt>
                <dd style="font-family:var(--font-mono, ui-monospace, monospace); user-select:all;">{mobilePairing.tenant}</dd>
              </div>
              <div style="display:flex; gap:0.9rem; align-items:baseline;">
                <dt style="min-width:5.5rem; opacity:0.66;">Token</dt>
                <dd style="font-family:var(--font-mono, ui-monospace, monospace); user-select:all; word-break:break-all;">{mobilePairing.token}</dd>
              </div>
            </dl>
            <p class="doc-body" style="opacity:0.66;">The no-VPN code works from any network (relay-assisted, end-to-end encrypted). The tailnet address requires both this machine and the phone on your Tailscale tailnet.</p>
          {/if}
          {#if mobileError}
            <p class="doc-body" style="color:var(--danger, #e5837a);">{mobileError}</p>
          {/if}
        </section>
      {/if}

      <!-- ──────────── DIAGNOSTICS (crash & panic records) ──────────── -->
      {#if activeTab === "diagnostics"}
        <CrashRecordsPanel />
      {/if}

      <!-- ──────────── ABOUT (version + updater) ──────────── -->
      {#if activeTab === "about"}
        <section class="doc-section">
          <h2 class="doc-h2">About</h2>
          <p class="doc-intro">Version info and updates. Releases are signed at <code>svrnme.sh</code> and verified on this machine before they install.</p>
          <UpdatesSection />
          <SetupReportCard />
        </section>
      {/if}

      <!-- ── Save bar ────────────────────────────────────────── -->
      {#if needsSave}
        {@const blockSave = activeTab === "models" && budgetState === "crit"}
        <div class="doc-save" class:doc-save--visible={dirty || !!saveMessage}>
          <button
            class="save-btn"
            onclick={handleSave}
            disabled={saving || !dirty || blockSave}
            aria-label="Save and apply settings"
            title={blockSave ? "Resolve the memory budget warning above before saving." : undefined}
          >
            {saving ? "Saving…" : "Save"}
          </button>
          {#if saveMessage}
            <span class="save-msg" class:save-msg--error={saveMessage.startsWith("Could")}>
              {saveMessage}
            </span>
          {:else if blockSave}
            <span class="save-msg save-msg--error">Over the memory budget — adjust models above.</span>
          {:else if dirty}
            <span class="save-msg save-msg--pending">Unsaved changes</span>
          {/if}
        </div>
      {/if}

    </div><!-- /cfg-doc -->
  </div><!-- /cfg-body -->
</div><!-- /cfg -->

<style>
  /* ── Root shell ─────────────────────────────────────────────── */
  .cfg {
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-primary);
  }

  /* ── Header ─────────────────────────────────────────────────── */
  .cfg-head {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 0 14px 0 20px;
    height: 48px;
    flex-shrink: 0;
    border-bottom: 1px solid var(--border);
    background: var(--bg-secondary);
  }

  .cfg-wordmark {
    font-size: 0.7rem;
    font-weight: 600;
    color: var(--text-muted);
    letter-spacing: 0.1em;
    text-transform: uppercase;
    flex-shrink: 0;
    font-family: var(--font-mono);
  }

  .cfg-search-wrap {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 7px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 0 10px;
    height: 30px;
    transition: border-color 0.15s;
    max-width: 320px;
  }

  .cfg-search-wrap:focus-within {
    border-color: var(--lavender);
  }

  .cfg-search-icon {
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .cfg-search {
    flex: 1;
    background: none;
    border: none;
    outline: none;
    font-size: 0.8rem;
    color: var(--text-primary);
    font-family: var(--font-sans);
  }

  .cfg-search::placeholder {
    color: var(--text-muted);
  }

  .cfg-search::-webkit-search-cancel-button {
    opacity: 0.4;
  }

  .cfg-close {
    color: var(--text-muted);
    padding: 6px;
    border-radius: var(--radius);
    display: flex;
    align-items: center;
    justify-content: center;
    transition: color 0.15s, background 0.15s;
    margin-left: auto;
  }

  .cfg-close:hover {
    color: var(--text-primary);
    background: var(--bg-surface);
  }

  /* ── Two-column body ─────────────────────────────────────────── */
  .cfg-body {
    flex: 1;
    display: flex;
    overflow: hidden;
  }

  /* ── Table of contents ───────────────────────────────────────── */
  .cfg-toc {
    width: 136px;
    flex-shrink: 0;
    border-right: 1px solid var(--border);
    background: var(--bg-secondary);
    display: flex;
    flex-direction: column;
    padding: 14px 0;
    gap: 0;
    overflow-y: auto;
  }

  /* Cluster headers — plumbing vs the operator cluster (D7). Not
     `.toc-item` and not buttons, so nav-by-label selectors are unaffected. */
  .toc-group-header {
    font-size: 0.62rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-muted);
    opacity: 0.65;
    padding: 12px 16px 4px;
    user-select: none;
  }
  .cfg-toc > .toc-group-header:first-child {
    padding-top: 2px;
  }

  .toc-item {
    display: block;
    width: 100%;
    text-align: left;
    padding: 7px 16px;
    font-size: 0.8rem;
    font-weight: 400;
    color: var(--text-muted);
    background: none;
    border: none;
    border-left: 2px solid transparent;
    cursor: pointer;
    letter-spacing: 0.01em;
    transition: color 0.12s, border-color 0.12s, background 0.12s;
    line-height: 1.3;
  }

  .toc-item:hover {
    color: var(--text-secondary);
    background: rgba(155, 135, 196, 0.04);
  }

  .toc-item--active {
    color: var(--text-primary);
    font-weight: 500;
    border-left-color: var(--accent);
    background: rgba(201, 168, 76, 0.04);
  }

  .toc-empty {
    padding: 10px 16px;
    font-size: 0.76rem;
    color: var(--text-muted);
    font-style: italic;
  }

  .toc-pending {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 7px 16px;
    font-size: 0.68rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    letter-spacing: 0.04em;
    border-top: 1px solid var(--border);
    margin-top: auto;
  }

  .toc-pending-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--accent);
    flex-shrink: 0;
    box-shadow: 0 0 4px rgba(201, 168, 76, 0.4);
  }

  /* ── Document ────────────────────────────────────────────────── */
  .cfg-doc {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }

  .doc-section {
    flex: 1;
    padding: 28px 28px 24px;
    max-width: 660px;
  }

  /* ── Document typography ─────────────────────────────────────── */
  .doc-h2 {
    font-size: 1.05rem;
    font-weight: 600;
    color: var(--text-primary);
    letter-spacing: -0.015em;
    margin-bottom: 8px;
    line-height: 1.2;
  }

  .doc-h3 {
    font-size: 0.72rem;
    font-weight: 700;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.1em;
    margin: 22px 0 10px;
  }

  .doc-intro {
    font-size: 0.82rem;
    color: var(--text-muted);
    line-height: 1.6;
    margin-bottom: 18px;
  }

  .doc-body {
    font-size: 0.82rem;
    color: var(--text-muted);
    line-height: 1.6;
    margin-bottom: 12px;
  }


  .doc-note {
    font-size: 0.78rem;
    color: var(--text-secondary);
    line-height: 1.5;
    padding: 10px 14px;
    background: rgba(121, 196, 120, 0.06);
    border: 1px solid rgba(121, 196, 120, 0.18);
    border-radius: var(--radius);
    margin-bottom: 18px;
  }

  .doc-note code {
    font-family: var(--font-mono);
    font-size: 0.8em;
    background: var(--bg-surface);
    padding: 1px 5px;
    border-radius: 3px;
    color: var(--growth);
  }

  .doc-divider {
    height: 1px;
    background: var(--border);
    margin: 22px 0 4px;
  }

  /* ── Section eyebrow — small accent label above each h3 / h2.
        Sans uppercase matches the existing `.doc-h3` eyebrow voice;
        serif in this system is reserved for assistant utterance,
        not UI chrome. Adds rhythm without inventing a parallel
        type treatment. ── */
  .section-eyebrow {
    display: block;
    font-family: var(--font-sans);
    font-size: 0.66rem;
    font-weight: 600;
    color: var(--lavender);
    letter-spacing: 0.12em;
    text-transform: uppercase;
    margin: 24px 0 -2px;
  }
  .section-eyebrow:first-child {
    margin-top: 0;
    margin-bottom: 4px;
  }

  /* ── Surface card — soft contained block for Knowledge sub-
        sections (Disk budget, KnowledgeView, Background ingest).
        Replaces the flat-list rhythm with a card-on-substrate
        composition so each control reads as its own object. ── */
  .surface-card {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 16px 18px;
    margin-top: 10px;
  }
  .surface-card :global(.cfg-entry) {
    border-bottom: none;
    border-top: none;
  }

  /* Inline file-path code style — matches the doc-body voice without
     pulling in monospace's heavier weight. Used for the
     `~/.svrnmesh/indexes/` reference in the Catalog corpora lede. */
  .path-inline {
    font-family: var(--font-mono);
    font-size: 0.82em;
    padding: 1px 5px;
    border-radius: 3px;
    background: var(--bg-elevated);
    color: var(--text-secondary);
  }

  .doc-loading {
    font-size: 0.82rem;
    color: var(--text-muted);
    padding: 40px 0;
    text-align: center;
    font-style: italic;
  }

  /* ── Memory budget meter (Models tab) ────────────────────────── */
  .budget-meter {
    border: 1px solid var(--border-mid);
    border-left-width: 3px;
    border-radius: var(--radius);
    padding: 12px 14px;
    margin: 14px 0 18px;
    background: var(--bg-secondary);
  }
  .budget-meter--ok   { border-left-color: var(--success, #6bbf6b); }
  .budget-meter--warn { border-left-color: var(--warning, #c9a84c); }
  .budget-meter--crit { border-left-color: var(--error,   #d96b6b); }

  .budget-meter-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 8px;
  }
  .budget-meter-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .budget-meter-label {
    font-family: var(--font-sans);
    font-size: 0.66rem;
    font-weight: 600;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .budget-meter-figure {
    font-size: 0.95rem;
    color: var(--text-primary);
    font-feature-settings: "tnum" 1;
  }
  .budget-meter-figure strong {
    font-weight: 600;
  }
  .budget-meter-of {
    color: var(--text-muted);
    font-weight: 400;
    margin-left: 4px;
  }
  .budget-meter-pct {
    font-family: var(--font-mono);
    font-size: 0.88rem;
    color: var(--text-secondary);
  }
  .budget-meter--warn .budget-meter-pct { color: var(--warning, #c9a84c); }
  .budget-meter--crit .budget-meter-pct { color: var(--error,   #d96b6b); }

  .budget-bar-track {
    position: relative;
    height: 6px;
    background: var(--bg-input);
    border-radius: 3px;
    overflow: hidden;
  }
  .budget-bar-fill {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--success, #6bbf6b);
    transition: width 200ms ease;
  }
  .budget-meter--warn .budget-bar-fill { background: var(--warning, #c9a84c); }
  .budget-meter--crit .budget-bar-fill { background: var(--error,   #d96b6b); }
  /* Over-budget overlay extends past 100% as a slim hatched marker so
     the user sees how far past the ceiling they've gone. */
  .budget-bar-over {
    position: absolute;
    inset: 0 0 0 auto;
    background: repeating-linear-gradient(
      45deg,
      var(--error, #d96b6b) 0 6px,
      transparent 6px 12px
    );
    opacity: 0.85;
  }

  .budget-meter-msg {
    font-size: 0.82rem;
    color: var(--text-secondary);
    line-height: 1.45;
    margin: 8px 0 0;
  }
  .budget-meter-msg--ok {
    color: var(--text-muted);
  }
  .budget-meter--warn .budget-meter-msg { color: var(--warning, #c9a84c); }
  .budget-meter--crit .budget-meter-msg { color: var(--error,   #d96b6b); }

  /* ── Model slot list ─────────────────────────────────────────── */
  .slot-list {
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    overflow: hidden;
    margin-bottom: 4px;
    background: var(--bg-primary);
  }

  .slot-item {
    border-bottom: 1px solid var(--border);
    position: relative;
  }

  /* State-colored left rail. Each slot role gets its own accent so
     the four rows read as distinct surfaces rather than a uniform
     stack: gold for the always-loaded Quick responder, lavender for
     the on-demand Main responder, growth for the embedder (the slot
     that makes your library searchable), muted for the optional
     Code specialist. The rail brightens when the slot is open. */
  .slot-item::before {
    content: "";
    position: absolute;
    inset: 0 auto 0 0;
    width: 3px;
    background: var(--border-mid);
    transition: background 140ms ease;
  }
  .slot-item[data-role="fast"]::before     { background: var(--accent); }
  .slot-item[data-role="primary"]::before  { background: var(--lavender); }
  .slot-item[data-role="embed"]::before    { background: var(--growth); }
  .slot-item[data-role="code"]::before     { background: var(--border-bright); }
  .slot-item--open::before { filter: brightness(1.25); }

  .slot-item:last-child {
    border-bottom: none;
  }

  .slot-item-row {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    text-align: left;
    padding: 14px 16px 14px 19px;
    background: none;
    border: none;
    cursor: pointer;
    transition: background 0.12s;
    flex-wrap: wrap;
  }

  .slot-item-row:hover {
    background: rgba(155, 135, 196, 0.05);
  }

  .slot-item--open .slot-item-row {
    background: rgba(201, 168, 76, 0.05);
  }

  .slot-item-role {
    font-family: var(--font-sans);
    font-size: 0.88rem;
    font-weight: 600;
    letter-spacing: -0.005em;
    color: var(--text-primary);
    min-width: 150px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .slot-item-sub {
    font-family: var(--font-sans);
    font-size: 0.72rem;
    font-weight: 400;
    color: var(--text-muted);
    letter-spacing: 0.005em;
  }

  .slot-item-file {
    font-size: 0.78rem;
    font-family: var(--font-mono);
    color: var(--success);
    flex: 1;
    word-break: break-all;
    line-height: 1.3;
  }

  .slot-item-unset {
    color: var(--text-muted);
    font-style: italic;
    font-family: var(--font-sans);
    font-size: 0.78rem;
  }

  .slot-item-unset--warn {
    color: var(--warning);
    font-style: normal;
    font-family: var(--font-mono);
  }

  .slot-item-size {
    display: inline-block;
    margin-left: 8px;
    padding: 1px 6px;
    font-size: 0.72rem;
    color: var(--text-muted);
    background: var(--bg-input);
    border-radius: 4px;
    white-space: nowrap;
  }

  .slot-item-meta {
    font-size: 0.66rem;
    font-family: var(--font-sans);
    font-weight: 600;
    color: var(--text-muted);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    flex-shrink: 0;
    padding: 2px 8px;
    border: 1px solid var(--border-mid);
    border-radius: 999px;
    background: var(--bg-secondary);
  }
  .slot-item[data-role="fast"] .slot-item-meta    { border-color: rgba(201, 168, 76, 0.35); color: var(--accent-light); }
  .slot-item[data-role="primary"] .slot-item-meta { border-color: rgba(155, 135, 196, 0.35); color: var(--lavender-light); }
  .slot-item[data-role="embed"] .slot-item-meta   { border-color: rgba(121, 196, 120, 0.35); color: var(--growth); }

  .slot-item-chevron {
    font-size: 0.7rem;
    color: var(--text-muted);
    flex-shrink: 0;
    font-family: var(--font-mono);
  }

  .slot-item-body {
    padding: 0 14px 14px;
    border-top: 1px solid var(--border);
    background: var(--bg-secondary);
  }

  .slot-item-desc {
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.5;
    padding: 10px 0 12px;
  }

  .slot-item-controls {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  /* ── Config entries (the core read/edit pattern) ─────────────── */
  .cfg-entry {
    border-bottom: 1px solid var(--border);
    border-top: 1px solid transparent;
  }

  .cfg-entry--open {
    background: rgba(201, 168, 76, 0.025);
  }

  .cfg-entry--toggle {
    background: none;
  }

  .cfg-entry-display {
    display: flex;
    align-items: baseline;
    gap: 12px;
    width: 100%;
    text-align: left;
    padding: 11px 0;
    background: none;
    border: none;
    cursor: pointer;
    flex-wrap: wrap;
  }

  /* Variant for always-visible (non-toggling) entries */
  .cfg-entry-display--static {
    cursor: default;
    pointer-events: none;
  }

  .cfg-entry-display:not(.cfg-entry-display--static):hover .cfg-entry-name {
    color: var(--text-primary);
  }

  .cfg-entry-name {
    font-size: 0.84rem;
    font-weight: 500;
    color: var(--text-secondary);
    min-width: 148px;
    flex-shrink: 0;
    transition: color 0.12s;
  }

  .cfg-entry-current {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex: 1;
    flex-wrap: wrap;
  }

  .cfg-entry-val {
    font-size: 0.84rem;
    color: var(--text-primary);
    font-weight: 400;
  }

  .cfg-entry-tech {
    font-size: 0.68rem;
    font-family: var(--font-mono);
    color: var(--text-muted);
    letter-spacing: 0.03em;
    opacity: 0.7;
  }

  .cfg-entry-prov {
    font-size: 0.67rem;
    font-family: var(--font-mono);
    color: var(--text-muted);
    letter-spacing: 0.04em;
    opacity: 0.5;
    flex-shrink: 0;
    margin-left: auto;
  }

  .cfg-entry-question {
    font-size: 0.8rem;
    color: var(--text-muted);
    line-height: 1.5;
    margin-bottom: 10px;
  }

  /* Edit block — appears below the display row */
  .cfg-entry-edit {
    padding: 0 0 14px;
  }

  /* Variant: always visible (not toggled) */
  .cfg-entry-edit--always {
    padding-bottom: 12px;
  }

  /* ── Advanced disclosure ─────────────────────────────────────── */
  .adv-toggle {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    padding: 10px 0;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: var(--text-primary);
    font: inherit;
  }
  .adv-toggle:hover .adv-toggle-label {
    color: var(--accent);
  }
  .adv-toggle-chev {
    color: var(--text-muted);
    font-size: 0.7rem;
    width: 10px;
  }
  .adv-toggle-label {
    font-size: 0.86rem;
    font-weight: 600;
  }
  .adv-toggle-hint {
    font-size: 0.76rem;
    color: var(--text-muted);
    margin-left: auto;
  }

  /* ── Toggle / checkbox entries ───────────────────────────────── */
  .cfg-toggle-row {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 11px 0;
    cursor: pointer;
    width: 100%;
  }

  .cfg-checkbox {
    margin-top: 2px;
    flex-shrink: 0;
    accent-color: var(--accent);
    width: 14px;
    height: 14px;
    cursor: pointer;
  }

  .cfg-toggle-body {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex: 1;
  }

  .cfg-toggle-label {
    font-size: 0.84rem;
    font-weight: 500;
    color: var(--text-primary);
  }

  .cfg-toggle-sub {
    font-size: 0.76rem;
    color: var(--text-muted);
    line-height: 1.45;
  }

  /* ── Web Search provider cards ───────────────────────────────── */
  .provider-grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
    margin: 4px 0 4px;
  }
  .provider-card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px 14px;
    background: var(--bg-secondary);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    text-align: left;
    cursor: pointer;
    font: inherit;
    color: var(--text-primary);
    transition: border-color 140ms ease, background 140ms ease, transform 80ms ease;
    min-width: 0;
  }
  .provider-card:hover {
    border-color: var(--lavender);
    background: var(--lavender-glow);
  }
  .provider-card:focus-visible {
    outline: 2px solid var(--lavender);
    outline-offset: 2px;
  }
  .provider-card--active {
    border-color: var(--accent);
    background: var(--accent-dim);
  }
  .provider-card--active:hover {
    border-color: var(--accent-light);
    background: var(--accent-dim);
  }
  .provider-card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    min-width: 0;
  }
  .provider-card-name {
    font-size: 0.88rem;
    font-weight: 600;
    color: var(--text-primary);
    letter-spacing: -0.005em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .provider-card--active .provider-card-name {
    color: var(--accent-light);
  }
  .provider-card-tag {
    flex-shrink: 0;
    font-size: 0.62rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid var(--border-mid);
    color: var(--text-muted);
    background: var(--bg-input);
  }
  .provider-card-tag--free {
    color: var(--growth);
    border-color: rgba(121, 196, 120, 0.4);
    background: rgba(121, 196, 120, 0.10);
  }
  .provider-card-tag--paid {
    color: var(--lavender-light);
    border-color: rgba(155, 135, 196, 0.4);
    background: var(--lavender-dim);
  }
  .provider-card-hint {
    font-size: 0.74rem;
    color: var(--text-muted);
    line-height: 1.4;
  }
  .provider-card--active .provider-card-hint {
    color: var(--text-secondary);
  }

  /* ── Preset selector ─────────────────────────────────────────── */
  .preset-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 10px;
    align-items: center;
  }

  .preset-btn {
    padding: 4px 13px;
    border-radius: 100px;
    font-size: 0.76rem;
    font-weight: 500;
    border: 1px solid var(--border-mid);
    background: none;
    color: var(--text-muted);
    cursor: pointer;
    transition: border-color 0.12s, background 0.12s, color 0.12s;
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
    font-size: 0.66rem;
    font-family: var(--font-mono);
    color: var(--text-muted);
    letter-spacing: 0.05em;
    opacity: 0.6;
    padding: 3px 8px;
    border: 1px dashed var(--border-mid);
    border-radius: 100px;
  }

  .preset-desc {
    font-size: 0.77rem;
    color: var(--text-muted);
    line-height: 1.45;
    margin-bottom: 3px;
  }

  .preset-tech {
    font-size: 0.65rem;
    font-family: var(--font-mono);
    color: var(--text-muted);
    opacity: 0.5;
    letter-spacing: 0.04em;
  }

  /* ── Inline fields ───────────────────────────────────────────── */
  .inline-field {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 10px;
  }

  .inline-field-label {
    font-size: 0.8rem;
    color: var(--text-muted);
    min-width: 60px;
  }

  .inline-field-unit {
    font-size: 0.76rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
  }

  .cfg-number-input {
    width: 80px;
    padding: 5px 8px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    outline: none;
    text-align: right;
    font-size: 0.82rem;
    color: var(--text-primary);
    font-family: var(--font-mono);
    transition: border-color 0.15s;
  }

  .cfg-number-input:focus {
    border-color: var(--accent);
  }

  .cfg-textarea {
    width: 100%;
    box-sizing: border-box;
    padding: 8px 10px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    outline: none;
    font-size: 0.82rem;
    line-height: 1.5;
    color: var(--text-primary);
    font-family: inherit;
    resize: vertical;
    min-height: 96px;
    transition: border-color 0.15s;
  }

  .cfg-textarea:focus {
    border-color: var(--accent);
  }

  .cfg-textarea::placeholder {
    color: var(--text-muted);
  }

  .cfg-text-input {
    flex: 1;
    padding: 5px 8px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    outline: none;
    font-size: 0.8rem;
    color: var(--text-primary);
    transition: border-color 0.15s;
  }

  .cfg-text-input:focus {
    border-color: var(--accent);
  }

  .cfg-path-input {
    width: 100%;
    padding: 6px 10px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    outline: none;
    font-size: 0.78rem;
    color: var(--text-primary);
    font-family: var(--font-mono);
    transition: border-color 0.15s;
  }

  .cfg-path-input:focus {
    border-color: var(--accent);
  }

  .cfg-path-input::placeholder {
    color: var(--text-muted);
    opacity: 0.5;
  }

  .cfg-select {
    padding: 5px 8px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    outline: none;
    font-size: 0.8rem;
    color: var(--text-primary);
    font-family: var(--font-sans);
    cursor: pointer;
    appearance: none;
    min-width: 200px;
  }

  .cfg-select:focus {
    border-color: var(--accent);
  }

  .path-field-group {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 4px;
  }

  .path-label {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  /* ── Edit-done button (closes an edit block) ─────────────────── */
  .edit-done {
    margin-top: 12px;
    font-size: 0.72rem;
    color: var(--text-muted);
    padding: 3px 10px;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    background: none;
    cursor: pointer;
    transition: color 0.12s, border-color 0.12s;
    display: block;
  }

  .edit-done:hover {
    color: var(--text-primary);
    border-color: var(--border-bright);
  }

  .edit-row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 8px;
  }

  /* ── Action buttons ──────────────────────────────────────────── */
  .act-btn {
    padding: 4px 12px;
    border-radius: var(--radius);
    font-size: 0.74rem;
    font-weight: 500;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    color: var(--text-secondary);
    cursor: pointer;
    transition: border-color 0.12s, color 0.12s, background 0.12s;
  }

  .act-btn:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--text-primary);
  }

  .act-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .act-btn--ghost {
    background: none;
    border-color: var(--border);
  }

  .act-btn--danger:hover {
    border-color: var(--error) !important;
    color: var(--error) !important;
  }

  /* ── Storage bar ─────────────────────────────────────────────── */
  .storage-bar-wrap {
    height: 2px;
    background: var(--border);
    margin: 0;
    overflow: hidden;
  }

  .storage-bar-fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.25s ease, background 0.25s ease;
  }

  .storage-bar-fill--near {
    background: var(--accent-light);
  }

  .storage-bar-fill--over {
    background: var(--error);
  }

  /* ── Caution + error text ────────────────────────────────────── */
  .cfg-caution {
    font-size: 0.75rem;
    color: var(--warning);
    line-height: 1.4;
    margin-bottom: 8px;
    padding: 7px 10px;
    background: var(--accent-dim);
    border-radius: var(--radius);
  }

  .cfg-error {
    font-size: 0.75rem;
    color: var(--error);
    line-height: 1.4;
    margin-top: 6px;
  }

  /* ── Save bar ────────────────────────────────────────────────── */
  .doc-save {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 28px;
    border-top: 1px solid var(--border);
    background: var(--bg-secondary);
    flex-shrink: 0;
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.2s;
  }

  .doc-save--visible {
    opacity: 1;
    pointer-events: auto;
  }

  .save-btn {
    padding: 7px 18px;
    background: var(--accent);
    color: var(--text-on-accent);
    border-radius: var(--radius);
    font-weight: 600;
    font-size: 0.8rem;
    letter-spacing: 0.02em;
    transition: background 0.15s, opacity 0.15s;
  }

  .save-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .save-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .save-msg {
    font-size: 0.76rem;
    color: var(--success);
    font-family: var(--font-mono);
  }

  .save-msg--error {
    color: var(--error);
  }

  .save-msg--pending {
    color: var(--text-muted);
    font-style: italic;
    font-family: var(--font-sans);
  }
</style>
