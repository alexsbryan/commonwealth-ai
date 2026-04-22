<script lang="ts">
  import { parseAssistantContent } from "../parse-message";
  import { renderMarkdown } from "../utils/markdown";
  import { insightStore } from "../stores/insights.svelte";
  import { clipInsight } from "../api";
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
    }>,
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

  function handleProseClick(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (!target.classList.contains("source-citation")) return;

    const sourceName = target.getAttribute("data-source");
    if (!sourceName) return;

    // Find the matching chunk — try several matching strategies.
    const sn = sourceName.toLowerCase();
    const chunk =
      // 1. Exact title match.
      retrievedChunks.find((c) => c.title === sourceName) ??
      // 2. Case-insensitive title match.
      retrievedChunks.find((c) => c.title.toLowerCase() === sn) ??
      // 3. Source name appears within a longer title.
      retrievedChunks.find((c) => c.title.toLowerCase().includes(sn)) ??
      // 4. Title appears within the source name.
      retrievedChunks.find((c) =>
        sn.includes(c.title.toLowerCase()) && c.title.length > 2,
      ) ??
      // 5. Fallback: first chunk from any corpus (show something rather than nothing).
      (retrievedChunks.length > 0 ? retrievedChunks[0] : null);

    if (!chunk) return;

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
</style>
