<script lang="ts">
  import { onMount } from "svelte";
  import { getConfig, saveConfig } from "../api";
  import type { DesktopConfig } from "../types";
  import KnowledgeStatus from "./KnowledgeStatus.svelte";
  import MeshSettings from "./MeshSettings.svelte";
  import SkillManager from "./SkillManager.svelte";
  import ModelSelector from "../setup/ModelSelector.svelte";

  interface Props {
    onClose: () => void;
  }

  let { onClose }: Props = $props();

  let config: DesktopConfig | null = $state(null);
  let saving = $state(false);
  let saveMessage = $state("");

  onMount(async () => {
    try {
      config = await getConfig();
    } catch (e) {
      console.error("Failed to load config:", e);
    }
  });

  async function handleSave() {
    if (!config || saving) return;
    saving = true;
    saveMessage = "";
    try {
      await saveConfig(config);
      saveMessage = "Settings saved. Runtime rebuilt.";
    } catch (e) {
      saveMessage = `Error: ${e}`;
    }
    saving = false;
  }
</script>

<div class="settings-panel">
  <div class="settings-header">
    <h2>Settings</h2>
    <button class="close-btn" onclick={onClose}>&times;</button>
  </div>

  {#if config}
    <div class="settings-body">
      <div class="section">
        <h3>Fast model <em>(always loaded)</em></h3>
        <p class="section-help">
          Used for routing, quick answers, and most queries. Stays in memory.
        </p>
        <ModelSelector
          selectedPath={config.model_path}
          onSelect={(p) => (config!.model_path = p)}
          showRawInput={true}
        />
      </div>

      <div class="section">
        <h3>Deep reasoning model <em>(loads on demand)</em></h3>
        <p class="section-help">
          Larger model for complex reasoning. Loads when needed, unloads after
          60s idle. Optional.
        </p>
        <ModelSelector
          selectedPath={config.primary_model_path ?? ""}
          onSelect={(p) => (config!.primary_model_path = p || null)}
          showRawInput={true}
        />
      </div>

      <div class="section">
        <h3>Embedding model <em>(required for knowledge bases)</em></h3>
        <p class="section-help">
          Dedicated GGUF embedding model. Required to install knowledge
          bases (Wikipedia, SEP, …) and to use RAG over your documents.
          A small model like <code>nomic-embed-text-v1.5.Q4_K_M.gguf</code>
          works well — typically &lt;500 MB.
        </p>
        <ModelSelector
          selectedPath={config.embed_model_path ?? ""}
          onSelect={(p) => (config!.embed_model_path = p || null)}
          showRawInput={true}
        />
        {#if !config.embed_model_path}
          <p class="warning-text">
            ⚠ No embedding model configured. Knowledge base installs will
            fail until you select one.
          </p>
        {/if}
      </div>

      <div class="section">
        <h3>Inference</h3>
        <label>
          <span>Context size</span>
          <input type="number" bind:value={config.context_size} />
        </label>
      </div>

      <div class="section">
        <h3>Storage</h3>
        <label>
          <span>Data directory</span>
          <input type="text" bind:value={config.data_dir} />
        </label>
        <label>
          <span>Skills directory</span>
          <input type="text" bind:value={config.skills_dir} />
        </label>
      </div>

      <div class="section">
        <h3>Knowledge Base</h3>
        <KnowledgeStatus />
      </div>

      <div class="section" id="mesh-section">
        <h3>Community Mesh</h3>
        <p class="section-help">
          Pool compute and knowledge with people you trust. Tap a
          <code>sovereign://join/…</code> link to join an existing mesh,
          or create one to invite friends.
        </p>
        <MeshSettings />
      </div>

      <div class="section">
        <h3>Search</h3>
        <label>
          <span>Provider</span>
          <select bind:value={config.search_backend.provider}>
            <option value="duckduckgo">DuckDuckGo (free)</option>
            <option value="brave">Brave Search</option>
            <option value="tavily">Tavily</option>
          </select>
        </label>
        {#if config.search_backend.provider !== "duckduckgo"}
          <label>
            <span>API Key</span>
            <input
              type="password"
              value={config.search_backend.api_key ?? ""}
              oninput={(e) =>
                (config!.search_backend.api_key =
                  (e.target as HTMLInputElement).value || null)}
            />
          </label>
        {/if}
      </div>

      <div class="section">
        <SkillManager />
      </div>

      <div class="save-area">
        <button class="save-btn" onclick={handleSave} disabled={saving}>
          {saving ? "Saving..." : "Save & Apply"}
        </button>
        {#if saveMessage}
          <span class="save-msg" class:error={saveMessage.startsWith("Error")}>
            {saveMessage}
          </span>
        {/if}
      </div>
    </div>
  {:else}
    <div class="loading">Loading settings...</div>
  {/if}
</div>

<style>
  .settings-panel {
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .settings-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 24px;
    border-bottom: 1px solid var(--border);
  }

  .settings-header h2 {
    font-size: 1.2rem;
    font-weight: 500;
  }

  .close-btn {
    font-size: 1.5rem;
    color: var(--text-muted);
    padding: 4px 8px;
    border-radius: var(--radius);
  }

  .close-btn:hover {
    color: var(--text-primary);
    background: var(--bg-surface);
  }

  .settings-body {
    flex: 1;
    overflow-y: auto;
    padding: 20px 24px;
  }

  .section {
    margin-bottom: 24px;
  }

  .section h3 {
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin-bottom: 12px;
  }

  .section h3 em {
    font-style: normal;
    color: var(--text-muted);
    font-weight: normal;
    text-transform: none;
    letter-spacing: 0;
    font-size: 0.8rem;
  }

  .section-help {
    font-size: 0.78rem;
    color: var(--text-muted);
    margin-top: -6px;
    margin-bottom: 12px;
    line-height: 1.4;
  }

  .section-help code {
    background: var(--bg-input);
    padding: 1px 5px;
    border-radius: 3px;
    font-family: ui-monospace, SFMono-Regular, monospace;
    font-size: 0.74rem;
  }

  .warning-text {
    margin-top: 8px;
    padding: 8px 12px;
    background: rgba(217, 119, 6, 0.1);
    border: 1px solid rgba(217, 119, 6, 0.3);
    border-radius: var(--radius);
    color: rgb(180, 83, 9);
    font-size: 0.78rem;
    line-height: 1.4;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 12px;
  }

  label span {
    font-size: 0.85rem;
    color: var(--text-secondary);
  }

  label span em {
    font-style: normal;
    color: var(--text-muted);
    font-weight: normal;
    font-size: 0.78rem;
  }

  .field-help {
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-top: 4px;
    line-height: 1.4;
  }

  input,
  select {
    padding: 8px 12px;
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

  .save-area {
    display: flex;
    align-items: center;
    gap: 12px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
  }

  .save-btn {
    padding: 10px 24px;
    background: var(--accent);
    color: white;
    border-radius: var(--radius);
    font-weight: 500;
    transition: background 0.2s;
  }

  .save-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .save-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .save-msg {
    font-size: 0.85rem;
    color: var(--success);
  }

  .save-msg.error {
    color: var(--error);
  }

  .loading {
    padding: 2rem;
    color: var(--text-muted);
    text-align: center;
  }
</style>
