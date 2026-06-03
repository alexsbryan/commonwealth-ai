<script lang="ts">
  // Mobile's own thin orchestration over the SHARED leaf components.
  // (Desktop's AssistantMessage adds clip/insight/reading-surface
  // behaviour that doesn't apply on mobile; the shared package only
  // ships the prop-driven leaves + the content parser.)
  import {
    parseAssistantContent,
    RoutingMeta,
    SourceAttribution,
    ThinkBlock,
  } from "@sovereign/chat-ui";
  import { renderMarkdown } from "../utils/markdown";
  import { resolveCitation } from "../api";
  import { corporaStore } from "../stores/corpora.svelte";

  let { content, metadata }: { content: string; metadata?: Record<string, unknown> } = $props();

  const blocks = $derived(parseAssistantContent(content));
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const provenance = $derived((metadata?.provenance ?? null) as any);
  const retrievedChunks = $derived(
    (metadata?.retrieved_chunks ?? []) as Array<{
      title?: string;
      corpus_id: string;
      chunk_id: string;
    }>,
  );

  // Citation tap → resolve the snippet from cache (the (corpus_id,
  // chunk_id) handle that proves the answer used an installed corpus).
  let openSnippet = $state<string | null>(null);
  async function onCitation(corpusId: string, chunkId: string) {
    openSnippet = (await resolveCitation(corpusId, chunkId)) ?? "(snippet unavailable)";
  }
</script>

<div class="assistant">
  {#each blocks as block}
    {#if block.type === "think"}
      <ThinkBlock content={block.content} />
    {:else}
      <div class="prose">{@html renderMarkdown(block.content)}</div>
    {/if}
  {/each}

  <SourceAttribution {content} {retrievedChunks} />

  {#if retrievedChunks.length}
    <div class="cites">
      {#each retrievedChunks as c (c.chunk_id)}
        <button class="cite" onclick={() => onCitation(c.corpus_id, c.chunk_id)}>
          {#if corporaStore.isPrivate(c.corpus_id)}<span
              class="lock"
              title="Private to this host — never shared with mesh peers">🔒</span
            >{/if}
          {c.title ?? c.corpus_id}
        </button>
      {/each}
    </div>
  {/if}

  {#if provenance}
    <RoutingMeta {provenance} {retrievedChunks} />
  {/if}

  {#if openSnippet}
    <button class="sheet-scrim" onclick={() => (openSnippet = null)} aria-label="Close">
      <div class="sheet"><p>{openSnippet}</p></div>
    </button>
  {/if}
</div>

<style>
  .assistant {
    align-self: flex-start;
    max-width: 92%;
  }
  .prose {
    line-height: 1.5;
  }
  .cites {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
    margin-top: 0.4rem;
  }
  .cite {
    background: var(--surface);
    color: var(--accent);
    font-size: 0.75rem;
    padding: 0.25rem 0.5rem;
    border-radius: 6px;
  }
  .lock {
    font-size: 0.7rem;
    margin-right: 0.15rem;
  }
  .sheet-scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: flex-end;
    border: none;
    padding: 0;
  }
  .sheet {
    background: var(--surface);
    width: 100%;
    padding: 1rem;
    border-radius: 14px 14px 0 0;
    text-align: left;
  }
</style>
