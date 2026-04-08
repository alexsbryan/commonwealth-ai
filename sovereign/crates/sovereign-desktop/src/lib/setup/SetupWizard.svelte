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

  let stepNum = $derived(
    step === "persona" ? 1 :
    step === "persona-setup" ? 2 :
    step === "knowledge" ? 3 : 4
  );

  let totalSteps = $derived(selectedPersona === "developer" ? 3 : 4);

  function handlePersonaNext(config: SetupConfig) {
    partialConfig = config;
    step = "knowledge";
  }

  function handleKnowledgeSelect(tierId: string) {
    if (partialConfig) partialConfig.selected_tier = tierId;
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

  <!-- ── Persistent header ── -->
  <header class="wizard-header">
    <div class="wizard-brand">
      <span class="wizard-mark" aria-hidden="true">◈</span>
      <span class="wizard-name">SOVEREIGN</span>
    </div>

    {#if step !== "finishing"}
      <nav class="step-track" aria-label="Setup progress, step {stepNum} of {totalSteps}">
        {#each Array(totalSteps) as _, i}
          {#if i > 0}
            <div class="step-connector" class:done={stepNum > i}></div>
          {/if}
          <div
            class="step-dot"
            class:active={stepNum === i + 1}
            class:done={stepNum > i + 1}
          ></div>
        {/each}
      </nav>
      <span class="step-label" aria-hidden="true">{stepNum} / {totalSteps}</span>
    {/if}
  </header>

  <!-- ── Persona selection — two-column layout ── -->
  {#if step === "persona"}
    <div class="persona-step">

      <!-- Left ambient panel -->
      <aside class="persona-panel" aria-hidden="true">
        <div class="panel-bloom panel-bloom-a"></div>
        <div class="panel-bloom panel-bloom-b"></div>
        <div class="panel-content">
          <div class="panel-mark">◈</div>
          <p class="panel-tagline">Your AI.<br>Your data.<br>Your mesh.</p>
          <p class="panel-sub">Local intelligence,<br>shared freely.</p>
        </div>
        <div class="panel-ring-wrap">
          <div class="p-ring p-ring-1"></div>
          <div class="p-ring p-ring-2"></div>
          <div class="p-ring p-ring-3"></div>
        </div>
      </aside>

      <!-- Right: persona cards -->
      <div class="persona-right">
        <div class="persona-right-inner">
          <h1>Choose your path</h1>
          <p class="persona-subtitle">You can change this anytime in settings.</p>

          <div class="persona-cards">

            <button
              class="persona-card research"
              onclick={() => { selectedPersona = "research"; step = "persona-setup"; }}
            >
              <div class="card-stripe"></div>
              <div class="card-body">
                <div class="card-icon">
                  <svg width="20" height="20" viewBox="0 0 20 20" fill="none" aria-hidden="true">
                    <circle cx="8.5" cy="8.5" r="6" stroke="currentColor" stroke-width="1.5"/>
                    <line x1="13" y1="13" x2="17.5" y2="17.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
                    <line x1="8.5" y1="5.5" x2="8.5" y2="11.5" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" opacity="0.55"/>
                    <line x1="5.5" y1="8.5" x2="11.5" y2="8.5" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" opacity="0.55"/>
                  </svg>
                </div>
                <div class="card-text">
                  <h2>Research & Analysis</h2>
                  <p>Private research across web and local documents. Synthesize findings with full citations.</p>
                </div>
                <span class="card-arrow" aria-hidden="true">→</span>
              </div>
            </button>

            <button
              class="persona-card assistant"
              onclick={() => { selectedPersona = "assistant"; step = "persona-setup"; }}
            >
              <div class="card-stripe"></div>
              <div class="card-body">
                <div class="card-icon">
                  <svg width="20" height="20" viewBox="0 0 20 20" fill="none" aria-hidden="true">
                    <path d="M10 1 L18.5 5.75 L18.5 14.25 L10 19 L1.5 14.25 L1.5 5.75 Z" stroke="currentColor" stroke-width="1.4"/>
                    <circle cx="10" cy="10" r="2.8" stroke="currentColor" stroke-width="1.2"/>
                  </svg>
                </div>
                <div class="card-text">
                  <h2>Personal Assistant</h2>
                  <p>Tasks, planning, and organization — managed by AI on your machine, for your eyes only.</p>
                </div>
                <span class="card-arrow" aria-hidden="true">→</span>
              </div>
            </button>

            <button
              class="persona-card developer"
              onclick={() => { selectedPersona = "developer"; step = "persona-setup"; }}
            >
              <div class="card-stripe"></div>
              <div class="card-body">
                <div class="card-icon">
                  <svg width="20" height="20" viewBox="0 0 20 20" fill="none" aria-hidden="true">
                    <path d="M10 2 L10 10 M10 10 L4 17 M10 10 L16 17" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
                    <circle cx="10" cy="2" r="1.8" stroke="currentColor" stroke-width="1.2"/>
                    <circle cx="4" cy="17" r="1.8" stroke="currentColor" stroke-width="1.2"/>
                    <circle cx="16" cy="17" r="1.8" stroke="currentColor" stroke-width="1.2"/>
                  </svg>
                </div>
                <div class="card-text">
                  <h2>Developer</h2>
                  <p>Models, config, and trait boundaries. Full control over inference settings from day one.</p>
                </div>
                <span class="card-arrow" aria-hidden="true">→</span>
              </div>
            </button>

          </div>
        </div>
      </div>
    </div>

  <!-- ── Form steps ── -->
  {:else if step === "persona-setup" || step === "knowledge" || step === "websearch"}
    <div class="step-body">
      {#if step === "persona-setup" && selectedPersona === "research"}
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
      {/if}
    </div>

  <!-- ── Finishing ── -->
  {:else if step === "finishing"}
    <div class="finishing-screen">
      {#if error}
        <p class="finishing-error">{error}</p>
        <button class="finishing-back" onclick={() => { step = "knowledge"; }}>Go back</button>
      {:else}
        <div class="finishing-mark-wrap" aria-hidden="true">
          <div class="f-ring f-ring-1"></div>
          <div class="f-ring f-ring-2"></div>
          <div class="f-ring f-ring-3"></div>
          <div class="finishing-mark">◈</div>
        </div>
        <h2>Building your commons</h2>
        <p class="finishing-sub">Weaving knowledge, skills, and mesh connectivity&hellip;</p>
      {/if}
    </div>
  {/if}

</div>

<style>
  /* ── Wizard shell ── */
  .wizard {
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--bg-root);
    overflow: hidden;
  }

  /* ── Header ── */
  .wizard-header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 0 24px;
    height: 54px;
    flex-shrink: 0;
    border-bottom: 1px solid var(--border);
    background: var(--bg-secondary);
  }

  .wizard-brand {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }

  .wizard-mark {
    color: var(--accent);
    font-size: 1.05rem;
    filter: drop-shadow(0 0 5px rgba(201, 168, 76, 0.45));
  }

  .wizard-name {
    font-size: 0.68rem;
    font-weight: 700;
    letter-spacing: 0.22em;
    color: var(--text-secondary);
    text-transform: uppercase;
  }

  /* Step track */
  .step-track {
    display: flex;
    align-items: center;
    gap: 0;
    margin-left: auto;
  }

  .step-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--border-bright);
    flex-shrink: 0;
    transition: all 0.3s ease;
  }

  .step-dot.active {
    background: var(--accent);
    box-shadow: 0 0 8px rgba(201, 168, 76, 0.5);
    width: 20px;
    border-radius: 4px;
  }

  .step-dot.done {
    background: var(--growth);
  }

  .step-connector {
    width: 20px;
    height: 1px;
    background: var(--border-mid);
    flex-shrink: 0;
    transition: background 0.3s ease;
  }

  .step-connector.done {
    background: var(--growth);
    opacity: 0.5;
  }

  .step-label {
    font-size: 0.68rem;
    font-family: 'Syne Mono', monospace;
    color: var(--text-muted);
    letter-spacing: 0.08em;
    flex-shrink: 0;
    margin-left: 14px;
  }

  /* ── Persona step — two column ── */
  .persona-step {
    flex: 1;
    display: flex;
    overflow: hidden;
  }

  /* Left ambient panel */
  .persona-panel {
    width: 320px;
    flex-shrink: 0;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border);
    position: relative;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
  }

  .panel-bloom {
    position: absolute;
    border-radius: 50%;
    pointer-events: none;
  }

  .panel-bloom-a {
    width: 260px;
    height: 260px;
    top: -60px;
    left: -60px;
    background: radial-gradient(circle, rgba(121, 196, 120, 0.07) 0%, transparent 70%);
  }

  .panel-bloom-b {
    width: 220px;
    height: 220px;
    bottom: 20px;
    right: -40px;
    background: radial-gradient(circle, rgba(201, 168, 76, 0.07) 0%, transparent 70%);
  }

  .panel-content {
    position: relative;
    z-index: 1;
    text-align: center;
    padding: 0 28px;
  }

  .panel-mark {
    font-size: 3.2rem;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 18px rgba(201, 168, 76, 0.5));
    animation: wiz-breathe 2.8s ease-in-out infinite;
    margin-bottom: 22px;
    display: block;
  }

  .panel-tagline {
    font-size: 1.55rem;
    font-weight: 700;
    color: var(--text-primary);
    line-height: 1.4;
    margin-bottom: 14px;
    letter-spacing: -0.01em;
  }

  .panel-sub {
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.6;
    letter-spacing: 0.04em;
  }

  /* Panel expanding rings */
  .panel-ring-wrap {
    position: absolute;
    bottom: 0;
    left: 50%;
    transform: translateX(-50%);
    width: 1px;
    height: 1px;
  }

  .p-ring {
    position: absolute;
    border-radius: 50%;
    border: 1px solid rgba(201, 168, 76, 0.18);
    width: 100px;
    height: 100px;
    top: -50px;
    left: -50px;
    animation: wiz-ring-expand 5s ease-out infinite;
  }

  .p-ring-2 { animation-delay: 1.67s; }
  .p-ring-3 { animation-delay: 3.33s; }

  @keyframes wiz-ring-expand {
    0%   { transform: scale(0.6); opacity: 0.5; }
    100% { transform: scale(4);   opacity: 0; }
  }

  @keyframes wiz-breathe {
    0%, 100% { filter: drop-shadow(0 0 12px rgba(201, 168, 76, 0.4)); }
    50%       { filter: drop-shadow(0 0 28px rgba(201, 168, 76, 0.65)); }
  }

  /* Right panel */
  .persona-right {
    flex: 1;
    overflow-y: auto;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 40px 48px;
  }

  .persona-right-inner {
    width: 100%;
    max-width: 480px;
  }

  .persona-right-inner h1 {
    font-size: 1.6rem;
    font-weight: 700;
    color: var(--text-primary);
    margin-bottom: 6px;
    letter-spacing: -0.02em;
  }

  .persona-subtitle {
    font-size: 0.82rem;
    color: var(--text-muted);
    margin-bottom: 28px;
    letter-spacing: 0.02em;
  }

  /* Persona cards */
  .persona-cards {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .persona-card {
    display: flex;
    align-items: stretch;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    overflow: hidden;
    transition:
      border-color 0.2s,
      background 0.2s,
      transform 0.2s,
      box-shadow 0.2s;
    text-align: left;
    width: 100%;
    cursor: pointer;
  }

  .persona-card:hover {
    background: var(--bg-elevated);
    transform: translateX(4px);
  }

  /* Color-coded left stripe per persona */
  .card-stripe {
    width: 4px;
    flex-shrink: 0;
  }

  .research .card-stripe { background: var(--sky); }
  .assistant .card-stripe { background: var(--growth); }
  .developer .card-stripe { background: var(--coral); }

  .research:hover  { border-color: var(--sky);    box-shadow: 0 2px 20px rgba(74, 186, 216, 0.1); }
  .assistant:hover { border-color: var(--growth);  box-shadow: 0 2px 20px rgba(121, 196, 120, 0.1); }
  .developer:hover { border-color: var(--coral);   box-shadow: 0 2px 20px rgba(224, 112, 72, 0.1); }

  .card-body {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 18px 18px;
    flex: 1;
  }

  .card-icon {
    flex-shrink: 0;
    width: 38px;
    height: 38px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--radius);
    background: var(--bg-input);
    transition: background 0.2s;
  }

  .research  .card-icon { color: var(--sky); }
  .assistant .card-icon { color: var(--growth); }
  .developer .card-icon { color: var(--coral); }

  .persona-card:hover .card-icon {
    background: var(--bg-secondary);
  }

  .card-text {
    flex: 1;
    min-width: 0;
  }

  .card-text h2 {
    font-size: 0.92rem;
    font-weight: 600;
    color: var(--text-primary);
    margin-bottom: 4px;
    letter-spacing: 0.01em;
  }

  .card-text p {
    font-size: 0.78rem;
    color: var(--text-secondary);
    line-height: 1.45;
    margin: 0;
  }

  .card-arrow {
    flex-shrink: 0;
    font-size: 1rem;
    color: var(--text-muted);
    transition: color 0.2s, transform 0.2s;
    line-height: 1;
  }

  .persona-card:hover .card-arrow { transform: translateX(4px); }
  .research:hover  .card-arrow { color: var(--sky); }
  .assistant:hover .card-arrow { color: var(--growth); }
  .developer:hover .card-arrow { color: var(--coral); }

  /* ── Form steps ── */
  .step-body {
    flex: 1;
    overflow-y: auto;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 40px 24px 32px;
  }

  /* ── Finishing screen ── */
  .finishing-screen {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0;
    text-align: center;
    padding: 2rem;
  }

  .finishing-mark-wrap {
    position: relative;
    width: 90px;
    height: 90px;
    display: flex;
    align-items: center;
    justify-content: center;
    margin-bottom: 24px;
  }

  .f-ring {
    position: absolute;
    border-radius: 50%;
    border: 1px solid rgba(201, 168, 76, 0.3);
    width: 48px;
    height: 48px;
    animation: wiz-ring-expand 3s ease-out infinite;
  }

  .f-ring-2 { animation-delay: 1s; }
  .f-ring-3 { animation-delay: 2s; }

  .finishing-mark {
    font-size: 2.6rem;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 16px rgba(201, 168, 76, 0.55));
    animation: wiz-breathe 2.8s ease-in-out infinite;
    position: relative;
    z-index: 1;
  }

  .finishing-screen h2 {
    font-size: 1.3rem;
    font-weight: 400;
    color: var(--text-secondary);
    margin-bottom: 10px;
    letter-spacing: 0.04em;
  }

  .finishing-sub {
    font-size: 0.78rem;
    color: var(--text-muted);
    letter-spacing: 0.06em;
  }

  .finishing-error {
    color: var(--error);
    font-size: 0.9rem;
    margin-bottom: 16px;
    max-width: 420px;
    text-align: center;
    line-height: 1.5;
  }

  .finishing-back {
    padding: 8px 20px;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font-size: 0.85rem;
    transition: border-color 0.2s, color 0.2s;
  }

  .finishing-back:hover {
    border-color: var(--accent);
    color: var(--accent);
  }
</style>
