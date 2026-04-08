<script lang="ts">
  import { onMount } from "svelte";
  import { getConfig, saveConfig } from "../api";
  import type { DesktopConfig } from "../types";
  import KnowledgeStatus from "./KnowledgeStatus.svelte";
  import MeshSettings from "./MeshSettings.svelte";
  import SkillManager from "./SkillManager.svelte";
  import ModelSelector from "../setup/ModelSelector.svelte";
  import RecipeTestingPanel from "./RecipeTestingPanel.svelte";

  interface Props {
    onClose: () => void;
  }

  let { onClose }: Props = $props();

  type Tab = "models" | "knowledge" | "mesh" | "tools" | "paths" | "recipes";
  let activeTab: Tab = $state("models");

  let config: DesktopConfig | null = $state(null);
  let saving = $state(false);
  let saveMessage = $state("");
  let dirty = $state(false);

  onMount(async () => {
    try {
      config = await getConfig();
    } catch (e) {
      console.error("Failed to load config:", e);
    }
  });

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

  let needsSave = $derived(activeTab === "models" || activeTab === "paths");

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

  let activeSlot: "fast" | "reasoning" | "embed" | null = $state(null);

  function modelFileName(path: string): string {
    return path.split(/[\\/]/).pop() ?? path;
  }

  let slotSelectedPath = $derived.by((): string => {
    if (!config || !activeSlot) return "";
    if (activeSlot === "fast") return config.model_path ?? "";
    if (activeSlot === "reasoning") return config.primary_model_path ?? "";
    return config.embed_model_path ?? "";
  });

  function handleSlotSelect(path: string) {
    if (!config || !activeSlot) return;
    if (activeSlot === "fast") config.model_path = path;
    else if (activeSlot === "reasoning") config.primary_model_path = path || null;
    else config.embed_model_path = path || null;
    markDirty();
  }

  const tabs: { id: Tab; label: string }[] = [
    { id: "models",    label: "Models"    },
    { id: "knowledge", label: "Knowledge" },
    { id: "mesh",      label: "Mesh"      },
    { id: "tools",     label: "Tools"     },
    { id: "paths",     label: "Paths"     },
    { id: "recipes",   label: "Recipes"   },
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

          <!-- ── Three-column slot grid ── -->
          <div class="model-slots-grid">

            <div class="slot-card" class:slot-card--active={activeSlot === "fast"}>
              <div class="slot-card-head">
                <span class="slot-card-title">Fast</span>
                <span class="slot-status-badge">Always loaded</span>
              </div>
              <p class="slot-card-desc">Handles most queries and routing. Stays in memory continuously.</p>
              <div class="slot-current">
                {#if config.model_path}
                  <span class="slot-file">{modelFileName(config.model_path)}</span>
                  <div class="slot-btns">
                    <button class="slot-btn" onclick={() => activeSlot = "fast"}>Change</button>
                    <button class="slot-btn slot-btn--clear" onclick={() => { config!.model_path = ""; markDirty(); }}>Clear</button>
                  </div>
                {:else}
                  <span class="slot-empty">No model assigned</span>
                  <button class="slot-btn slot-btn--add" onclick={() => activeSlot = "fast"}>Add model</button>
                {/if}
              </div>
            </div>

            <div class="slot-card" class:slot-card--active={activeSlot === "reasoning"}>
              <div class="slot-card-head">
                <span class="slot-card-title">Reasoning</span>
                <span class="slot-status-badge slot-status-badge--opt">Optional</span>
              </div>
              <p class="slot-card-desc">Loads for complex tasks. Unloads after 60 s of idle time.</p>
              <div class="slot-current">
                {#if config.primary_model_path}
                  <span class="slot-file">{modelFileName(config.primary_model_path)}</span>
                  <div class="slot-btns">
                    <button class="slot-btn" onclick={() => activeSlot = "reasoning"}>Change</button>
                    <button class="slot-btn slot-btn--clear" onclick={() => { config!.primary_model_path = null; markDirty(); }}>Clear</button>
                  </div>
                {:else}
                  <span class="slot-empty">No model assigned</span>
                  <button class="slot-btn slot-btn--add" onclick={() => activeSlot = "reasoning"}>Add model</button>
                {/if}
              </div>
            </div>

            <div class="slot-card" class:slot-card--active={activeSlot === "embed"}>
              <div class="slot-card-head">
                <span class="slot-card-title">Embedding</span>
                <span class="slot-status-badge slot-status-badge--req">For knowledge</span>
              </div>
              <p class="slot-card-desc">Converts text to vectors for local knowledge search.</p>
              <div class="slot-current">
                {#if config.embed_model_path}
                  <span class="slot-file">{modelFileName(config.embed_model_path)}</span>
                  <div class="slot-btns">
                    <button class="slot-btn" onclick={() => activeSlot = "embed"}>Change</button>
                    <button class="slot-btn slot-btn--clear" onclick={() => { config!.embed_model_path = null; markDirty(); }}>Clear</button>
                  </div>
                {:else}
                  <span class="slot-empty">No model assigned</span>
                  <button class="slot-btn slot-btn--add" onclick={() => activeSlot = "embed"}>Add model</button>
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
              No embedding model set. Knowledge base installs will be unavailable until you add one.
            </div>
          {/if}

          <!-- ── Full-width model picker ── -->
          {#if activeSlot}
            <div class="model-picker-row">
              <div class="picker-head">
                <span class="picker-label">
                  {#if activeSlot === "fast"}Fast model{:else if activeSlot === "reasoning"}Reasoning model{:else}Embedding model{/if}
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
          <KnowledgeStatus />
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
</style>
