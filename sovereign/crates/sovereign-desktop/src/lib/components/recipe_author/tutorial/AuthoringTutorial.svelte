<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Center-pane replay player for the seeded authoring tutorial. Shows the
  // conversation transcript up to the current step, a teaching caption for the
  // current step, and Back / Next controls so it's fully self-paced and
  // re-runnable. The right rail (TutorialArtifacts) reveals the matching
  // dashboard artifacts in lockstep — currentStep lives in the workspace so
  // both panes stay in sync.
  import { tick } from "svelte";
  import { type TutorialStep, turnsThrough } from "./federalistTutorial";

  let {
    steps,
    currentStep,
    onNext,
    onBack,
    onExit,
    onFinish,
    onLaunchExplorer,
  }: {
    steps: TutorialStep[];
    currentStep: number;
    onNext: () => void;
    onBack: () => void;
    onExit: () => void;
    /// Last step's secondary action — exit the tutorial and start a real project.
    onFinish: () => void;
    /// Last step's PRIMARY action — install the real Federalist corpus and open
    /// the live explorer over it (the demo's running-thing-with-real-data finale).
    onLaunchExplorer: () => void;
  } = $props();

  let transcript = $derived(turnsThrough(steps, currentStep));
  let step = $derived(steps[currentStep]);
  let isFirst = $derived(currentStep === 0);
  let isLast = $derived(currentStep === steps.length - 1);

  let transcriptRef: HTMLDivElement | null = $state(null);

  // Keep the newest turn in view as the user steps forward.
  $effect(() => {
    void currentStep;
    void tick().then(() => {
      if (transcriptRef) transcriptRef.scrollTop = transcriptRef.scrollHeight;
    });
  });
</script>

<section class="tutorial" data-testid="authoring-tutorial">
  <header class="t-head">
    <div class="t-title">
      <span class="t-mark" aria-hidden="true">▷</span>
      <span>Guided example — authoring a recipe</span>
    </div>
    <div class="t-head-right">
      <span class="t-progress">Step {currentStep + 1} of {steps.length}</span>
      <button
        type="button"
        class="t-exit"
        onclick={onExit}
        title="Exit the walkthrough"
        aria-label="Exit the walkthrough"
      >✕</button>
    </div>
  </header>

  <div class="t-transcript" bind:this={transcriptRef}>
    {#each transcript as turn, i (i)}
      <div
        class="t-msg"
        class:user={turn.role === "user"}
        class:assistant={turn.role === "assistant"}
      >
        <div class="t-role">{turn.role === "user" ? "you" : "agent"}</div>
        <div class="t-content">{turn.content}</div>
      </div>
    {/each}
  </div>

  <div class="t-caption" role="note">
    <span class="t-caption-label">What's happening</span>
    <p>{step.caption}</p>
  </div>

  <footer class="t-controls">
    <button
      type="button"
      class="t-back"
      onclick={onBack}
      disabled={isFirst}
      data-testid="tutorial-back"
    >← Back</button>
    {#if isLast}
      <div class="t-finale-actions">
        <button
          type="button"
          class="t-back"
          onclick={onFinish}
          data-testid="tutorial-start-own"
        >Start your own</button>
        <button
          type="button"
          class="t-next primary"
          onclick={onLaunchExplorer}
          data-testid="tutorial-launch-explorer"
        >Open the live explorer →</button>
      </div>
    {:else}
      <button
        type="button"
        class="t-next primary"
        onclick={onNext}
        data-testid="tutorial-next"
      >Next →</button>
    {/if}
  </footer>
</section>

<style>
  .tutorial {
    display: flex;
    flex-direction: column;
    flex: 1 1 auto;
    min-height: 0;
    font-family: var(--font-sans);
    color: var(--text-primary);
  }
  .t-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
    font-size: 0.85rem;
  }
  .t-title {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-weight: 600;
  }
  .t-mark {
    color: var(--lavender, #b3a7e0);
  }
  .t-head-right {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .t-progress {
    color: var(--text-muted);
    font-size: 0.74rem;
    font-family: var(--font-mono);
    letter-spacing: 0.02em;
  }
  .t-exit {
    background: transparent;
    border: 1px solid var(--border-mid);
    color: var(--text-secondary);
    border-radius: var(--radius);
    padding: 2px 8px;
    cursor: pointer;
    font-size: 0.8rem;
  }
  .t-exit:hover {
    border-color: var(--accent);
    color: var(--accent-light);
  }
  .t-transcript {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .t-msg {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-width: 85%;
  }
  .t-msg.user {
    align-self: flex-end;
    align-items: flex-end;
  }
  .t-msg.assistant {
    align-self: flex-start;
  }
  .t-role {
    font-size: 0.66rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.1em;
    font-weight: 600;
  }
  .t-content {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 9px 12px;
    font-size: 0.9rem;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .t-msg.user .t-content {
    background: var(--lavender-glow);
    border-color: color-mix(in srgb, var(--lavender) 30%, transparent);
  }
  .t-caption {
    margin: 0 16px;
    padding: 12px 14px;
    border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
    background: var(--accent-glow, color-mix(in srgb, var(--accent) 8%, transparent));
    border-radius: var(--radius-lg, 8px);
    border-left-width: 3px;
  }
  .t-caption-label {
    display: block;
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    font-weight: 600;
    color: var(--accent-light, #dfc068);
    margin-bottom: 4px;
  }
  .t-caption p {
    margin: 0;
    font-size: 0.88rem;
    line-height: 1.55;
    color: var(--text-primary);
  }
  .t-controls {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 14px 16px;
    border-top: 1px solid var(--border);
    margin-top: 12px;
  }
  .t-back,
  .t-next {
    padding: 7px 16px;
    border-radius: var(--radius);
    font-family: inherit;
    font-size: 0.88rem;
    cursor: pointer;
    border: 1px solid var(--border-mid);
    background: transparent;
    color: var(--text-secondary);
  }
  .t-back:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .t-next.primary {
    background: var(--lavender-dim);
    border-color: color-mix(in srgb, var(--lavender) 50%, transparent);
    color: var(--text-primary);
    font-weight: 500;
  }
  .t-next.primary:hover {
    background: color-mix(in srgb, var(--lavender) 28%, transparent);
  }
  .t-back:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--text-primary);
  }
  .t-finale-actions {
    display: flex;
    gap: 0.6rem;
    align-items: center;
  }
</style>
