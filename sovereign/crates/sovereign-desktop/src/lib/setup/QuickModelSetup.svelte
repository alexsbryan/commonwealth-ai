<!--
  QuickModelSetup — the only wizard step after the Setup collapse.
  Pick a model, optionally toggle capabilities, done.

  Time-to-first-value replacement for the prior persona-gated flow.
  All capabilities default-on; the user can flip them off before
  proceeding, or change them later in Settings.
-->
<script lang="ts">
  import type { SetupConfig } from "../types";
  import ModelSelector from "./ModelSelector.svelte";

  interface Props {
    onNext: (config: SetupConfig) => void;
    /// Error carried over from a prior finishing-step failure
    /// (complete_setup rejected, e.g., model path bad). Cleared by
    /// the user's next action.
    errorMessage?: string;
  }

  let { onNext, errorMessage }: Props = $props();

  let modelPath = $state("");
  // All capabilities default-on. Users rarely *want* to opt out on
  // first launch — they're just choosing between "chat with a model"
  // and "chat with a model that can also do X". If they dislike a
  // capability they can disable it in Settings → Tools.
  let enableWebSearch = $state(true);
  let enableShell = $state(true);
  let enableKnowledge = $state(true);
  // Advanced toggles default-off — power-user surfaces, not the
  // first-launch happy path. Hidden behind a disclosure so the
  // wizard stays readable for the typical user.
  let showAdvanced = $state(false);
  let enableRecipeAuthoring = $state(false);
  let localError = $state("");

  function handleSubmit() {
    if (!modelPath.trim()) {
      localError = "Please select a model first.";
      return;
    }
    localError = "";

    const tools: string[] = [];
    if (enableShell) tools.push("shell");
    if (enableWebSearch || enableKnowledge) {
      tools.push("search");
      tools.push("web_fetch");
    }
    if (enableKnowledge) {
      tools.push("document");
    }

    onNext({
      model_path: modelPath,
      active_skills: [],
      enabled_tools: tools,
      enable_recipe_authoring: enableRecipeAuthoring,
    });
  }
</script>

<div class="setup-form">
  <h2>Pick a model.</h2>
  <p class="desc">
    Inference runs on your machine. Pick something that fits — this is
    your default, and the atlas pipeline uses it to read your content.
    A faster model means faster enrichment; you can add a larger one
    later in Settings.
  </p>

  <ModelSelector selectedPath={modelPath} onSelect={(p) => (modelPath = p)} />

  <div class="toggles">
    <h3>Capabilities</h3>

    <label class="toggle-row">
      <input type="checkbox" bind:checked={enableKnowledge} />
      <span>
        <span class="toggle-title">Knowledge & documents</span>
        <span class="toggle-sub">Ask about your own files, notes, vaults</span>
      </span>
    </label>

    <label class="toggle-row">
      <input type="checkbox" bind:checked={enableWebSearch} />
      <span>
        <span class="toggle-title">Web search</span>
        <span class="toggle-sub">Grounded answers from the live web</span>
      </span>
    </label>

    <label class="toggle-row">
      <input type="checkbox" bind:checked={enableShell} />
      <span>
        <span class="toggle-title">Shell commands</span>
        <span class="toggle-sub">Run local commands on your machine</span>
      </span>
    </label>
  </div>

  <div class="advanced">
    <button
      type="button"
      class="advanced-toggle"
      onclick={() => (showAdvanced = !showAdvanced)}
      aria-expanded={showAdvanced}
      data-testid="setup-advanced-toggle"
    >
      {showAdvanced ? "▾" : "▸"} Advanced
    </button>
    {#if showAdvanced}
      <div class="advanced-body">
        <label class="toggle-row">
          <input
            type="checkbox"
            bind:checked={enableRecipeAuthoring}
            data-testid="setup-recipe-author-toggle"
          />
          <span>
            <span class="toggle-title">Recipe Author workspace</span>
            <span class="toggle-sub">
              A guided workspace for building Sovereign corpus recipes
              by conversation. Surfaces a "Recipe Author →" entry in
              the chat sidebar; can be flipped any time in Settings.
            </span>
          </span>
        </label>
      </div>
    {/if}
  </div>

  {#if localError}
    <p class="error">{localError}</p>
  {:else if errorMessage}
    <p class="error">{errorMessage}</p>
  {/if}

  <button class="submit-btn" onclick={handleSubmit} disabled={!modelPath}>
    Continue
  </button>
</div>

<style>
  .setup-form {
    max-width: 560px;
    width: 100%;
    color: var(--text-primary);
  }

  h2 {
    font-family: var(--font-serif);
    font-style: italic;
    font-size: 1.9rem;
    font-weight: 500;
    color: var(--accent-light);
    margin-bottom: 10px;
    letter-spacing: -0.005em;
    line-height: 1.1;
  }

  .desc {
    color: var(--text-secondary);
    margin-bottom: 22px;
    font-size: 0.92rem;
    line-height: 1.55;
    max-width: 58ch;
  }

  .advanced {
    margin-top: 14px;
    margin-bottom: 18px;
  }
  .advanced-toggle {
    background: transparent;
    border: none;
    color: var(--text-secondary);
    font-size: 0.82rem;
    cursor: pointer;
    padding: 0;
  }
  .advanced-toggle:hover {
    color: var(--accent-light);
  }
  .advanced-body {
    margin-top: 8px;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg, 8px);
    padding: 12px 14px;
  }

  .toggles {
    margin-top: 18px;
    margin-bottom: 18px;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg, 8px);
    overflow: hidden;
  }

  .toggles h3 {
    font-size: 0.68rem;
    font-weight: 700;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.1em;
    padding: 12px 16px 8px;
    border-bottom: 1px solid var(--border);
  }

  .toggle-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    cursor: pointer;
    border-bottom: 1px solid var(--border);
    transition: background 0.15s;
  }

  .toggle-row:last-child {
    border-bottom: none;
  }

  .toggle-row:hover {
    background: var(--bg-elevated);
  }

  .toggle-row input {
    width: 15px;
    height: 15px;
    accent-color: var(--accent);
    flex-shrink: 0;
  }

  .toggle-row span {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .toggle-title {
    font-size: 0.9rem;
    color: var(--text-primary);
  }
  .toggle-sub {
    font-size: 0.76rem;
    color: var(--text-muted);
  }

  .error {
    color: var(--error);
    font-size: 0.9rem;
    margin-bottom: 12px;
  }

  /* Primary CTA: gold-foil stamp on dark text. 1px accent-light
     inner rule reads as a press-struck impression. */
  .submit-btn {
    width: 100%;
    padding: 12px 14px;
    background: var(--accent);
    color: var(--text-on-accent);
    border: 1px solid var(--accent);
    border-radius: var(--radius-lg, 10px);
    font-family: var(--font-sans);
    font-size: 0.96rem;
    font-weight: 600;
    letter-spacing: 0.01em;
    cursor: pointer;
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.18),
      0 1px 0 rgba(0, 0, 0, 0.25);
    transition: background 160ms ease, transform 120ms ease, box-shadow 160ms ease;
  }
  .submit-btn:hover:not(:disabled) {
    background: var(--accent-light);
    transform: translateY(-1px);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.22),
      0 4px 14px var(--accent-glow);
  }
  .submit-btn:active:not(:disabled) {
    transform: translateY(0);
    box-shadow: inset 0 2px 0 rgba(0, 0, 0, 0.18);
  }
  .submit-btn:disabled {
    opacity: 0.42;
    cursor: not-allowed;
  }
</style>
