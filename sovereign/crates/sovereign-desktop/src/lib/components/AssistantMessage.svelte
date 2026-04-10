<script lang="ts">
  import { parseAssistantContent } from "../parse-message";
  import { insightStore } from "../stores/insights.svelte";
  import { clipInsight } from "../api";
  import type { InsightSource } from "../types";
  import ThinkBlock from "./ThinkBlock.svelte";
  import RoutingMeta from "./RoutingMeta.svelte";
  import ResponseParagraph from "./ResponseParagraph.svelte";
  import ResearchGapCard from "./ResearchGapCard.svelte";
  import SourceAttribution from "./SourceAttribution.svelte";

  interface Props {
    content: string;
    messageId: string;
    conversationId: string;
    metadata?: Record<string, unknown>;
    isStreaming?: boolean;
  }

  let { content, messageId, conversationId, metadata, isStreaming }: Props =
    $props();

  let blocks = $derived(parseAssistantContent(content));

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

  // Build source from provenance metadata.
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
    position?: { name: string; style: import("../types").PositionStyle };
  }) {
    const source = buildSource();
    const sourceJson = JSON.stringify(source);
    const positionJson = detail.position
      ? JSON.stringify({
          name: detail.position.name,
          style: detail.position.style,
        })
      : undefined;

    try {
      const node = await clipInsight(
        detail.text,
        messageId,
        detail.paragraphIndex,
        sourceJson,
        positionJson,
      );
      insightStore.add(node);
    } catch (e) {
      console.error("Failed to clip insight:", e);
    }
  }
</script>

<div class="sv-ai-msg">
  <div class="role-label">&#x25C8; SOVEREIGN</div>

  <RoutingMeta {provenance} />

  {#each blocks as block, i (i)}
    {#if block.type === "think"}
      <ThinkBlock content={block.text} />
    {:else if block.type === "research_gap"}
      <ResearchGapCard text={block.text} gapQuery={block.gapQuery} />
    {:else}
      <ResponseParagraph
        text={block.text}
        index={i}
        position={block.position}
        alreadyClipped={insightStore.has(messageId, i)}
        onclip={handleClip}
      />
    {/if}
  {/each}

  <SourceAttribution {content} />
</div>

<style>
  .sv-ai-msg {
    align-self: flex-start;
    padding: 0 0 0 14px;
    border-left: 2px solid color-mix(in srgb, var(--lavender) 35%, transparent);
    max-width: 82%;
    margin-bottom: 18px;
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
