<script lang="ts">
  import type { QuestionData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: QuestionData;
  }

  let { data }: Props = $props();

  function resolutionLabel(kind: string): string {
    switch (kind) {
      case "resolved": return "Resolved";
      case "contested": return "Contested";
      case "open": return "Open";
      case "dissolved": return "Dissolved";
      default: return kind;
    }
  }
</script>

<div class="body">
  <p class="content">{data.content}</p>

  <dl class="fields">
    <dt>Question type</dt>
    <dd class="kind">{data.question_type}</dd>

    <dt>Resolution</dt>
    <dd class="resolution" data-kind={data.resolution_status.kind}>
      <span class="resolution-label">
        {resolutionLabel(data.resolution_status.kind)}
      </span>
      {#if data.resolution_status.claim_id}
        <span class="arrow">→</span>
        <AtomLink atomId={data.resolution_status.claim_id} />
      {:else if data.resolution_status.claim_ids && data.resolution_status.claim_ids.length > 0}
        <span class="arrow">→</span>
        <ul class="atom-link-list">
          {#each data.resolution_status.claim_ids as id (id)}
            <li><AtomLink atomId={id} /></li>
          {/each}
        </ul>
      {/if}
    </dd>

    {#if (data.addressed_by?.length ?? 0) > 0}
      <dt>Addressed by</dt>
      <dd>
        <ul class="atom-link-list">
          {#each data.addressed_by ?? [] as id (id)}
            <li><AtomLink atomId={id} /></li>
          {/each}
        </ul>
      </dd>
    {/if}
  </dl>
</div>

<style>
  .body { display: flex; flex-direction: column; gap: 16px; }
  .content { margin: 0; line-height: 1.55; font-size: 1rem; }
  .fields {
    display: grid;
    grid-template-columns: 130px 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 0.85rem;
  }
  .fields dt { color: var(--text-muted); font-size: 0.78rem; letter-spacing: 0.02em; }
  .fields dd { margin: 0; }
  .kind { text-transform: capitalize; }
  .resolution {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .resolution .resolution-label { font-weight: 500; }
  .resolution[data-kind="contested"] .resolution-label { color: var(--warning, #c93); }
  .resolution[data-kind="open"] .resolution-label { color: var(--text-muted); font-style: italic; }
  .arrow { color: var(--text-muted); }
  .atom-link-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
  }
</style>
