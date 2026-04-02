<script lang="ts">
  import ResearchSetup from "./ResearchSetup.svelte";
  import AssistantSetup from "./AssistantSetup.svelte";
  import DeveloperSetup from "./DeveloperSetup.svelte";

  interface Props {
    onComplete: () => void;
  }

  let { onComplete }: Props = $props();

  type Persona = null | "research" | "assistant" | "developer";
  let selectedPersona: Persona = $state(null);
</script>

<div class="wizard">
  {#if selectedPersona === null}
    <div class="persona-select">
      <h1>Welcome to Sovereign</h1>
      <p class="subtitle">How will you use Sovereign?</p>

      <div class="persona-cards">
        <button
          class="persona-card"
          onclick={() => (selectedPersona = "research")}
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
          onclick={() => (selectedPersona = "assistant")}
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
          onclick={() => (selectedPersona = "developer")}
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
  {:else if selectedPersona === "research"}
    <ResearchSetup
      {onComplete}
      onBack={() => (selectedPersona = null)}
    />
  {:else if selectedPersona === "assistant"}
    <AssistantSetup
      {onComplete}
      onBack={() => (selectedPersona = null)}
    />
  {:else if selectedPersona === "developer"}
    <DeveloperSetup
      {onComplete}
      onBack={() => (selectedPersona = null)}
    />
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
</style>
