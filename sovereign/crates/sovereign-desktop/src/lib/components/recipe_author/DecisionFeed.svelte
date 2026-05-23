<script lang="ts">
  // Chronological feed of decisions and deferred questions. The
  // dashboard's primary "what happened" surface — every non-trivial
  // choice the agent or partner made shows up here, attributed.
  import Card from "./Card.svelte";
  import type { DashboardNoteEntry } from "../../types";

  let {
    decisions,
    deferredQuestions,
  }: {
    decisions: DashboardNoteEntry[];
    deferredQuestions: DashboardNoteEntry[];
  } = $props();

  type FeedEntry = DashboardNoteEntry & { kindLabel: string };

  // Merge + sort newest first. NoteRow.created_at is RFC 3339 — string
  // sort is correct because all entries share an offset.
  const merged: FeedEntry[] = $derived.by(() => {
    const out: FeedEntry[] = [
      ...decisions.map((d) => ({ ...d, kindLabel: d.decision_kind ?? "decision" })),
      ...deferredQuestions.map((q) => ({ ...q, kindLabel: "deferred-question" })),
    ];
    out.sort((a, b) => b.created_at.localeCompare(a.created_at));
    return out.slice(0, 30);
  });

  function relativeTime(iso: string): string {
    const ms = Date.parse(iso);
    if (Number.isNaN(ms)) return iso;
    const diff = Date.now() - ms;
    const mins = Math.floor(diff / 60_000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m`;
    const hrs = Math.floor(mins / 60);
    if (hrs < 24) return `${hrs}h`;
    return new Date(ms).toLocaleDateString();
  }
</script>

<Card title="Decisions" counter={decisions.length + deferredQuestions.length}>
  {#if merged.length === 0}
    <p class="muted">No decisions logged yet.</p>
  {:else}
    <ul>
      {#each merged as e (e.id)}
        <li>
          <div class="row-head">
            <span
              class="kind"
              class:deferred={e.kindLabel === "deferred-question"}
              >{e.kindLabel}</span
            >
            {#if e.attribution}
              <span class="attr">{e.attribution}</span>
            {/if}
            <span class="when">{relativeTime(e.created_at)}</span>
          </div>
          <p class="content">{e.content}</p>
        </li>
      {/each}
    </ul>
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
  }
  li {
    border-left: 2px solid color-mix(in srgb, var(--lavender) 50%, transparent);
    padding: 0.1rem 0 0.1rem 0.55rem;
  }
  .row-head {
    display: flex;
    gap: 0.4rem;
    align-items: baseline;
    font-size: 0.72rem;
  }
  .kind {
    text-transform: uppercase;
    color: var(--muted-bright, #b8bac1);
    font-weight: 600;
    letter-spacing: 0.04em;
  }
  .kind.deferred {
    color: var(--accent-light);
  }
  .attr {
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  .when {
    margin-left: auto;
    color: var(--muted, #8a8c93);
  }
  .content {
    margin: 0.2rem 0 0;
    font-size: 0.82rem;
    line-height: 1.35;
  }
</style>
