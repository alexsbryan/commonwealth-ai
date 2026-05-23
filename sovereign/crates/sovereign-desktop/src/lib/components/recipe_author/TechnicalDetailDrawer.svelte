<script lang="ts">
  // Read-only drawer that shows the live recipe.toml. Collapsed by
  // default — the partner doesn't read TOML in the common path; this
  // is for "let me see what the agent actually wrote."
  import Card from "./Card.svelte";

  let { recipeToml }: { recipeToml: string | null } = $props();
  let expanded = $state(false);
</script>

<Card title="Recipe TOML">
  {#if !recipeToml}
    <p class="muted">No recipe drafted yet.</p>
  {:else}
    <button
      type="button"
      class="toggle"
      onclick={() => (expanded = !expanded)}
      data-testid="recipe-author-toml-toggle"
    >
      {expanded ? "Hide TOML" : `Show TOML (${recipeToml.split("\n").length} lines)`}
    </button>
    {#if expanded}
      <pre class="toml">{recipeToml}</pre>
    {/if}
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  .toggle {
    background: transparent;
    border: none;
    color: var(--lavender-light);
    cursor: pointer;
    padding: 0;
    font-size: 0.78rem;
    text-decoration: underline;
  }
  pre.toml {
    margin: 0.5rem 0 0;
    background: rgba(0, 0, 0, 0.3);
    padding: 0.6rem 0.7rem;
    border-radius: 4px;
    overflow-x: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.74rem;
    line-height: 1.4;
    color: var(--fg, #e6e6e8);
    max-height: 360px;
    overflow-y: auto;
  }
</style>
