<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Glassbox card: did the on-disk recipe.toml parse? If not, why?
  //
  // The engine's `Recipe::from_toml` runs every dashboard poll. On
  // failure, the error text comes from `translate_parse_error`,
  // which has already rewritten generic toml-parse messages into
  // partner-readable guidance ("Recipe is missing the [acquire]
  // section. Add it with type = ..."). We render those verbatim.
  //
  // Three visual states:
  // - ok=true                   → green "valid" pill
  // - ok=false + no_recipe=true → muted "no recipe drafted yet"
  // - ok=false + errors.length  → red "needs attention" + each error
  //                                in its own block, copy-friendly
  import Card from "./Card.svelte";
  import type { RecipeValidationReport } from "../../types";
  import { recipeAuthorChat } from "../../stores/recipeAuthorChat";

  let { validation }: { validation: RecipeValidationReport } = $props();

  let copiedIdx: number | null = $state(null);

  // Conversational recovery: hand the parse errors to the live agent, which is
  // prompted to ACT on "fix it" (rewrite the recipe), or to explain them.
  function askFix() {
    recipeAuthorChat.requestTurn(
      `The recipe has validation errors. Fix them in the recipe and re-validate:\n\n${validation.errors.join("\n\n")}`,
    );
  }
  function askWhy() {
    recipeAuthorChat.requestTurn(
      `Explain these recipe validation errors in plain language and what change fixes each — don't edit yet:\n\n${validation.errors.join("\n\n")}`,
    );
  }

  async function copy(text: string, idx: number) {
    try {
      await navigator.clipboard.writeText(text);
      copiedIdx = idx;
      setTimeout(() => {
        if (copiedIdx === idx) copiedIdx = null;
      }, 1200);
    } catch {
      // Clipboard may be unavailable in some browsers / sandboxes;
      // silently degrade rather than throw across the workspace.
    }
  }
</script>

<Card title="Recipe validation">
  {#if validation.no_recipe}
    <p class="muted">No recipe drafted yet.</p>
  {:else if validation.ok}
    <div class="row">
      <span class="pill ok">valid</span>
      <span class="muted">Engine parsed the recipe.toml without errors.</span>
    </div>
    <div class="row">
      {#if validation.enrichment_ready}
        <span class="pill ok">enrichment ready</span>
        <span class="muted">Build will produce a knowledge graph (atoms).</span>
      {:else}
        <span class="pill warn" data-testid="enrichment-not-ready">no enrichment</span>
        <span class="muted">
          This recipe builds with <strong>zero atoms</strong> — turn on atlas
          enrichment to get a knowledge graph.
        </span>
      {/if}
    </div>
  {:else}
    <div class="row">
      <span class="pill fail">needs attention</span>
      <span class="muted">
        {validation.errors.length === 1
          ? "1 issue blocking the recipe"
          : `${validation.errors.length} issues blocking the recipe`}
      </span>
    </div>
    <ul class="errors">
      {#each validation.errors as err, i}
        <li>
          <pre class="err-text">{err}</pre>
          <button
            type="button"
            class="copy"
            onclick={() => copy(err, i)}
            data-testid="recipe-validation-copy"
          >
            {copiedIdx === i ? "copied" : "copy"}
          </button>
        </li>
      {/each}
    </ul>
    <div class="fix-actions">
      <button
        type="button"
        class="fix"
        onclick={askFix}
        data-testid="recipe-validation-ask-fix"
      >
        Ask agent to fix
      </button>
      <button type="button" class="why" onclick={askWhy}>Explain</button>
    </div>
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  .row {
    display: flex;
    gap: 0.6rem;
    align-items: baseline;
    font-size: 0.82rem;
  }
  .pill {
    text-transform: uppercase;
    font-size: 0.7rem;
    padding: 1px 8px;
    border-radius: 999px;
    font-weight: 600;
    letter-spacing: 0.04em;
  }
  .pill.ok {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .pill.fail {
    background: var(--coral-dim);
    color: var(--coral);
  }
  .pill.warn {
    background: var(--amber-flash);
    color: var(--amber);
  }
  .errors {
    list-style: none;
    margin: 0.5rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .errors li {
    position: relative;
    background: color-mix(in srgb, var(--coral) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--coral) 35%, transparent);
    border-radius: 4px;
    padding: 0.5rem 0.6rem;
  }
  .err-text {
    margin: 0;
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.78rem;
    line-height: 1.4;
    color: var(--fg, #e6e6e8);
    white-space: pre-wrap;
    word-break: break-word;
    padding-right: 3rem;
  }
  .copy {
    position: absolute;
    top: 6px;
    right: 6px;
    background: transparent;
    border: 1px solid var(--border, #2a2c33);
    color: var(--muted, #8a8c93);
    font-size: 0.68rem;
    padding: 2px 8px;
    border-radius: 4px;
    cursor: pointer;
  }
  .copy:hover {
    background: var(--bg-elevated);
    color: var(--fg, #e6e6e8);
  }
  .fix-actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.6rem;
  }
  .fix,
  .why {
    font-size: 0.74rem;
    padding: 3px 10px;
    border-radius: 4px;
    cursor: pointer;
    border: 1px solid var(--border, #2a2c33);
  }
  .fix {
    background: var(--bg-elevated);
    color: var(--fg, #e6e6e8);
  }
  .fix:hover {
    border-color: var(--growth, #4caf82);
  }
  .why {
    background: transparent;
    color: var(--muted, #8a8c93);
  }
  .why:hover {
    color: var(--fg, #e6e6e8);
  }
</style>
