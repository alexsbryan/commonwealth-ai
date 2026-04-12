<script lang="ts">
  import { parseAssistantContent } from "../parse-message";
  import { renderMarkdown } from "../utils/markdown";
  import { insightStore } from "../stores/insights.svelte";
  import { clipInsight } from "../api";
  import type { InsightSource } from "../types";
  import ThinkBlock from "./ThinkBlock.svelte";
  import RoutingMeta from "./RoutingMeta.svelte";
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

<div class="sv-ai-msg">
  <div class="role-label">&#x25C8; SOVEREIGN</div>

  <RoutingMeta {provenance} {retrievedChunks} />

  {#each thinkBlocks as block}
    <ThinkBlock content={block.text} />
  {/each}

  {#if proseText}
    <div class="sv-prose">
      {@html proseHtml}
    </div>
  {/if}

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
