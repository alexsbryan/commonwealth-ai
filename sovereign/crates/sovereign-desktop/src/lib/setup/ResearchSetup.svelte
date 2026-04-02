<script lang="ts">
  import { completeSetup } from "../api";
  import ModelSelector from "./ModelSelector.svelte";

  interface Props {
    onComplete: () => void;
    onBack: () => void;
  }

  let { onComplete, onBack }: Props = $props();

  let modelPath = $state("");
  let documentsDir = $state("");
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
        active_skills: ["research-analyst"],
        enabled_tools: [
          "shell",
          "web_search",
          "web_fetch",
          "knowledge",
          "document",
        ],
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
  <h2>Research & Analysis</h2>
  <p class="desc">
    Sovereign will activate the Research Analyst skill with web search
    and document knowledge tools. First, pick a model.
  </p>

  <ModelSelector selectedPath={modelPath} onSelect={(p) => (modelPath = p)} />

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
    margin-top: 16px;
  }

  .submit-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .submit-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
