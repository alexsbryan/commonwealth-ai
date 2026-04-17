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
      description: "Purpose-built retrieval model with last-token pooling. Best default choice.",
      min_ram_gb: 2,
    },
    {
      name: "mxbai-embed-large-v1",
      file_name: "mxbai-embed-large-v1.Q4_K_M.gguf",
      url: "https://huggingface.co/mixedbread-ai/mxbai-embed-large-v1-GGUF/resolve/main/mxbai-embed-large-v1.Q4_K_M.gguf",
      size_estimate: "~340 MB",
      ram_minimum: "4 GB",
      description: "1024-dim, top-ranked on MTEB. Great retrieval quality.",
      min_ram_gb: 2,
    },
    {
      name: "bge-large-en-v1.5",
      file_name: "bge-large-en-v1.5-q4_k_m.gguf",
      url: "https://huggingface.co/second-state/BGE-large-EN-v1.5-GGUF/resolve/main/bge-large-en-v1.5-q4_k_m.gguf",
      size_estimate: "~330 MB",
      ram_minimum: "4 GB",
      description: "1024-dim, strong semantic search performance.",
      min_ram_gb: 2,
    },
    {
      name: "snowflake-arctic-embed-m-v1.5",
      file_name: "snowflake-arctic-embed-m-v1.5.Q4_K_M.gguf",
      url: "https://huggingface.co/Snowflake/snowflake-arctic-embed-m-v1.5-GGUF/resolve/main/snowflake-arctic-embed-m-v1.5.Q4_K_M.gguf",
      size_estimate: "~120 MB",
      ram_minimum: "4 GB",
      description: "768-dim, compact and accurate for document retrieval.",
      min_ram_gb: 2,
    },
  ];

  const RECOMMENDED_MODELS: RecommendedModel[] = [
    {
      name: "Qwen 2.5 0.5B",
      file_name: "qwen2.5-0.5b-instruct-q8_0.gguf",
      url: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q8_0.gguf",
      size_estimate: "~600 MB",
      ram_minimum: "8 GB",
      description: "Fast and lightweight. Great for getting started.",
      min_ram_gb: 4,
    },
    {
      name: "Qwen 2.5 3B",
      file_name: "qwen2.5-3b-instruct-q4_k_m.gguf",
      url: "https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf",
      size_estimate: "~2 GB",
      ram_minimum: "8 GB",
      description: "Good balance of speed and quality.",
      min_ram_gb: 8,
    },
    {
      name: "Qwen 2.5 7B",
      file_name: "qwen2.5-7b-instruct-q4_k_m.gguf",
      url: "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q4_k_m.gguf",
      size_estimate: "~4.7 GB",
      ram_minimum: "16 GB",
      description: "Strong general-purpose quality.",
      min_ram_gb: 16,
    },
    {
      name: "Qwen 2.5 14B",
      file_name: "qwen2.5-14b-instruct-q4_k_m.gguf",
      url: "https://huggingface.co/Qwen/Qwen2.5-14B-Instruct-GGUF/resolve/main/qwen2.5-14b-instruct-q4_k_m.gguf",
      size_estimate: "~9 GB",
      ram_minimum: "32 GB",
      description: "Higher quality. Better at reasoning and coding.",
      min_ram_gb: 24,
    },
    {
      name: "Qwen 2.5 32B",
      file_name: "qwen2.5-32b-instruct-q4_k_m.gguf",
      url: "https://huggingface.co/Qwen/Qwen2.5-32B-Instruct-GGUF/resolve/main/qwen2.5-32b-instruct-q4_k_m.gguf",
      size_estimate: "~20 GB",
      ram_minimum: "48 GB",
      description: "Excellent quality for deep reasoning workloads.",
      min_ram_gb: 40,
    },
    {
      name: "Qwen 2.5 72B",
      file_name: "qwen2.5-72b-instruct-q4_k_m.gguf",
      url: "https://huggingface.co/Qwen/Qwen2.5-72B-Instruct-GGUF/resolve/main/qwen2.5-72b-instruct-q4_k_m.gguf",
      size_estimate: "~42 GB",
      ram_minimum: "96 GB",
      description: "Frontier-class local model. Requires a workstation.",
      min_ram_gb: 80,
    },
  ];

  function modelTier(model: RecommendedModel): "basic" | "standard" | "premium" {
    if (model.min_ram_gb <= 10) return "basic";
    if (model.min_ram_gb <= 30) return "standard";
    return "premium";
  }

  // Active model list — embed models are all small enough to always show.
  let activeModels = $derived(embedMode ? EMBED_MODELS : RECOMMENDED_MODELS);

  // Models that fit in this system's RAM (with a 2 GB headroom).
  let visibleModels = $derived.by(() => {
    if (!hardware) return activeModels.slice(0, 3);
    const ram = hardware.system_ram_gb;
    return activeModels.filter((m) => m.min_ram_gb + 2 <= ram);
  });

  // Largest model that fits — flagged as the recommendation.
  let recommendedFileName = $derived.by(() => {
    if (!hardware || visibleModels.length === 0) return null;
    return visibleModels[visibleModels.length - 1].file_name;
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
          // Add to discovered list and auto-select.
          const home = "~/.sovereign/models/";
          const newModel: DiscoveredModel = {
            path:
              p.file_name.includes("/")
                ? p.file_name
                : home + p.file_name,
            file_name: p.file_name,
            size_bytes: p.downloaded_bytes,
            location_label: "Sovereign Models",
          };
          // Re-scan to get the real path.
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
      });
      onSelect(path);
    } catch (e) {
      downloadError = `${e}`;
      downloading = null;
    }
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
    <div class="section-header">Models found on this machine</div>
    <div class="model-grid">
      {#each discovered as model (model.path)}
        <button
          class="model-card"
          class:selected={selectedPath === model.path}
          onclick={() => onSelect(model.path)}
        >
          <div class="model-name">{model.file_name}</div>
          <div class="model-meta">
            <span class="model-size">{formatSize(model.size_bytes)}</span>
            <span class="model-location">{model.location_label}</span>
          </div>
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

  <!-- Selected indicator -->
  {#if selectedPath}
    <div class="selected-indicator">
      Selected: <code>{selectedPath}</code>
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
    padding: 12px 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    text-align: left;
    transition:
      border-color 0.15s,
      background 0.15s;
    width: 100%;
  }

  button.model-card:hover {
    border-color: var(--accent);
    background: var(--bg-surface);
  }

  .model-card.selected {
    border-color: var(--accent);
    background: rgba(201, 168, 76, 0.1);
  }

  .model-name {
    font-weight: 600;
    font-size: 0.95rem;
    margin-bottom: 2px;
  }

  .model-desc {
    font-size: 0.8rem;
    color: var(--text-secondary);
    margin-bottom: 6px;
  }

  .model-meta {
    display: flex;
    gap: 12px;
    font-size: 0.8rem;
    color: var(--text-muted);
  }

  .model-size {
    font-weight: 500;
  }

  .download-card {
    cursor: default;
  }

  .download-card.recommended {
    border-color: var(--accent);
    background: rgba(201, 168, 76, 0.07);
  }

  .badge {
    display: inline-block;
    margin-left: 8px;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--accent);
    color: var(--text-on-accent);
    font-size: 0.7rem;
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
    margin-top: 8px;
    padding: 6px 16px;
    border-radius: var(--radius);
    font-weight: 500;
    font-size: 0.85rem;
    transition: background 0.2s;
  }

  .btn-download {
    background: var(--accent);
    color: var(--text-on-accent);
  }

  .btn-download:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .btn-download:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .btn-select {
    background: var(--success);
    color: var(--text-on-accent);
  }

  .btn-select:hover:not(:disabled) {
    background: #6ed876;
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
    margin-top: 12px;
    padding: 8px 12px;
    background: rgba(76, 175, 80, 0.08);
    border: 1px solid var(--success);
    border-radius: var(--radius);
    font-size: 0.8rem;
    color: var(--success);
  }

  .selected-indicator code {
    background: var(--bg-primary);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.8rem;
    word-break: break-all;
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
