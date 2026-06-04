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
  import { corporaStore } from "../stores/corpora.svelte";
  import ReaderView from "../ui/ReaderView.svelte";

  let { content, metadata }: { content: string; metadata?: Record<string, unknown> } = $props();

  const blocks = $derived(parseAssistantContent(content));
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const provenance = $derived((metadata?.provenance ?? null) as any);
  // Wire shape from the Rust core's `metadata_blob` (remote/map.rs): each
  // citation carries an opaque string `chunk_id` handle (resolved against
  // the cache on tap), the corpus, an optional title, and the snippet.
  const retrievedChunks = $derived(
    (metadata?.retrieved_chunks ?? []) as Array<{
      title: string | null;
      corpus_id: string;
      chunk_id: string;
      snippet: string;
    }>,
  );

  // The shared @sovereign/chat-ui leaves (SourceAttribution / RoutingMeta)
  // require `title` + `snippet` and type `chunk_id` as a numeric desktop
  // chunk index. Project mobile's rows to exactly the fields those leaves
  // render — dropping the string `chunk_id`, which only the tap-to-resolve
  // path below needs — so the prop contracts line up.
  const renderChunks = $derived(
    retrievedChunks.map((c) => ({
      title: c.title ?? c.corpus_id,
      corpus_id: c.corpus_id,
      snippet: c.snippet,
    })),
  );

  // Tapping a citation — the inline ◈ chip rendered in the prose, or a
  // chip in the grid below — opens the glass-box reader on the cited
  // passage (full text + context, fetched from the host).
  type ReaderTarget = {
    corpusId: string;
    chunkId: string;
    title: string;
    isPrivate: boolean;
  };
  let openReader = $state<ReaderTarget | null>(null);

  function openReaderFor(c: {
    corpus_id: string;
    chunk_id: string;
    title: string | null;
  }) {
    openReader = {
      corpusId: c.corpus_id,
      chunkId: c.chunk_id,
      title: c.title ?? c.corpus_id,
      isPrivate: corporaStore.isPrivate(c.corpus_id),
    };
  }

  // Delegated handler for the inline ◈ citations markdown.ts emits as
  // `<span class="source-citation">`. Map the chip back to a retrieved
  // chunk — by numeric index for `[N]`, else by title for `[Source: X]` —
  // and open the reader.
  // Resolve a tapped/activated `.source-citation` element to a retrieved
  // chunk and open the reader. Shared by the pointer + keyboard handlers.
  function resolveCitationEl(el: HTMLElement) {
    const idxAttr = el.getAttribute("data-citation-index");
    if (idxAttr) {
      const chunk = retrievedChunks[parseInt(idxAttr, 10) - 1];
      if (chunk) openReaderFor(chunk);
      return;
    }

    const source = el.getAttribute("data-source");
    if (!source) return;
    const sn = source.toLowerCase();
    const chunk =
      retrievedChunks.find((c) => c.title === source) ??
      retrievedChunks.find((c) => (c.title ?? "").toLowerCase() === sn) ??
      retrievedChunks.find((c) => {
        const t = (c.title ?? "").toLowerCase();
        return t.length > 0 && (t.includes(sn) || sn.includes(t));
      });
    if (chunk) openReaderFor(chunk);
  }

  function onProseClick(e: MouseEvent) {
    const el = (e.target as HTMLElement | null)?.closest(
      ".source-citation",
    ) as HTMLElement | null;
    if (!el) return;
    e.preventDefault();
    resolveCitationEl(el);
  }

  // Keyboard activation — the inline chips are `role="button" tabindex=0`,
  // so Enter/Space must trigger them like a real button.
  function onProseKeydown(e: KeyboardEvent) {
    if (e.key !== "Enter" && e.key !== " ") return;
    const el = (e.target as HTMLElement | null)?.closest(
      ".source-citation",
    ) as HTMLElement | null;
    if (!el) return;
    e.preventDefault();
    resolveCitationEl(el);
  }
</script>

<!-- The wrapper delegates pointer + keyboard activation for the inline
     citation chips markdown.ts injects via {@html} (role=button tabindex=0
     spans that can't be real <button>s through @html). -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="assistant" onclick={onProseClick} onkeydown={onProseKeydown}>
  {#each blocks as block}
    {#if block.type === "think"}
      <ThinkBlock content={block.text} />
    {:else}
      <div class="sv-prose">{@html renderMarkdown(block.text)}</div>
    {/if}
  {/each}

  <SourceAttribution {content} retrievedChunks={renderChunks} />

  {#if retrievedChunks.length}
    <div class="cites" role="group" aria-label="Sources">
      {#each retrievedChunks as c (c.chunk_id)}
        <button
          class="cite"
          onclick={() => openReaderFor(c)}
          aria-label={`Read source: ${c.title ?? c.corpus_id}${corporaStore.isPrivate(c.corpus_id) ? " (private to this host)" : ""}`}
        >
          {#if corporaStore.isPrivate(c.corpus_id)}<span
              class="lock"
              aria-hidden="true"
              title="Private to this host — never shared with mesh peers">🔒</span
            >{/if}
          {c.title ?? c.corpus_id}
        </button>
      {/each}
    </div>
  {/if}

  {#if provenance}
    <RoutingMeta {provenance} retrievedChunks={renderChunks} />
  {/if}
</div>

{#if openReader}
  {#key openReader.corpusId + openReader.chunkId}
    <ReaderView
      corpusId={openReader.corpusId}
      chunkId={openReader.chunkId}
      title={openReader.title}
      isPrivate={openReader.isPrivate}
      onclose={() => (openReader = null)}
    />
  {/key}
{/if}

<style>
  .assistant {
    align-self: flex-start;
    max-width: 94%;
    min-width: 0;
  }
  /* Clickable corpus citations — the lavender ◈ chip, the Sovereign
     citation signature. 🔒 marks corpora private to this host. */
  .cites {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    margin-top: 0.7rem;
    padding-top: 0.7rem;
    border-top: 1px solid var(--border);
  }
  .cite {
    display: inline-flex;
    align-items: center;
    gap: 0.22rem;
    font-family: var(--font-sans);
    font-size: 0.74rem;
    font-weight: 500;
    color: var(--lavender-light);
    background: var(--lavender-dim);
    border: 1px solid color-mix(in srgb, var(--lavender) 26%, transparent);
    padding: 0.22rem 0.55rem;
    border-radius: var(--radius);
    transition: background 0.15s, border-color 0.15s;
  }
  .cite::before {
    content: "◈";
    font-size: 0.7em;
    opacity: 0.6;
  }
  .cite:active {
    background: color-mix(in srgb, var(--lavender) 24%, transparent);
    border-color: var(--lavender);
  }
  .lock { font-size: 0.72em; }
</style>
