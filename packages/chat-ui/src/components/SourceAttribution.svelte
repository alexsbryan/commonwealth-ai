<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts" module>
  /**
   * Strip the "Sources:" section from message content.
   * Returns the content before the sources block.
   */
  export function stripSources(content: string): string {
    const idx = content.lastIndexOf("\nSources:\n");
    if (idx === -1) {
      const idx2 = content.lastIndexOf("\n\nSources:\n");
      if (idx2 === -1) return content;
      return content.slice(0, idx2).trimEnd();
    }
    return content.slice(0, idx).trimEnd();
  }
</script>

<script lang="ts">
  /** Conv-tiered PPR provenance gate (A3-lite, spec
   *  CONV_TIERED_PORT.md). Sources whose matching retrieved chunk
   *  carries a `ppr_mass_norm > PPR_BADGE_THRESHOLD` get an
   *  "↗ surfaced via entity bridge: <seed>" subtitle. Threshold
   *  picked so only well-boosted chunks render the badge — chunks
   *  that barely cleared cosine baseline don't add noise. */
  const PPR_BADGE_THRESHOLD = 0.5;

  interface RetrievedChunk {
    title: string;
    corpus_id: string;
    url?: string;
    snippet: string;
    chunk_id?: number | null;
    source_doc_id?: string | null;
    metadata?: Record<string, string>;
  }

  interface Props {
    content: string;
    /** When provided, the source list cross-references each parsed
     *  citation line against the retrieved-chunks payload to surface
     *  PPR-bridge provenance subtitles. Match is by title (fuzzy:
     *  case-insensitive, with trim). Missing matches degrade
     *  gracefully to the original source line. */
    retrievedChunks?: RetrievedChunk[];
  }

  let { content, retrievedChunks }: Props = $props();

  interface ParsedSource {
    raw: string;
    title: string;
  }

  interface SourceGroup {
    label: string;
    count: number;
    sources: ParsedSource[];
  }

  let groups: SourceGroup[] = $derived.by(() => {
    return parseSources(content);
  });

  let expanded = $state(false);

  /** Per-source PPR-bridge attribution. Returns the bridge seed
   *  entity name when the retrieved chunk matching this source line
   *  has `metadata.ppr_mass_norm > threshold` and a `ppr_seed`. */
  function pprBridgeFor(source: ParsedSource): string | null {
    if (!retrievedChunks || retrievedChunks.length === 0) return null;
    const titleNorm = source.title.toLowerCase().trim();
    if (titleNorm === "") return null;
    const match =
      retrievedChunks.find((c) => c.title === source.title) ??
      retrievedChunks.find((c) => c.title.toLowerCase().trim() === titleNorm);
    if (!match || !match.metadata) return null;
    const massRaw = match.metadata.ppr_mass_norm;
    const seed = match.metadata.ppr_seed;
    if (!seed || !massRaw) return null;
    const mass = parseFloat(massRaw);
    if (!Number.isFinite(mass) || mass <= PPR_BADGE_THRESHOLD) return null;
    return seed;
  }

  function parseSources(text: string): SourceGroup[] {
    // Find "Sources:" section.
    const patterns = ["\n\nSources:\n", "\nSources:\n"];
    let sourcesText = "";
    for (const p of patterns) {
      const idx = text.lastIndexOf(p);
      if (idx !== -1) {
        sourcesText = text.slice(idx + p.length);
        break;
      }
    }
    if (!sourcesText) return [];

    // Parse lines like "[1] corpus: article" or "[1] title -- url"
    const lines = sourcesText.split("\n").filter((l) => l.trim().startsWith("["));
    const groupMap = new Map<string, ParsedSource[]>();

    for (const line of lines) {
      const cleaned = line.replace(/^\[\d+\]\s*/, "").trim();
      // Try to extract corpus name from "corpus: article" format.
      const colonIdx = cleaned.indexOf(": ");
      let label: string;
      let title: string;
      if (colonIdx > 0 && colonIdx < 30) {
        label = cleaned.slice(0, colonIdx);
        title = cleaned.slice(colonIdx + 2);
      } else if (cleaned.includes(" \u2014 ")) {
        // "title \u2014 url" format (em-dash separator)
        label = "Web";
        title = cleaned.split(" \u2014 ")[0];
      } else {
        label = "Source";
        title = cleaned;
      }
      const existing = groupMap.get(label) ?? [];
      existing.push({ raw: cleaned, title });
      groupMap.set(label, existing);
    }

    return Array.from(groupMap.entries()).map(([label, sources]) => ({
      label,
      count: sources.length,
      sources,
    }));
  }
</script>

{#if groups.length > 0}
  <div class="attribution">
    <div class="badges" role="button" tabindex="0" onclick={() => (expanded = !expanded)} onkeydown={(e) => e.key === 'Enter' && (expanded = !expanded)}>
      {#each groups as group}
        <span class="badge">{group.label} ({group.count})</span>
      {/each}
    </div>
    {#if expanded}
      <div class="source-list">
        {#each groups as group}
          {#each group.sources as source}
            {@const bridge = pprBridgeFor(source)}
            <div class="source-item">
              <div class="source-line">{source.raw}</div>
              {#if bridge}
                <div class="ppr-bridge" title="Conv-tiered entity-graph PPR boost (A3-lite)">
                  ↗ surfaced via entity bridge:
                  <span class="bridge-seed">{bridge}</span>
                </div>
              {/if}
            </div>
          {/each}
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .attribution {
    margin-top: 8px;
  }
  .badges {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    cursor: pointer;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    padding: 2px 10px;
    font-size: 0.75rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .source-list {
    margin-top: 6px;
    padding: 8px 12px;
    background: var(--bg-surface);
    border-radius: var(--radius);
    border: 1px solid var(--border);
  }
  .source-item {
    font-size: 0.8rem;
    color: var(--text-secondary);
    padding: 2px 0;
    line-height: 1.4;
  }
  .ppr-bridge {
    margin-top: 2px;
    font-size: 0.72rem;
    color: var(--text-muted);
    font-style: italic;
    padding-left: 1.2em;
  }
  .bridge-seed {
    color: var(--lavender-light);
    font-style: normal;
    font-weight: 500;
  }
</style>
