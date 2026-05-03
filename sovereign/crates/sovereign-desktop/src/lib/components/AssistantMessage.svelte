<script lang="ts">
  import { parseAssistantContent } from "../parse-message";
  import { renderMarkdown } from "../utils/markdown";
  import { insightStore } from "../stores/insights.svelte";
  import { clipInsight } from "../api";
  import { readingSession } from "../stores/readingSession.svelte";
  import type { InsightSource, NextStepOffer } from "../types";
  import ThinkBlock from "./ThinkBlock.svelte";
  import RoutingMeta from "./RoutingMeta.svelte";
  import SourceAttribution from "./SourceAttribution.svelte";
  import SourcePopover from "./SourcePopover.svelte";
  import NextStepButtons from "./NextStepButtons.svelte";

  interface Props {
    content: string;
    messageId: string;
    conversationId: string;
    metadata?: Record<string, unknown>;
    isStreaming?: boolean;
    onNextStep?: (offer: NextStepOffer) => void;
  }

  let {
    content,
    messageId,
    conversationId,
    metadata,
    isStreaming,
    onNextStep,
  }: Props = $props();

  let blocks = $derived(parseAssistantContent(content));

  // Separate think blocks from prose content. Prose blocks are merged
  // and rendered as a single markdown document so headings, lists, and
  // horizontal rules render correctly.
  let thinkBlocks = $derived(blocks.filter((b) => b.type === "think"));
  let proseText = $derived(
    blocks
      .filter((b) => b.type !== "think")
      .map((b) => b.text)
      .join("\n\n"),
  );
  let proseHtml = $derived(renderMarkdown(proseText));

  let provenance = $derived(
    metadata?.provenance as
      | {
          intent: string;
          search_method?: string;
          sources?: { origin: string; count: number }[];
          inference_backend: string;
          oicp_match?: string;
          total_latency_ms: number;
          tokens_used: number;
          coarse_intent?: string;
          self_assessment?: string;
        }
      | undefined,
  );

  let retrievedChunks = $derived(
    (metadata?.retrieved_chunks ?? []) as Array<{
      title: string;
      corpus_id: string;
      url?: string;
      snippet: string;
      // PR1 plumbed these through; absent on legacy / synthetic
      // (atlas-virtual, web-fetch) chunks.
      chunk_id?: number | null;
      source_doc_id?: string | null;
    }>,
  );

  // The user-visible question that triggered this assistant turn —
  // used as the first step of the inquiry breadcrumb when a
  // citation opens the reading surface. Falls back to a generic
  // label when not derivable from message metadata.
  let originLabel = $derived(
    (metadata?.user_query as string | undefined) ?? "From your question",
  );

  // Set by chat.machine when the user redirected away from this
  // message's Propose banner. The bubble stays in history (so the
  // redirect decision is legible later) but renders de-emphasised.
  let redirectedAway = $derived(metadata?.redirected_away === true);

  // PR3 — grounded next-step offers. Hide on streaming bubbles
  // (answer not done yet) and redirected-away bubbles (the user
  // already moved past this answer).
  let nextStepOffers = $derived(
    (metadata?.next_steps ?? []) as NextStepOffer[],
  );
  let showNextSteps = $derived(
    !isStreaming &&
      !redirectedAway &&
      nextStepOffers.length > 0 &&
      !!onNextStep,
  );

  // ── Citation popover state ─────────────────────────────────

  let popoverChunk = $state<{
    title: string;
    corpus_id: string;
    url?: string;
    snippet: string;
  } | null>(null);

  let popoverAnchor = $state({ x: 0, y: 0 });

  /// Toast-style "we cited a source that wasn't in the retrieval
  /// set" indicator. Surfaces the failure mode legibly instead of
  /// the previous behavior — silently grabbing an arbitrary chunk.
  /// Cleared after 3 seconds or on the next citation click.
  let unresolvedNotice = $state<string | null>(null);
  let unresolvedTimeout: ReturnType<typeof setTimeout> | null = null;

  function showUnresolved(name: string) {
    unresolvedNotice = name;
    if (unresolvedTimeout) clearTimeout(unresolvedTimeout);
    unresolvedTimeout = setTimeout(() => {
      unresolvedNotice = null;
    }, 3000);
  }

  function handleProseClick(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (!target.classList.contains("source-citation")) return;

    const sourceName = target.getAttribute("data-source");
    if (!sourceName) return;

    // Resolve `data-source` to a retrieved chunk via increasingly
    // permissive matchers. Crucially: there is NO arbitrary
    // fallback — if nothing matches we surface that to the user
    // rather than opening a wrong card. (Prior behavior grabbed
    // `retrievedChunks[0]` when no match was found, which produced
    // surprises like a Dostoevsky citation opening a George H. W.
    // Bush snippet from a totally unrelated wikipedia hit.)
    const sn = sourceName.toLowerCase();
    const MIN_OVERLAP = 4; // chars — guards against single-token spurious hits like "Bush" in many titles
    const chunk =
      // 1. Exact title match.
      retrievedChunks.find((c) => c.title === sourceName) ??
      // 2. Case-insensitive title match.
      retrievedChunks.find((c) => c.title.toLowerCase() === sn) ??
      // 3. Source name appears within a longer title (require
      //    enough characters to avoid spurious common-word hits).
      retrievedChunks.find(
        (c) =>
          sn.length >= MIN_OVERLAP &&
          c.title.toLowerCase().includes(sn),
      ) ??
      // 4. Title appears within the source name (require the
      //    title itself to be substantive — a 4-char title would
      //    match too eagerly).
      retrievedChunks.find(
        (c) =>
          c.title.length >= MIN_OVERLAP &&
          sn.includes(c.title.toLowerCase()),
      ) ??
      null;

    if (!chunk) {
      showUnresolved(sourceName);
      return;
    }

    // Glass-box reading surface — when the citation carries a
    // chunk_id, open the cited passage in the reading column
    // instead of the legacy popover. The popover is a useful
    // fallback for synthetic / web chunks that don't have a
    // dereferenceable id.
    if (chunk.chunk_id != null && chunk.corpus_id) {
      void readingSession.openCitation(
        chunk.corpus_id,
        chunk.chunk_id,
        originLabel,
      );
      return;
    }

    const rect = target.getBoundingClientRect();
    popoverAnchor = { x: rect.left, y: rect.bottom };
    popoverChunk = chunk;
  }

  // ── Insight clipping ───────────────────────────────────────

  function buildSource(): InsightSource {
    const sources = provenance?.sources ?? [];
    const corpusSource = sources.find((s) => s.count > 0);
    return {
      corpus_id: corpusSource?.origin ?? null,
      article_title: null,
      conversation_id: conversationId,
    };
  }

  async function handleClip(detail: {
    text: string;
    paragraphIndex: number;
  }) {
    const source = buildSource();
    const sourceJson = JSON.stringify(source);

    try {
      const node = await clipInsight(
        detail.text,
        messageId,
        detail.paragraphIndex,
        sourceJson,
        undefined,
      );
      insightStore.add(node);
    } catch (e) {
      console.error("Failed to clip insight:", e);
    }
  }
</script>

<div class="sv-ai-msg" class:redirected={redirectedAway}>
  <div class="role-label">
    &#x25C8; SOVEREIGN
    {#if redirectedAway}
      <span class="redirected-note">• redirected to a different approach</span>
    {/if}
  </div>

  <RoutingMeta {provenance} {retrievedChunks} />

  {#each thinkBlocks as block}
    <ThinkBlock content={block.text} />
  {/each}

  {#if proseText}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div class="sv-prose" onclick={handleProseClick}>
      {@html proseHtml}
    </div>
  {/if}

  <SourceAttribution {content} />

  {#if showNextSteps}
    <NextStepButtons
      offers={nextStepOffers}
      onselect={(offer) => onNextStep?.(offer)}
    />
  {/if}
</div>

{#if popoverChunk}
  <SourcePopover
    chunk={popoverChunk}
    anchor={popoverAnchor}
    onclose={() => (popoverChunk = null)}
  />
{/if}

{#if unresolvedNotice}
  <div class="unresolved-toast" role="status">
    <span class="dot" aria-hidden="true">⚠</span>
    Cited source <em>{unresolvedNotice}</em> wasn't in the retrieved chunks.
  </div>
{/if}

<style>
  .sv-ai-msg {
    align-self: flex-start;
    padding: 0 0 0 14px;
    border-left: 2px solid color-mix(in srgb, var(--lavender) 35%, transparent);
    max-width: 82%;
    margin-bottom: 18px;
  }

  .sv-ai-msg.redirected {
    opacity: 0.55;
    border-left-style: dashed;
  }

  .redirected-note {
    margin-left: 8px;
    font-weight: 400;
    color: var(--text-muted);
    letter-spacing: 0.06em;
    text-transform: none;
  }

  .role-label {
    font-size: 0.67rem;
    font-weight: 700;
    letter-spacing: 0.12em;
    color: var(--accent);
    margin-bottom: 6px;
    text-transform: uppercase;
    filter: drop-shadow(0 0 4px rgba(201, 168, 76, 0.3));
    font-family: var(--font-sans);
  }

  /* Unresolved-citation toast — surfaces the failure mode legibly
     instead of opening an arbitrary wrong card. Self-dismissing. */
  .unresolved-toast {
    position: fixed;
    bottom: 24px;
    left: 50%;
    transform: translateX(-50%);
    padding: 10px 16px;
    font-size: 0.82rem;
    color: var(--text-secondary);
    background: var(--bg-elevated, var(--bg-surface));
    border: 1px solid var(--border-mid);
    border-radius: 8px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.25);
    z-index: 60;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    pointer-events: none;
    animation: toast-fade 3s ease forwards;
  }

  .unresolved-toast .dot {
    color: var(--warning, #c9a84c);
  }

  .unresolved-toast em {
    font-style: normal;
    color: var(--text-primary);
    font-weight: 500;
  }

  @keyframes toast-fade {
    0%   { opacity: 0; transform: translate(-50%, 6px); }
    8%   { opacity: 1; transform: translate(-50%, 0); }
    85%  { opacity: 1; transform: translate(-50%, 0); }
    100% { opacity: 0; transform: translate(-50%, -2px); }
  }
</style>
