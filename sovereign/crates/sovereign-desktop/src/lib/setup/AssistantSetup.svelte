<script lang="ts">
  import { completeSetup } from "../api";
  import ModelSelector from "./ModelSelector.svelte";

  interface Props {
    onComplete: () => void;
    onBack: () => void;
  }

  let { onComplete, onBack }: Props = $props();

  let modelPath = $state("");
  let enableWebSearch = $state(true);
  let enableShell = $state(true);
  let enableKnowledge = $state(true);
  let submitting = $state(false);
  let error = $state("");

  async function handleSubmit() {
    if (!modelPath.trim()) {
      error = "Please select a model first.";
      return;
    }
    submitting = true;
    error = "";

    const tools: string[] = [];
    if (enableShell) tools.push("shell");
    if (enableWebSearch) {
      tools.push("web_search");
      tools.push("web_fetch");
    }
    if (enableKnowledge) {
      tools.push("knowledge");
      tools.push("document");
    }

    try {
      await completeSetup({
        model_path: modelPath,
        active_skills: [],
        enabled_tools: tools,
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
  <h2>Personal Assistant</h2>
  <p class="desc">
    A general-purpose assistant. Pick a model, then choose capabilities.
  </p>

  <ModelSelector selectedPath={modelPath} onSelect={(p) => (modelPath = p)} />

  <div class="toggles">
    <h3>Capabilities</h3>

    <label class="toggle-row">
      <input type="checkbox" bind:checked={enableWebSearch} />
      <span>Web search</span>
    </label>

    <label class="toggle-row">
      <input type="checkbox" bind:checked={enableShell} />
      <span>Shell commands</span>
    </label>

    <label class="toggle-row">
      <input type="checkbox" bind:checked={enableKnowledge} />
      <span>Knowledge & documents</span>
    </label>
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

  .toggles {
    margin-top: 16px;
    margin-bottom: 16px;
  }

  .toggles h3 {
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin-bottom: 12px;
  }

  .toggle-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
    margin-bottom: 10px;
    cursor: pointer;
  }

  .toggle-row input {
    width: 16px;
    height: 16px;
    accent-color: var(--accent);
  }

  .toggle-row span {
    font-size: 0.95rem;
    color: var(--text-primary);
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
