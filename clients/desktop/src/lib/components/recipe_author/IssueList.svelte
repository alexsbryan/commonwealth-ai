<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Outstanding issues for the project — `kind=recipe_issue` notes
  // grouped by category. The agent emits these from `recipe_test`
  // failures; the dashboard shows totals + an expandable list.
  import Card from "./Card.svelte";
  import type { DashboardNoteEntry } from "../../types";
  import { recipeAuthorChat } from "../../stores/recipeAuthorChat";

  let { issues }: { issues: DashboardNoteEntry[] } = $props();

  // Conversational recovery: hand the outstanding test issues to the live agent
  // (prompted to ACT on "fix it"), which rewrites the recipe and re-tests.
  function askFix() {
    const lines = issues
      .slice(0, 20)
      .map((i) => `- ${i.content}`)
      .join("\n");
    recipeAuthorChat.requestTurn(
      `Fix these issues from the last recipe test, then re-test:\n\n${lines}`,
    );
  }

  type Group = { category: string; count: number; samples: string[] };

  const groups: Group[] = $derived.by(() => {
    const byCat = new Map<string, Group>();
    for (const i of issues) {
      const cat =
        ((i.payload as Record<string, unknown> | undefined)?.category as
          | string
          | undefined) ?? "uncategorized";
      const g = byCat.get(cat) ?? { category: cat, count: 0, samples: [] };
      g.count += 1;
      if (g.samples.length < 3) g.samples.push(i.content);
      byCat.set(cat, g);
    }
    return [...byCat.values()].sort((a, b) => b.count - a.count);
  });
</script>

<Card title="Issues" counter={issues.length}>
  {#if issues.length === 0}
    <p class="muted">No outstanding issues.</p>
  {:else}
    <ul>
      {#each groups as g (g.category)}
        <li>
          <div class="row-head">
            <span class="cat">{g.category}</span>
            <span class="cnt">{g.count}</span>
          </div>
          <ul class="samples">
            {#each g.samples as s}
              <li>{s}</li>
            {/each}
            {#if g.count > g.samples.length}
              <li class="more">+ {g.count - g.samples.length} more…</li>
            {/if}
          </ul>
        </li>
      {/each}
    </ul>
    <button
      type="button"
      class="fix"
      onclick={askFix}
      data-testid="recipe-issues-ask-fix"
    >
      Ask agent to fix
    </button>
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .row-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 0.82rem;
  }
  .cat {
    text-transform: uppercase;
    font-size: 0.72rem;
    letter-spacing: 0.05em;
    color: var(--muted-bright, #b8bac1);
  }
  .cnt {
    font-size: 0.72rem;
    background: var(--coral-dim);
    color: var(--coral);
    padding: 1px 8px;
    border-radius: 999px;
  }
  .samples {
    margin: 0.3rem 0 0 0.5rem;
    padding: 0;
    font-size: 0.78rem;
    color: var(--muted-bright, #b8bac1);
  }
  .samples li {
    padding: 2px 0;
    border-left: 2px solid var(--bg-elevated);
    padding-left: 0.5rem;
  }
  .samples li.more {
    color: var(--muted, #8a8c93);
    font-style: italic;
    border-left: none;
  }
  .fix {
    margin-top: 0.6rem;
    font-size: 0.74rem;
    padding: 3px 10px;
    border-radius: 4px;
    cursor: pointer;
    border: 1px solid var(--border, #2a2c33);
    background: var(--bg-elevated);
    color: var(--fg, #e6e6e8);
  }
  .fix:hover {
    border-color: var(--growth, #4caf82);
  }
</style>
