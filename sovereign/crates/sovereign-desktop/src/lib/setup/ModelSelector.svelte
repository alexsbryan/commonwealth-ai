<script lang="ts">
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { scanForModels, downloadModel, detectHardware } from "../api";
  import type {
    DiscoveredModel,
    DownloadProgress,
    RecommendedModel,
    HardwareInfo,
  } from "../types";

  interface Props {
    selectedPath: string;
    onSelect: (path: string) => void;
    showRawInput?: boolean;
    /** When true, shows embedding-specific model recommendations */
    embedMode?: boolean;
  }

  let { selectedPath, onSelect, showRawInput = false, embedMode = false }: Props = $props();

  let discovered: DiscoveredModel[] = $state([]);
  let scanning = $state(true);
  let downloading: string | null = $state(null);
  let downloadPercent: number = $state(0);
  let downloadedBytes: number = $state(0);
  let downloadTotalBytes: number | null = $state(null);
  let downloadError: string | null = $state(null);
  let showManualInput = $state(false);
  let manualPath = $state("");
  let unlisten: UnlistenFn | null = null;
  let hardware: HardwareInfo | null = $state(null);

  const EMBED_MODELS: RecommendedModel[] = [
    {
      name: "Qwen3-Embedding-0.6B",
      file_name: "Qwen3-Embedding-0.6B-Q8_0.gguf",
      url: "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/main/Qwen3-Embedding-0.6B-Q8_0.gguf",
      size_estimate: "~640 MB",
      ram_minimum: "4 GB",
      description: "Purpose-built retrieval model with last-token pooling. Best default choice for atlas and search.",
      min_ram_gb: 2,
    },
  ];

  const RECOMMENDED_MODELS: RecommendedModel[] = [
    // ── Tiny — any device ──────────────────────────────────────────
    // {
    //   name: "Qwen3-0.6B",
    //   file_name: "Qwen3-0.6B-Q8_0.gguf",
    //   url: "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q8_0.gguf",
    //   size_estimate: "~760 MB",
    //   ram_minimum: "4 GB",
    //   description: "Smallest Qwen3. 200+ t/s on any machine. Good for lightweight tasks and getting started.",
    //   min_ram_gb: 2,
    // },
    {
      name: "Qwen3.5-2B",
      file_name: "Qwen3-2B-Q6_K.gguf",
      url: "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/main/Qwen3.5-2B-Q6_K.gguf",
      size_estimate: "~1.1 GB",
      ram_minimum: "4 GB",
      description: "Step up in quality with still-tiny size. Runs on any device.",
      min_ram_gb: 4,
    },
    // ── Universal daily drivers — fast on every platform ───────────
    // {
    //   name: "SmolLM3-3B",
    //   file_name: "SmolLM3-3B-Q4_K_M.gguf",
    //   url: "https://huggingface.co/unsloth/SmolLM3-3B-GGUF/resolve/main/SmolLM3-3B-Q4_K_M.gguf",
    //   size_estimate: "~1.8 GB",
    //   ram_minimum: "6 GB",
    //   description: "Best 3B available. Maximum throughput at this size class — 180+ t/s everywhere.",
    //   min_ram_gb: 4,
    // },
    // {
    //   name: "Phi-4 Mini",
    //   file_name: "Phi-4-mini-instruct-Q4_K_M.gguf",
    //   url: "https://huggingface.co/unsloth/Phi-4-mini-instruct-GGUF/resolve/main/Phi-4-mini-instruct-Q4_K_M.gguf",
    //   size_estimate: "~2.7 GB",
    //   ram_minimum: "6 GB",
    //   description: "MIT license. Optimized for structured output and pipelines. No thinking overhead.",
    //   min_ram_gb: 6,
    // },
    {
      name: "Qwen3-4B",
      file_name: "Qwen3-4B-Q4_K_M.gguf",
      url: "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf",
      size_estimate: "~2.5 GB",
      ram_minimum: "6 GB",
      description: "Text-only with /no_think mode. Excellent speed-to-quality ratio across all platforms.",
      min_ram_gb: 6,
    },
    {
      name: "Gemma 4 E4B",
      file_name: "gemma-4-E4B-it-Q4_K_M.gguf",
      url: "https://huggingface.co/unsloth/gemma-4-E4B-it-GGUF/resolve/main/gemma-4-E4B-it-Q4_K_M.gguf",
      size_estimate: "~3 GB",
      ram_minimum: "6 GB",
      description: "MoE: 8B total, 4B active. Near Qwen3-8B quality at half the size. 110+ t/s on any GPU.",
      min_ram_gb: 6,
    },
    // ── Standard — best universal choice ──────────────────────────
    {
      name: "Qwen3-8B",
      file_name: "Qwen3-8B-Q4_K_M.gguf",
      url: "https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf",
      size_estimate: "~5 GB",
      ram_minimum: "12 GB",
      description: "Best cross-platform daily driver. Fits in 8GB VRAM. /no_think for structured tasks. 131K context.",
      min_ram_gb: 12,
    },
    {
      name: "Qwen3-14B",
      file_name: "Qwen3-14B-Q4_K_M.gguf",
      url: "https://huggingface.co/Qwen/Qwen3-14B-GGUF/resolve/main/Qwen3-14B-Q4_K_M.gguf",
      size_estimate: "~8.4 GB",
      ram_minimum: "20 GB",
      description: "Dense 14B. Stronger reasoning and coding than 8B. Best option for RTX 4060 users needing higher quality.",
      min_ram_gb: 20,
    },
    // ── Workstation — unified-memory systems (Apple Silicon, Strix Halo) ──
    {
      name: "Qwen3-30B-A3B",
      file_name: "Qwen3-30B-A3B-Q4_K_M.gguf",
      url: "https://huggingface.co/Qwen/Qwen3-30B-A3B-GGUF/resolve/main/Qwen3-30B-A3B-Q4_K_M.gguf",
      size_estimate: "~17 GB",
      ram_minimum: "24 GB",
      description: "MoE: 3B active, 30B knowledge. 86 t/s on Strix Halo, ~100 t/s on Apple Silicon. Avoid on 8GB VRAM GPUs.",
      min_ram_gb: 24,
    },
    {
      name: "Gemma 4 26B-A4B",
      file_name: "gemma-4-26B-A4B-it-UD-Q4_K_M.gguf",
      url: "https://huggingface.co/unsloth/gemma-4-26B-A4B-it-GGUF/resolve/main/gemma-4-26B-A4B-it-UD-Q4_K_M.gguf",
      size_estimate: "~15 GB",
      ram_minimum: "24 GB",
      description: "Arena-tier MoE quality. 4B active params. Fast on unified-memory systems. Slow on 8GB VRAM GPUs.",
      min_ram_gb: 24,
    },
    {
      name: "Qwen3.6-35B-A3B",
      file_name: "Qwen3.6-35B-A3B-UD-Q4_K_M.gguf",
      url: "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/resolve/main/Qwen3.6-35B-A3B-UD-Q4_K_M.gguf",
      size_estimate: "~20 GB",
      ram_minimum: "32 GB",
      description: "Best coding MoE. 3B active, 35B knowledge. 64 t/s on Strix Halo, ~90 t/s on Apple Silicon. Avoid on 8GB VRAM GPUs.",
      min_ram_gb: 32,
    },
  ];

  function modelTier(model: RecommendedModel): "basic" | "standard" | "premium" {
    if (model.min_ram_gb <= 10) return "basic";
    if (model.min_ram_gb <= 30) return "standard";
    return "premium";
  }

  // Active model list — embed models are all small enough to always show.
  let activeModels = $derived(embedMode ? EMBED_MODELS : RECOMMENDED_MODELS);

  // Models that fit in this system's RAM (with a 2 GB headroom),
  // with the recommended pick sorted to the front.
  let visibleModels = $derived.by(() => {
    if (!hardware) return activeModels.slice(0, 3);
    const ram = hardware.system_ram_gb;
    const fits = activeModels.filter((m) => m.min_ram_gb + 2 <= ram);
    if (fits.length === 0) return fits;
    // Recommended = largest that fits. Move it to position 0.
    const recommended = fits[fits.length - 1];
    return [recommended, ...fits.slice(0, fits.length - 1)];
  });

  // First entry is always the recommended pick (after the sort above).
  let recommendedFileName = $derived.by(() => {
    if (!hardware || visibleModels.length === 0) return null;
    return visibleModels[0].file_name;
  });

  onMount(async () => {
    // Listen for download progress events.
    unlisten = await listen<DownloadProgress>(
      "download-progress",
      (event) => {
        const p = event.payload;
        downloadPercent = p.percent ?? 0;
        downloadedBytes = p.downloaded_bytes;
        downloadTotalBytes = p.total_bytes;

        if (p.status === "complete") {
          rescan(p.file_name);
          downloading = null;
        } else if (p.status === "error") {
          downloadError = p.error ?? "Download failed";
          downloading = null;
        }
      },
    );

    // Initial scan.
    try {
      discovered = await scanForModels();
    } catch (e) {
      console.error("Model scan failed:", e);
    }
    scanning = false;

    // Detect hardware to filter recommended models.
    try {
      hardware = await detectHardware();
    } catch (e) {
      console.error("Hardware detection failed:", e);
    }
  });

  async function rescan(autoSelectFileName?: string) {
    try {
      discovered = await scanForModels();
      if (autoSelectFileName) {
        const match = discovered.find((m) =>
          m.file_name === autoSelectFileName,
        );
        if (match) onSelect(match.path);
      }
    } catch {
      // Ignore scan errors on rescan.
    }
  }

  async function handleBrowse() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "GGUF Models", extensions: ["gguf"] }],
    });
    if (selected) {
      onSelect(selected as string);
    }
  }

  async function handleDownload(model: RecommendedModel) {
    // Check if already in discovered list.
    const existing = discovered.find((m) => m.file_name === model.file_name);
    if (existing) {
      onSelect(existing.path);
      return;
    }

    downloading = model.file_name;
    downloadPercent = 0;
    downloadedBytes = 0;
    downloadTotalBytes = null;
    downloadError = null;

    try {
      const path = await downloadModel({
        url: model.url,
        file_name: model.file_name,
        // Pass the advertised size to the backend so its
        // post-download validator can reject stubs sized
        // wildly below what the catalogue claims. The field
        // is `~640 MB`-style human text; we parse it into GB.
        size_gb: parseSizeEstimateToGb(model.size_estimate),
      });
      onSelect(path);
    } catch (e) {
      downloadError = `${e}`;
      downloading = null;
    }
  }

  /** Parse strings like "~640 MB", "~20 GB", "~9.5 GB" to a
   *  floating-point GB value. Returns undefined on unrecognised
   *  input — the backend validator falls back to a 1 MB floor. */
  function parseSizeEstimateToGb(s: string): number | undefined {
    const m = s.match(/([\d.]+)\s*(MB|GB)/i);
    if (!m) return undefined;
    const n = parseFloat(m[1]);
    if (!Number.isFinite(n)) return undefined;
    return m[2].toUpperCase() === "GB" ? n : n / 1024;
  }

  function handleManualSubmit() {
    if (manualPath.trim()) {
      onSelect(manualPath.trim());
    }
  }

  function formatSize(bytes: number): string {
    if (bytes >= 1_000_000_000) {
      return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
    }
    if (bytes >= 1_000_000) {
      return `${(bytes / 1_000_000).toFixed(0)} MB`;
    }
    return `${(bytes / 1_000).toFixed(0)} KB`;
  }

  function formatDownloadProgress(): string {
    const dl = formatSize(downloadedBytes);
    if (downloadTotalBytes) {
      return `${dl} / ${formatSize(downloadTotalBytes)}`;
    }
    return dl;
  }
</script>

<div class="model-selector">
  <!-- Section A: Discovered Models -->
  {#if scanning}
    <div class="section-header">
      <span class="spinner-small"></span> Scanning for models...
    </div>
  {:else if discovered.length > 0}
    <div class="section-header">Models on this machine</div>
    <div class="model-grid">
      {#each discovered as model (model.path)}
        {@const isSelected = selectedPath === model.path}
        <button
          class="model-card"
          class:selected={isSelected}
          onclick={() => onSelect(model.path)}
          aria-pressed={isSelected}
        >
          <span class="card-rail" aria-hidden="true"></span>
          <span class="card-main">
            <span class="model-name">{model.file_name}</span>
            <span class="model-meta">
              <span class="model-size">{formatSize(model.size_bytes)}</span>
              <span class="model-location">{model.location_label}</span>
            </span>
          </span>
          {#if isSelected}
            <span class="using-pill" aria-hidden="true">
              <span class="using-check">✓</span>
              <span class="using-text">Using this</span>
            </span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}

  <!-- Section B: Download a Model -->
  <div class="section-header">
    {discovered.length > 0 ? "Or download a model" : "Download a model"}
    {#if hardware}
      <span class="hw-summary">
        &middot; {hardware.system_ram_gb.toFixed(0)} GB RAM{hardware.gpu_available
          ? ` &middot; GPU: ${hardware.gpu_name ?? "available"}`
          : ""}
      </span>
    {/if}
  </div>
  <div class="model-grid">
    {#each visibleModels as model (model.file_name)}
      {@const alreadyHave = discovered.some(
        (m) => m.file_name === model.file_name,
      )}
      {@const isRecommended = model.file_name === recommendedFileName}
      <div
        class="model-card download-card"
        class:already-have={alreadyHave}
        class:recommended={isRecommended}
      >
        <div class="model-name">
          {model.name}
          {#if !embedMode}
            {@const tier = modelTier(model)}
            {#if tier === "basic"}
              <span class="tier-badge tier-basic">Basic</span>
            {:else if tier === "standard"}
              <span class="tier-badge tier-standard">Standard</span>
            {:else}
              <span class="tier-badge tier-premium"><span class="premium-star">✦</span> Premium</span>
            {/if}
          {/if}
          {#if isRecommended}
            <span class="badge">Recommended for your system</span>
          {/if}
        </div>
        <div class="model-desc">{model.description}</div>
        <div class="model-meta">
          <span class="model-size">{model.size_estimate}</span>
          <span class="model-ram">Needs {model.ram_minimum} RAM</span>
        </div>
        {#if downloading === model.file_name}
          <div class="progress-bar">
            <div
              class="progress-fill"
              style="width: {downloadPercent.toFixed(0)}%"
            ></div>
          </div>
          <div class="progress-text">
            {downloadPercent.toFixed(0)}% &mdash; {formatDownloadProgress()}
          </div>
        {:else if alreadyHave}
          <button
            class="btn-action btn-select"
            onclick={() => {
              const match = discovered.find(
                (m) => m.file_name === model.file_name,
              );
              if (match) onSelect(match.path);
            }}
          >
            Use this model
          </button>
        {:else}
          <button
            class="btn-action btn-download"
            onclick={() => handleDownload(model)}
            disabled={downloading !== null}
          >
            Download
          </button>
        {/if}
      </div>
    {/each}
  </div>

  {#if downloadError}
    <p class="error">{downloadError}</p>
  {/if}

  <!-- Section C: Browse -->
  <div class="browse-row">
    <button class="btn-browse" onclick={handleBrowse}>
      Browse for a model file...
    </button>
  </div>

  <!-- Section D: Manual Input (Developer only) -->
  {#if showRawInput}
    <div class="manual-section">
      <button
        class="manual-toggle"
        onclick={() => (showManualInput = !showManualInput)}
      >
        {showManualInput ? "Hide" : "Or enter path manually"}
      </button>
      {#if showManualInput}
        <div class="manual-input">
          <input
            type="text"
            bind:value={manualPath}
            placeholder="/path/to/model.gguf"
            onkeydown={(e) => e.key === "Enter" && handleManualSubmit()}
          />
          <button
            class="btn-action btn-select"
            onclick={handleManualSubmit}
            disabled={!manualPath.trim()}
          >
            Use
          </button>
        </div>
      {/if}
    </div>
  {/if}

  <!-- Selected indicator — doubles as the slot explainer so the
       user knows what this model is for without needing to read
       docs. -->
  {#if selectedPath}
    <div class="selected-indicator">
      <code>{selectedPath}</code>
      <span class="selected-role">
        {embedMode ? "embedding · retrieval · atlas" : "chat · search · atlas enrichment"}
      </span>
    </div>
  {/if}
</div>

<style>
  .model-selector {
    margin-bottom: 16px;
  }

  .section-header {
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin: 16px 0 10px;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .section-header:first-child {
    margin-top: 0;
  }

  .model-grid {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .model-card {
    position: relative;
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 12px 16px 12px 18px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    text-align: left;
    color: var(--text-primary);
    font-family: var(--font-sans);
    transition:
      border-color 160ms ease,
      background 160ms ease,
      box-shadow 160ms ease;
    width: 100%;
  }

  button.model-card:hover {
    border-color: var(--border-bright);
    background: var(--bg-surface);
  }

  /* Inline rail — lives at the left edge, acts like a page-gutter
     mark. Idle: 2px amethyst tint. Selected: 3px gold foil with
     subtle glow. Immediately legible without dominating color. */
  .card-rail {
    position: absolute;
    left: 0;
    top: 8px;
    bottom: 8px;
    width: 2px;
    background: var(--border-mid);
    border-radius: 1px;
    transition: background 160ms ease, width 160ms ease, box-shadow 160ms ease;
  }
  .card-main {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .model-card.selected {
    border-color: var(--accent);
    background: var(--accent-dim);
    box-shadow:
      inset 0 1px 0 rgba(223, 192, 104, 0.12),
      0 2px 12px var(--accent-glow);
  }
  .model-card.selected .card-rail {
    width: 3px;
    background: var(--accent);
    box-shadow: 0 0 12px rgba(201, 168, 76, 0.55);
  }
  .model-card.selected .model-name {
    color: var(--text-primary);
  }

  /* "Using this" pill — Syne Mono ledger stamp. Gold foil tint on
     a dark inset so it reads as a press mark, not a material chip. */
  .using-pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
    padding: 4px 10px;
    border: 1px solid var(--accent);
    background: var(--bg-root);
    color: var(--accent-light);
    border-radius: 999px;
    font-family: var(--font-mono);
    font-size: 0.65rem;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }
  .using-check {
    color: var(--accent);
    font-size: 0.8rem;
  }
  .using-text {
    line-height: 1;
  }

  .model-name {
    font-weight: 600;
    font-size: 0.92rem;
    color: var(--text-primary);
    margin-bottom: 2px;
    word-break: break-word;
  }

  .model-desc {
    font-size: 0.82rem;
    color: var(--text-secondary);
    margin-bottom: 8px;
    line-height: 1.5;
  }

  .model-meta {
    display: flex;
    gap: 12px;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    color: var(--text-muted);
    letter-spacing: 0.02em;
  }

  .model-size {
    color: var(--text-secondary);
  }

  .download-card {
    cursor: default;
    display: block;
  }

  /* Recommendation wears lavender so it never competes with the
     gold "selected / using this" state on a discovered model.
     Lavender is the court's crown color per the palette notes;
     gold is reserved for the choice the user has already made. */
  .download-card.recommended {
    border-color: var(--lavender);
    background: var(--lavender-dim);
    box-shadow: inset 0 1px 0 rgba(196, 184, 232, 0.12);
  }

  .badge {
    display: inline-block;
    margin-left: 8px;
    padding: 2px 10px;
    border-radius: 999px;
    background: transparent;
    border: 1px solid var(--lavender);
    color: var(--lavender-light);
    font-family: var(--font-mono);
    font-size: 0.62rem;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    font-weight: 500;
    vertical-align: middle;
  }

  .hw-summary {
    font-size: 0.7rem;
    font-weight: 400;
    color: var(--text-muted);
    text-transform: none;
    letter-spacing: normal;
  }

  .btn-action {
    margin-top: 10px;
    padding: 7px 14px;
    border-radius: var(--radius);
    border: 1px solid transparent;
    font-family: var(--font-sans);
    font-weight: 600;
    font-size: 0.85rem;
    letter-spacing: 0.01em;
    cursor: pointer;
    transition: background 160ms ease, border-color 160ms ease, transform 120ms ease;
  }

  /* Download: hollow lavender outline — it's an action on a
     model the user hasn't chosen yet, so it shouldn't claim
     primary-CTA territory (gold is reserved for "selected"). */
  .btn-download {
    background: transparent;
    color: var(--lavender-light);
    border-color: var(--lavender);
  }
  .btn-download:hover:not(:disabled) {
    background: var(--lavender-dim);
    border-color: var(--lavender-light);
    transform: translateY(-1px);
  }
  .btn-download:disabled {
    opacity: 0.38;
    cursor: not-allowed;
  }

  /* Use-this for an already-downloaded model: sage green foil —
     semantic "this one's ready to pick". */
  .btn-select {
    background: var(--growth);
    color: var(--text-on-accent);
    border-color: var(--growth);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.15);
  }
  .btn-select:hover:not(:disabled) {
    background: #8FD28E;
    border-color: #8FD28E;
    transform: translateY(-1px);
  }
  .btn-select:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .progress-bar {
    margin-top: 8px;
    height: 6px;
    background: var(--border);
    border-radius: 3px;
    overflow: hidden;
  }

  .progress-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.3s;
  }

  .progress-text {
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-top: 4px;
  }

  .error {
    color: var(--error);
    font-size: 0.85rem;
    margin-top: 8px;
  }

  .browse-row {
    margin-top: 12px;
  }

  .btn-browse {
    padding: 10px 16px;
    background: var(--bg-surface);
    border: 1px dashed var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
    width: 100%;
    transition:
      border-color 0.15s,
      color 0.15s;
    font-size: 0.9rem;
  }

  .btn-browse:hover {
    border-color: var(--accent);
    color: var(--text-primary);
  }

  .manual-section {
    margin-top: 8px;
  }

  .manual-toggle {
    font-size: 0.8rem;
    color: var(--text-muted);
    text-decoration: underline;
    padding: 4px 0;
  }

  .manual-toggle:hover {
    color: var(--text-primary);
  }

  .manual-input {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }

  .manual-input input {
    flex: 1;
    padding: 8px 12px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
    font-size: 0.85rem;
  }

  .manual-input input:focus {
    border-color: var(--accent);
  }

  .selected-indicator {
    margin-top: 14px;
    padding: 10px 14px;
    background: var(--bg-surface);
    border: 1px solid var(--border-bright);
    border-left: 2px solid var(--accent);
    border-radius: var(--radius);
    font-family: var(--font-mono);
    font-size: 0.72rem;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    display: flex;
    gap: 10px;
    align-items: baseline;
    flex-wrap: wrap;
  }
  .selected-indicator::before {
    content: "DEFAULT MODEL";
    color: var(--accent-light);
    font-size: 0.62rem;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    font-weight: 600;
  }
  .selected-indicator code {
    background: var(--bg-root);
    padding: 2px 6px;
    border-radius: 3px;
    color: var(--text-primary);
    font-size: 0.72rem;
    word-break: break-all;
    border: 1px solid var(--border);
  }
  .selected-role {
    color: var(--text-muted);
    font-size: 0.66rem;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    margin-left: auto;
  }

  /* ── Tier badges ── */
  .tier-badge {
    display: inline-block;
    font-size: 0.6rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    padding: 1px 7px;
    border-radius: 10px;
    vertical-align: middle;
    margin-left: 6px;
  }

  .tier-basic {
    background: rgba(110, 136, 86, 0.15);
    color: var(--text-muted);
    border: 1px solid rgba(110, 136, 86, 0.25);
  }

  .tier-standard {
    background: rgba(74, 186, 216, 0.12);
    color: var(--sky);
    border: 1px solid rgba(74, 186, 216, 0.25);
  }

  .tier-premium {
    background: linear-gradient(
      90deg,
      rgba(201, 168, 76, 0.12) 0%,
      rgba(240, 168, 72, 0.30) 40%,
      rgba(201, 168, 76, 0.12) 80%
    );
    background-size: 200% 100%;
    animation: premium-glimmer 2.5s ease-in-out infinite;
    border: 1px solid rgba(201, 168, 76, 0.4);
    color: var(--accent-light);
  }

  @keyframes premium-glimmer {
    0%   { background-position: 100% 0; }
    100% { background-position: -100% 0; }
  }

  .premium-star {
    display: inline-block;
    animation: star-wobble 5s ease-in-out infinite;
  }

  @keyframes star-wobble {
    0%, 70%, 100% { transform: rotate(0deg) scale(1); }
    73%  { transform: rotate(-18deg) scale(1.3); }
    77%  { transform: rotate(12deg) scale(0.85); }
    81%  { transform: rotate(-8deg) scale(1.1); }
    85%  { transform: rotate(4deg) scale(1); }
  }

  .spinner-small {
    width: 14px;
    height: 14px;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    display: inline-block;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
