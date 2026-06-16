<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Center-pane welcome shown when no project is selected — the
  // first-timer's first screen. Replaces the bare "Pick a project…"
  // line with a short explanation of what Recipe Author does and the
  // three-step arc it actually follows (charter → BuildEnrichCard →
  // land-in-use handoff). Pure presentation; the CTA reuses the same
  // new-project trigger the sidebar button fires.
  import { installStarterCorpus } from "../../api";

  let {
    hasProjects,
    onNewProject,
    onOpenChat,
  }: {
    hasProjects: boolean;
    onNewProject: () => void;
    // Navigate to chat (host-provided). After the sample corpus installs we
    // call this; ChatView's empty state then mines its starter questions.
    onOpenChat?: () => void;
  } = $props();

  // "Try a sample corpus" — restore the bundled Federalist starter (offline,
  // ~1s) so a first-timer can chat with a real grounded corpus before
  // authoring their own. Idempotent backend; we just land in chat after.
  let installing = $state(false);
  let installError = $state<string | null>(null);
  async function tryStarter() {
    if (installing) return;
    installing = true;
    installError = null;
    try {
      await installStarterCorpus();
      onOpenChat?.();
    } catch (e) {
      installError = typeof e === "string" ? e : String(e);
    } finally {
      installing = false;
    }
  }

  // Mirrors the real flow the dashboard cards drive, in order.
  const STEPS: { n: string; title: string; body: string }[] = [
    {
      n: "1",
      title: "Describe your domain",
      body: "Write a short charter — what the corpus is, who it's for, and any boundaries you've already settled. The agent reads it on every turn.",
    },
    {
      n: "2",
      title: "Build & enrich",
      body: "The agent drafts the recipe — where documents come from, how they're parsed, and the ontology used to extract entities, claims, and questions. You run it.",
    },
    {
      n: "3",
      title: "Use it in chat",
      body: "Once built, jump straight into a conversation grounded in your sources — with starter questions mined from the corpus itself.",
    },
  ];
</script>

<div class="welcome" data-testid="recipe-author-welcome">
  <div class="inner">
    <span class="mark" aria-hidden="true">◇</span>
    <h1>Author a knowledge corpus</h1>
    <p class="lede">
      Turn a body of documents into a corpus you can ask questions of —
      grounded in your own sources, enriched with an ontology you shape.
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

    <div class="cta-row">
      <button
        type="button"
        class="cta"
        onclick={onNewProject}
        data-testid="recipe-author-welcome-cta"
      >
        + New project
      </button>
      {#if onOpenChat}
        <button
          type="button"
          class="cta-secondary"
          onclick={tryStarter}
          disabled={installing}
          data-testid="recipe-author-welcome-starter"
        >
          {installing ? "Setting up…" : "Try a sample corpus →"}
        </button>
      {/if}
    </div>
    {#if installError}
      <p class="install-error" role="alert">{installError}</p>
    {/if}
    {#if hasProjects}
      <p class="aside">…or pick a project from the list on the left.</p>
    {:else}
      <p class="aside">
        The sample is <em>The Federalist Papers</em> — ask it a question, then
        build a corpus from your own files.
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
  .cta-secondary:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--lavender) 45%, transparent);
    color: inherit;
  }
  .cta-secondary:disabled {
    opacity: 0.6;
    cursor: progress;
  }
  .install-error {
    margin: 0.5rem 0 0;
    color: var(--coral, #e2706e);
    font-size: 0.82rem;
  }
  .aside {
    margin: 0.5rem 0 0;
    color: var(--muted, #8a8c93);
    font-size: 0.82rem;
  }
</style>
