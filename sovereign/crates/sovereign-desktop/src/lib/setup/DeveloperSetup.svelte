<script lang="ts">
  import { completeSetup } from "../api";
  import ModelSelector from "./ModelSelector.svelte";

  interface Props {
    onComplete: () => void;
    onBack: () => void;
  }

  let { onComplete, onBack }: Props = $props();

  let modelPath = $state("");
  let primaryModelPath = $state("");
  let dataDir = $state("");
  let contextSize = $state(2048);
  let searchProvider = $state("duckduckgo");
  let searchApiKey = $state("");
  let submitting = $state(false);
  let error = $state("");

  async function handleSubmit() {
    if (!modelPath.trim()) {
      error = "Please select a model first.";
      return;
    }
    submitting = true;
    error = "";

    try {
      await completeSetup({
        model_path: modelPath,
        primary_model_path: primaryModelPath || undefined,
        data_dir: dataDir || undefined,
        active_skills: [],
        enabled_tools: [
          "shell",
          "web_search",
          "web_fetch",
          "knowledge",
          "document",
        ],
        search_provider:
          searchProvider !== "duckduckgo" ? searchProvider : undefined,
        search_api_key: searchApiKey || undefined,
      });
      onComplete();
    } catch (e) {
      error = `Setup failed: ${e}`;
    }
    submitting = false;
  }
</script>

<div class="setup-form">
  <button class="back-btn" onclick={onBack}>&larr; Back</button>
  <h2>Developer Setup</h2>
  <p class="desc">
    Full control over model selection, inference settings, and tool
    configuration.
  </p>

  <ModelSelector
    selectedPath={modelPath}
    onSelect={(p) => (modelPath = p)}
    showRawInput={true}
  />

  <div class="section">
    <h3>Advanced</h3>

    <label>
      <span>Primary model (optional, for deep reasoning)</span>
      <input
        type="text"
        bind:value={primaryModelPath}
        placeholder="models/primary.gguf"
      />
    </label>

    <label>
      <span>Context size</span>
      <input type="number" bind:value={contextSize} min="512" step="512" />
    </label>

    <label>
      <span>Data directory</span>
      <input
        type="text"
        bind:value={dataDir}
        placeholder="Default: ~/.local/share/sovereign"
      />
    </label>
  </div>

  <div class="section">
    <h3>Search Backend</h3>
    <label>
      <span>Provider</span>
      <select bind:value={searchProvider}>
        <option value="duckduckgo">DuckDuckGo (free)</option>
        <option value="brave">Brave Search</option>
        <option value="tavily">Tavily</option>
      </select>
    </label>

    {#if searchProvider !== "duckduckgo"}
      <label>
        <span>API Key</span>
        <input type="password" bind:value={searchApiKey} />
      </label>
    {/if}
  </div>

  <div class="info-box">
    <p>
      Config: <code>~/.config/sovereign/desktop.toml</code>
    </p>
    <p>
      Skills: <code>~/.local/share/sovereign/skills/</code>
    </p>
  </div>

  {#if error}
    <p class="error">{error}</p>
  {/if}

  <button
    class="submit-btn"
    onclick={handleSubmit}
    disabled={submitting || !modelPath}
  >
    {submitting ? "Setting up..." : "Start"}
  </button>
</div>

<style>
  .setup-form {
    max-width: 500px;
    width: 100%;
    max-height: 80vh;
    overflow-y: auto;
  }

  .back-btn {
    color: var(--text-muted);
    margin-bottom: 16px;
    font-size: 0.9rem;
  }

  .back-btn:hover {
    color: var(--text-primary);
  }

  h2 {
    font-size: 1.4rem;
    font-weight: 500;
    margin-bottom: 8px;
  }

  .desc {
    color: var(--text-secondary);
    margin-bottom: 16px;
    font-size: 0.9rem;
    line-height: 1.5;
  }

  .section {
    margin-top: 20px;
    margin-bottom: 16px;
  }

  .section h3 {
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin-bottom: 12px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 12px;
  }

  label > span {
    font-size: 0.85rem;
    color: var(--text-secondary);
    font-weight: 500;
  }

  input,
  select {
    padding: 10px 14px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
  }

  input:focus,
  select:focus {
    border-color: var(--accent);
  }

  select {
    appearance: none;
    cursor: pointer;
  }

  .info-box {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px 16px;
    margin-bottom: 16px;
  }

  .info-box p {
    font-size: 0.8rem;
    color: var(--text-muted);
    margin-bottom: 4px;
    line-height: 1.5;
  }

  .info-box p:last-child {
    margin-bottom: 0;
  }

  code {
    background: var(--bg-primary);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.8rem;
  }

  .error {
    color: var(--error);
    font-size: 0.9rem;
    margin-bottom: 12px;
  }

  .submit-btn {
    width: 100%;
    padding: 12px;
    background: var(--accent);
    color: white;
    border-radius: var(--radius);
    font-weight: 500;
    font-size: 1rem;
    transition: background 0.2s;
    margin-top: 8px;
  }

  .submit-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .submit-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
