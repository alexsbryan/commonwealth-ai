<script lang="ts">
  import { completeSetup } from "../api";
  import type { SetupConfig } from "../types";
  import ResearchSetup from "./ResearchSetup.svelte";
  import AssistantSetup from "./AssistantSetup.svelte";
  import DeveloperSetup from "./DeveloperSetup.svelte";
  import KnowledgeBaseSetup from "./KnowledgeBaseSetup.svelte";
  import WebSearchSetup from "./WebSearchSetup.svelte";

  interface Props {
    onComplete: () => void;
  }

  let { onComplete }: Props = $props();

  type Persona = "research" | "assistant" | "developer";
  type Step = "persona" | "persona-setup" | "knowledge" | "websearch" | "finishing";

  let selectedPersona: Persona | null = $state(null);
  let step: Step = $state("persona");
  let partialConfig: SetupConfig | null = $state(null);
  let submitting = $state(false);
  let error = $state("");

  function handlePersonaNext(config: SetupConfig) {
    partialConfig = config;
    step = "knowledge";
  }

  function handleKnowledgeSelect(tierId: string) {
    if (partialConfig) partialConfig.selected_tier = tierId;
    // Developer already configured search in their setup step.
    if (selectedPersona === "developer") {
      finishSetup();
    } else {
      step = "websearch";
    }
  }

  function handleKnowledgeSkip() {
    if (selectedPersona === "developer") {
      finishSetup();
    } else {
      step = "websearch";
    }
  }

  function handleWebConfigure(provider: string, apiKey: string | null) {
    if (partialConfig) {
      partialConfig.search_provider = provider !== "duckduckgo" ? provider : undefined;
      partialConfig.search_api_key = apiKey ?? undefined;
    }
    finishSetup();
  }

  function handleWebSkip() {
    finishSetup();
  }

  async function finishSetup() {
    if (!partialConfig) return;
    step = "finishing";
    submitting = true;
    error = "";
    try {
      await completeSetup(partialConfig);
      onComplete();
    } catch (e) {
      error = `Setup failed: ${e}`;
      step = "knowledge";
    }
    submitting = false;
  }
</script>

<div class="wizard">
  {#if step === "persona"}
    <div class="persona-select">
      <h1>Welcome to Sovereign</h1>
      <p class="subtitle">How will you use Sovereign?</p>

      <div class="persona-cards">
        <button
          class="persona-card"
          onclick={() => { selectedPersona = "research"; step = "persona-setup"; }}
        >
          <div class="persona-icon">&#128269;</div>
          <h2>Research & Analysis</h2>
          <p>
            Private research across web and your documents.
            Synthesize findings with citations.
          </p>
        </button>

        <button
          class="persona-card"
          onclick={() => { selectedPersona = "assistant"; step = "persona-setup"; }}
        >
          <div class="persona-icon">&#128203;</div>
          <h2>Personal Assistant</h2>
          <p>
            Tasks, planning, and organization — managed by
            AI on your machine.
          </p>
        </button>

        <button
          class="persona-card"
          onclick={() => { selectedPersona = "developer"; step = "persona-setup"; }}
        >
          <div class="persona-icon">&#9881;</div>
          <h2>Developer</h2>
          <p>
            Show me the models, the config, and the trait
            boundaries.
          </p>
        </button>
      </div>
    </div>
  {:else if step === "persona-setup" && selectedPersona === "research"}
    <ResearchSetup
      onNext={handlePersonaNext}
      onBack={() => { step = "persona"; selectedPersona = null; }}
    />
  {:else if step === "persona-setup" && selectedPersona === "assistant"}
    <AssistantSetup
      onNext={handlePersonaNext}
      onBack={() => { step = "persona"; selectedPersona = null; }}
    />
  {:else if step === "persona-setup" && selectedPersona === "developer"}
    <DeveloperSetup
      onNext={handlePersonaNext}
      onBack={() => { step = "persona"; selectedPersona = null; }}
    />
  {:else if step === "knowledge" && selectedPersona}
    <KnowledgeBaseSetup
      persona={selectedPersona}
      onSelect={handleKnowledgeSelect}
      onSkip={handleKnowledgeSkip}
    />
  {:else if step === "websearch"}
    <WebSearchSetup
      onConfigure={handleWebConfigure}
      onSkip={handleWebSkip}
    />
  {:else if step === "finishing"}
    <div class="finishing">
      {#if error}
        <p class="error">{error}</p>
      {:else}
        <p>Setting up Sovereign...</p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .wizard {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    padding: 2rem;
    background: var(--bg-primary);
  }

  .persona-select {
    text-align: center;
    max-width: 800px;
  }

  h1 {
    font-size: 2rem;
    font-weight: 300;
    margin-bottom: 0.5rem;
  }

  .subtitle {
    color: var(--text-secondary);
    margin-bottom: 2rem;
    font-size: 1.1rem;
  }

  .persona-cards {
    display: flex;
    gap: 20px;
    justify-content: center;
  }

  .persona-card {
    width: 220px;
    padding: 24px 20px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    text-align: center;
    transition:
      border-color 0.2s,
      transform 0.2s;
  }

  .persona-card:hover {
    border-color: var(--accent);
    transform: translateY(-2px);
  }

  .persona-icon {
    font-size: 2rem;
    margin-bottom: 12px;
  }

  .persona-card h2 {
    font-size: 1rem;
    font-weight: 600;
    margin-bottom: 8px;
  }

  .persona-card p {
    font-size: 0.85rem;
    color: var(--text-secondary);
    line-height: 1.4;
  }

  .finishing {
    text-align: center;
    color: var(--text-secondary);
  }

  .error {
    color: var(--error);
  }
</style>
