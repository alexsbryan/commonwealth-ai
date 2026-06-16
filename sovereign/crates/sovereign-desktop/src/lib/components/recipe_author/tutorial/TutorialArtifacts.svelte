<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Right-rail companion to AuthoringTutorial: reveals the recipe's artifacts
  // (charter → source → ontology → recipe TOML → build → atlas) as the
  // walkthrough advances, spotlighting the one introduced on the current step.
  // This is the "understand the system step by step" half — you watch each part
  // appear and connect, not just read a chat log.
  import { tick } from "svelte";
  import type { TutorialReveal, RevealKey } from "./federalistTutorial";

  let { reveal, highlight }: { reveal: TutorialReveal; highlight?: RevealKey } =
    $props();

  let scrollRef: HTMLDivElement | null = $state(null);

  // Bring the freshly-revealed (hot) card into view as the user steps forward.
  $effect(() => {
    void highlight;
    void tick().then(() => {
      const hot = scrollRef?.querySelector<HTMLElement>(".card.hot");
      hot?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    });
  });
</script>

<div class="artifacts" bind:this={scrollRef} data-testid="tutorial-artifacts">
  <p class="a-head">The recipe takes shape</p>

  {#if reveal.charter}
    <div class="card" class:hot={highlight === "charter"}>
      <span class="card-label">Charter</span>
      <p class="card-text">{reveal.charter}</p>
    </div>
  {/if}

  {#if reveal.source}
    <div class="card" class:hot={highlight === "source"}>
      <span class="card-label">Source &amp; reader</span>
      <p class="card-text">{reveal.source}</p>
    </div>
  {/if}

  {#if reveal.ontology}
    <div class="card" class:hot={highlight === "ontology"}>
      <span class="card-label">Ontology — what to extract</span>
      <pre class="card-pre">{reveal.ontology}</pre>
    </div>
  {/if}

  {#if reveal.recipeToml}
    <div class="card" class:hot={highlight === "recipeToml"}>
      <span class="card-label">Recipe (TOML you can edit)</span>
      <pre class="card-pre toml">{reveal.recipeToml}</pre>
    </div>
  {/if}

  {#if reveal.build}
    <div class="card" class:hot={highlight === "build"}>
      <span class="card-label">Build &amp; enrich</span>
      <p class="card-text">
        {reveal.build === "done"
          ? "✓ Pipeline complete — corpus indexed and enriched"
          : "Running the pipeline…"}
      </p>
    </div>
  {/if}

  {#if reveal.atoms}
    <div class="card" class:hot={highlight === "atoms"}>
      <span class="card-label">Atlas — extracted with your ontology</span>
      <div class="atoms">
        {#each reveal.atoms as a (a.label)}
          <span class="atom-chip"><b>{a.count}</b> {a.label}</span>
        {/each}
      </div>
    </div>
  {/if}

  {#if reveal.done}
    <div class="card done" class:hot={highlight === "done"}>
      <span class="card-label">Your turn</span>
      <p class="card-text">
        Author one for your own domain — the same charter → ontology → recipe →
        build arc.
      </p>
    </div>
  {/if}

  {#if !reveal.charter}
    <p class="a-empty">
      As the walkthrough advances, each part of the recipe appears here.
    </p>
  {/if}
</div>

<style>
  .artifacts {
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
    height: 100%;
    overflow-y: auto;
  }
  .a-head {
    margin: 0 0 0.2rem;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    font-weight: 600;
    color: var(--text-secondary, #8a8c93);
  }
  .a-empty {
    color: var(--text-muted, #8a8c93);
    font-size: 0.82rem;
    font-style: italic;
    line-height: 1.5;
    margin: 0.4rem 0;
  }
  .card {
    border: 1px solid var(--border, #2a2c33);
    border-radius: var(--radius, 6px);
    padding: 10px 12px;
    background: var(--bg-elevated, transparent);
    display: flex;
    flex-direction: column;
    gap: 5px;
    transition:
      border-color 200ms ease,
      box-shadow 200ms ease,
      background 200ms ease;
  }
  /* The artifact introduced on the current step — spotlight it. */
  .card.hot {
    border-color: var(--accent, #c4a46a);
    box-shadow: 0 0 0 1px var(--accent, #c4a46a),
      0 0 16px -6px color-mix(in srgb, var(--accent) 60%, transparent);
    background: var(--accent-glow, color-mix(in srgb, var(--accent) 7%, transparent));
  }
  .card-label {
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    font-weight: 600;
    color: var(--text-muted, #8a8c93);
  }
  .card.hot .card-label {
    color: var(--accent-light, #dfc068);
  }
  .card-text {
    margin: 0;
    font-size: 0.85rem;
    line-height: 1.5;
    color: var(--text-primary, #e6e6e8);
  }
  .card-pre {
    margin: 0;
    font-family: var(--font-mono, ui-monospace, Menlo, monospace);
    font-size: 0.74rem;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--text-secondary, #c8c8cc);
    max-height: 260px;
    overflow-y: auto;
  }
  .card-pre.toml {
    background: var(--bg, #15171c);
    border: 1px solid var(--border, #2a2c33);
    border-radius: 4px;
    padding: 8px 10px;
    color: var(--text-primary, #e6e6e8);
  }
  .atoms {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .atom-chip {
    font-size: 0.78rem;
    padding: 3px 9px;
    border-radius: 999px;
    background: var(--bg, #15171c);
    border: 1px solid var(--border-mid, #3a3c43);
    color: var(--text-secondary, #c8c8cc);
  }
  .atom-chip b {
    color: var(--text-primary, #e6e6e8);
  }
  .card.done {
    border-style: dashed;
  }
</style>
