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
  interface Props {
    content: string;
  }

  let { content }: Props = $props();

  interface SourceGroup {
    label: string;
    count: number;
    sources: string[];
  }

  let groups: SourceGroup[] = $derived.by(() => {
    return parseSources(content);
  });

  let expanded = $state(false);

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
    const groupMap = new Map<string, string[]>();

    for (const line of lines) {
      const cleaned = line.replace(/^\[\d+\]\s*/, "").trim();
      // Try to extract corpus name from "corpus: article" format.
      const colonIdx = cleaned.indexOf(": ");
      let label: string;
      if (colonIdx > 0 && colonIdx < 30) {
        label = cleaned.slice(0, colonIdx);
      } else if (cleaned.includes(" \u2014 ")) {
        // "title -- url" format
        label = "Web";
      } else {
        label = "Source";
      }
      const existing = groupMap.get(label) ?? [];
      existing.push(cleaned);
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
            <div class="source-item">{source}</div>
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
</style>
