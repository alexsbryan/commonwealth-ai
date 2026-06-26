<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  AskScopeBar — a plain-language statement of what a question will reach,
  sitting just above the chat input (elegance phase, Move 1).

  The corpus scope already lived in `CorpusFilterStrip` (a row of toggle
  chips), but it read as a *filter*, not as intent. This bar says it
  out loud — "Asking everything you know" / "Asking ‹Notebook›" / "Asking
  N notebooks" — and clicking it reveals the strip to change the scope.
  So a notebook's Ask reads "Asking ‹this notebook›" the instant it opens.

  Pure presentation: derives the summary from `enabledCorpora` (the same
  `null = everything` / subset model the strip persists) and resolves
  friendly names via `listCorpora()`. No new state.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { listCorpora } from "../api";
  import type { CorpusEntry } from "../types";

  let {
    enabledCorpora,
    expanded = false,
    onToggle,
  }: {
    /** The active allow-list: `null` = everything, a subset = those ids,
     *  `[]` = nothing (the guarded edge). */
    enabledCorpora: string[] | null;
    expanded?: boolean;
    onToggle: () => void;
  } = $props();

  let corpora = $state<CorpusEntry[]>([]);
  onMount(async () => {
    try {
      corpora = await listCorpora();
    } catch {
      corpora = [];
    }
  });

  const isPartition = (id: string): boolean =>
    /^.+-partition-(?:node-[0-9a-f]+|self)$/.test(id);
  let parents = $derived(
    corpora.filter(
      (c) => c.status === "installed" && !c.parent_corpus_id && !isPartition(c.id),
    ),
  );
  const nameOf = (id: string): string =>
    parents.find((c) => c.id === id)?.name ?? id;

  // What the next question will reach, in plain language.
  let label = $derived.by(() => {
    if (enabledCorpora == null) return "everything you know";
    if (enabledCorpora.length === 0) return "nothing — pick a notebook";
    if (enabledCorpora.length === 1) return nameOf(enabledCorpora[0]);
    // A subset that happens to be every installed notebook reads as "all".
    if (parents.length > 0 && enabledCorpora.length >= parents.length) {
      return "everything you know";
    }
    return `${enabledCorpora.length} notebooks`;
  });
</script>

<button
  type="button"
  class="ask-scope"
  class:open={expanded}
  onclick={onToggle}
  aria-expanded={expanded}
  data-testid="ask-scope-bar"
  title="Change what this question searches"
>
  <svg class="scope-icon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <circle cx="12" cy="12" r="9" />
    <circle cx="12" cy="12" r="4.5" />
    <circle cx="12" cy="12" r="0.5" fill="currentColor" />
  </svg>
  <span class="scope-text">Asking <strong data-testid="ask-scope-label">{label}</strong></span>
  <svg class="scope-chevron" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="m6 9 6 6 6-6" />
  </svg>
</button>

<style>
  .ask-scope {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    padding: 7px 24px;
    background: var(--bg-secondary);
    border: none;
    border-top: 1px solid var(--border-mid);
    color: var(--text-secondary);
    font: inherit;
    font-size: 0.78rem;
    text-align: left;
    cursor: pointer;
    transition: color 0.12s ease, background 0.12s ease;
  }
  .ask-scope:hover {
    color: var(--text-primary);
    background: color-mix(in oklch, var(--accent) 5%, var(--bg-secondary));
  }
  .scope-icon {
    color: var(--accent);
    flex-shrink: 0;
  }
  .scope-text {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .scope-text strong {
    color: var(--text-primary);
    font-weight: 600;
  }
  .scope-chevron {
    color: var(--text-muted);
    flex-shrink: 0;
    transition: transform 0.16s ease;
  }
  .ask-scope.open .scope-chevron {
    transform: rotate(180deg);
  }
  @media (prefers-reduced-motion: reduce) {
    .scope-chevron {
      transition: none;
    }
  }
</style>
