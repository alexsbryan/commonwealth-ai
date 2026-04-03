<script lang="ts">
  import { onMount } from "svelte";
  import { getConfig, saveConfig } from "../api";
  import type { DesktopConfig } from "../types";
  import KnowledgeStatus from "./KnowledgeStatus.svelte";
  import SkillManager from "./SkillManager.svelte";

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
        <h3>Model</h3>
        <label>
          <span>Model path</span>
          <input type="text" bind:value={config.model_path} />
        </label>
        <label>
          <span>Primary model (optional)</span>
          <input
            type="text"
            value={config.primary_model_path ?? ""}
            oninput={(e) =>
              (config!.primary_model_path =
                (e.target as HTMLInputElement).value || null)}
          />
        </label>
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
