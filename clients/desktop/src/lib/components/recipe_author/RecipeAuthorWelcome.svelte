<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Center-pane welcome shown when no project is selected. The Recipe Author
  // teaches the *authoring* skill, so the headline action is a guided
  // walkthrough of authoring a real recipe — primary for a first-timer (no
  // projects yet), demoted to secondary once they have one and "+ New project"
  // leads. Pure presentation; the host owns the actions.
  let {
    hasProjects,
    onNewProject,
    onStartTutorial,
  }: {
    hasProjects: boolean;
    onNewProject: () => void;
    onStartTutorial: () => void;
  } = $props();

  // The authoring arc the tutorial then demonstrates, in order.
  const STEPS: { n: string; title: string; body: string }[] = [
    {
      n: "1",
      title: "Describe your domain",
      body: "Write a short charter — what you're building and who it's for. The agent reasons from it on every turn.",
    },
    {
      n: "2",
      title: "The agent drafts the recipe",
      body: "It interviews you for an ontology — your domain's entities, relationships, and questions — and writes the recipe as plain, editable TOML.",
    },
    {
      n: "3",
      title: "Build it into a knowledge graph",
      body: "Run the recipe: your documents become an atlas of exactly the things your ontology named — the skill you can now apply to any domain.",
    },
  ];
</script>

<div class="welcome" data-testid="recipe-author-welcome">
  <div class="inner">
    <span class="mark" aria-hidden="true">◇</span>
    <h1>Author a knowledge recipe</h1>
    <p class="lede">
      A recipe turns your documents into a knowledge graph — and you shape what
      it extracts by teaching the agent your domain. This tool is about learning
      that authoring skill, so you can do it for anything.
    </p>

    <ol class="steps">
      {#each STEPS as s (s.n)}
        <li>
          <span class="step-n" aria-hidden="true">{s.n}</span>
          <div class="step-text">
            <span class="step-title">{s.title}</span>
            <span class="step-body">{s.body}</span>
          </div>
        </li>
      {/each}
    </ol>

    <!-- Tutorial leads for a first-timer; "+ New project" leads once they
         have a project of their own. -->
    <div class="cta-row">
      {#if hasProjects}
        <button
          type="button"
          class="cta"
          onclick={onNewProject}
          data-testid="recipe-author-welcome-cta"
        >+ New project</button>
        <button
          type="button"
          class="cta-secondary"
          onclick={onStartTutorial}
          data-testid="recipe-author-welcome-tutorial"
        >Walk through a guided example →</button>
      {:else}
        <button
          type="button"
          class="cta"
          onclick={onStartTutorial}
          data-testid="recipe-author-welcome-tutorial"
        >Walk through an example →</button>
        <button
          type="button"
          class="cta-secondary"
          onclick={onNewProject}
          data-testid="recipe-author-welcome-cta"
        >+ New project</button>
      {/if}
    </div>

    {#if hasProjects}
      <p class="aside">…or pick a project from the list on the left.</p>
    {:else}
      <p class="aside">
        New here? The guided example walks through authoring a real recipe
        (The Federalist Papers), step by step — then you make your own.
      </p>
    {/if}
  </div>
</div>

<style>
  .welcome {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    padding: 2rem;
    overflow-y: auto;
  }
  .inner {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.5rem;
    max-width: 520px;
  }
  .mark {
    font-size: 1.6rem;
    opacity: 0.7;
    color: var(--lavender, #b3a7e0);
  }
  h1 {
    margin: 0.2rem 0 0;
    font-size: 1.35rem;
    font-weight: 600;
    letter-spacing: 0.01em;
  }
  .lede {
    margin: 0 0 0.6rem;
    color: var(--muted, #8a8c93);
    font-size: 0.92rem;
    line-height: 1.55;
  }
  .steps {
    list-style: none;
    margin: 0.4rem 0 0.9rem;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
    width: 100%;
  }
  .steps li {
    display: flex;
    align-items: flex-start;
    gap: 0.7rem;
  }
  .step-n {
    flex: 0 0 auto;
    width: 1.5rem;
    height: 1.5rem;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 999px;
    border: 1px solid color-mix(in srgb, var(--lavender) 45%, transparent);
    color: var(--lavender, #b3a7e0);
    font-size: 0.78rem;
    font-weight: 600;
    margin-top: 0.1rem;
  }
  .step-text {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  .step-title {
    font-size: 0.9rem;
    font-weight: 600;
  }
  .step-body {
    font-size: 0.84rem;
    color: var(--muted, #8a8c93);
    line-height: 1.5;
  }
  .cta-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6rem;
    margin-top: 0.3rem;
  }
  .cta {
    padding: 0.55rem 1.1rem;
    background: var(--lavender-dim);
    border: 1px solid color-mix(in srgb, var(--lavender) 50%, transparent);
    color: inherit;
    border-radius: 5px;
    cursor: pointer;
    font-size: 0.92rem;
    font-weight: 500;
  }
  .cta:hover {
    background: color-mix(in srgb, var(--lavender) 30%, transparent);
  }
  .cta-secondary {
    padding: 0.55rem 1.1rem;
    background: transparent;
    border: 1px solid var(--border, #2a2c33);
    color: var(--text-secondary, inherit);
    border-radius: 5px;
    cursor: pointer;
    font-size: 0.92rem;
  }
  .cta-secondary:hover {
    border-color: color-mix(in srgb, var(--lavender) 45%, transparent);
    color: inherit;
  }
  .aside {
    margin: 0.5rem 0 0;
    color: var(--muted, #8a8c93);
    font-size: 0.82rem;
    line-height: 1.5;
  }
</style>
