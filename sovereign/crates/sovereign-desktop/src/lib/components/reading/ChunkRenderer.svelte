<!--
  ChunkRenderer — renders a center chunk plus its immediate textual
  neighbors in source order, with the atom layer overlaid.

  The center (cited) chunk is visually distinguished with a left-rail
  accent + faint background tint so the user can see exactly what the
  librarian drew from. Surrounding chunks fade slightly so reading
  attention naturally lands on the cited passage.

  Atom layer: spans returned by the backend (entities, states) get a
  soft dotted underline. The treatment is deliberately quiet — read
  past it unless you reach for it. PR4 wires the click handler to
  open the atom panel.
-->
<script lang="ts">
  import {
    readingSession,
    type ChunkRecord,
    type AtomSpan,
  } from "../../stores/readingSession.svelte";

  interface Props {
    prev: ChunkRecord[];
    center: ChunkRecord;
    next: ChunkRecord[];
  }

  let { prev, center, next }: Props = $props();

  /// Click handler for atom spans in any of the rendered chunks.
  /// Walks up from the clicked node to find the .atom span (so the
  /// click can land on a child text node), reads its data-* attrs,
  /// and opens the atom panel via the store. The corpus_id used is
  /// the center chunk's — atoms anchor at section ids within a
  /// single corpus, so prev/center/next all share the same.
  function handleAtomClick(e: MouseEvent) {
    const target = e.target as HTMLElement;
    const atomEl = target.closest<HTMLElement>(".atom");
    if (!atomEl) return;
    const atomId = atomEl.dataset.atomId;
    if (!atomId) return;
    e.stopPropagation();
    void readingSession.openAtom(center.corpus_id, atomId);
  }

  // Reading surface scrolls; the cited chunk is positioned ~25%
  // from the top of the viewport on mount so prev is visible
  // above and reading flows naturally downward.
  let citedRef = $state<HTMLElement | null>(null);

  $effect(() => {
    if (citedRef) {
      requestAnimationFrame(() => {
        citedRef?.scrollIntoView({ block: "start", behavior: "auto" });
        citedRef?.parentElement?.scrollBy({
          top: -window.innerHeight * 0.18,
          behavior: "auto",
        });
      });
    }
  });

  // ── Atom-overlay rendering ──────────────────────────────────
  //
  // The backend returns `atom_spans` with byte offsets into the
  // chunk's UTF-8 content. JS strings are UTF-16 code units, so
  // we can't blindly use `content.slice(span_start, span_end)`
  // for non-ASCII content — multibyte chars (emoji, accented
  // letters past 0x80) would mis-align.
  //
  // We solve this once per chunk by scanning the content and
  // building a byte-index → code-unit-index lookup table. For
  // ASCII-only chunks the table is the identity map and the
  // overhead is negligible; for multi-byte chunks we still pay
  // O(n) once per render, then atom positioning is O(spans).

  type Segment =
    | { kind: "text"; text: string }
    | { kind: "atom"; text: string; atom: AtomSpan };

  function buildByteToUtf16Map(content: string): number[] {
    // Returns an array of length (utf8Length + 1) where
    // table[byteIdx] = corresponding utf-16 index. The +1 lets
    // us safely look up `span_end` (one past the last byte).
    const table: number[] = new Array(0);
    const encoder = new TextEncoder();
    let codeUnitIdx = 0;
    for (const ch of content) {
      // Each iteration covers one Unicode scalar value: that's
      // 1 utf-16 code unit for BMP, 2 for surrogate pairs.
      const utf16Len = ch.length;
      const utf8Len = encoder.encode(ch).length;
      for (let i = 0; i < utf8Len; i++) {
        table.push(codeUnitIdx);
      }
      codeUnitIdx += utf16Len;
    }
    table.push(codeUnitIdx); // sentinel for end-of-text
    return table;
  }

  function segmentChunk(content: string, spans: AtomSpan[]): Segment[] {
    if (!spans || spans.length === 0) {
      return [{ kind: "text", text: content }];
    }
    // Sort spans by start offset so we can walk left-to-right
    // without overlap (the backend already de-overlaps, but
    // sort defensively).
    const sorted = [...spans].sort((a, b) => a.span_start - b.span_start);

    // Decide whether we need the multibyte mapping.
    // ASCII fast-path: byte length equals string length.
    const needsMap = content.length !== new TextEncoder().encode(content).length;
    const byteToUtf16 = needsMap ? buildByteToUtf16Map(content) : null;

    const u16 = (byteIdx: number): number => {
      if (!byteToUtf16) return byteIdx; // ASCII fast path
      // Clamp to the table bounds to survive any backend
      // off-by-one — better to mis-position than crash.
      const clamped = Math.min(Math.max(byteIdx, 0), byteToUtf16.length - 1);
      return byteToUtf16[clamped];
    };

    const segments: Segment[] = [];
    let cursorByte = 0;
    for (const span of sorted) {
      if (span.span_start < cursorByte) continue; // overlap, drop
      if (span.span_start > cursorByte) {
        segments.push({
          kind: "text",
          text: content.slice(u16(cursorByte), u16(span.span_start)),
        });
      }
      segments.push({
        kind: "atom",
        text: content.slice(u16(span.span_start), u16(span.span_end)),
        atom: span,
      });
      cursorByte = span.span_end;
    }
    if (cursorByte < content.length) {
      segments.push({ kind: "text", text: content.slice(u16(cursorByte)) });
    }
    return segments;
  }

  // Splits a chunk into paragraph-ish blocks while preserving the
  // atom-span overlay. The chunk content is raw text from the
  // extractor; preserve double-newline paragraph breaks so prose
  // with internal structure renders correctly.
  //
  // For chunks with atom spans, we build segments first then split
  // on paragraph boundaries — the segmentation is paragraph-agnostic
  // because spans are byte-anchored to the whole chunk content.
  function paragraphSegments(chunk: ChunkRecord): Segment[][] {
    const segments = segmentChunk(chunk.content, chunk.atom_spans ?? []);
    // Walk segments and split on \n\s*\n boundaries inside text
    // segments. Atom segments never contain paragraph breaks.
    const out: Segment[][] = [[]];
    const splitRe = /\n\s*\n/;
    for (const seg of segments) {
      if (seg.kind === "atom") {
        out[out.length - 1].push(seg);
        continue;
      }
      const parts = seg.text.split(splitRe);
      out[out.length - 1].push({ kind: "text", text: parts[0] });
      for (let i = 1; i < parts.length; i++) {
        out.push([{ kind: "text", text: parts[i] }]);
      }
    }
    // Strip empty paragraphs (whitespace-only).
    return out
      .map((segs) =>
        segs.filter((s) => !(s.kind === "text" && s.text.trim() === "")),
      )
      .filter((segs) => segs.length > 0);
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="chunk-renderer" onclick={handleAtomClick}>
  {#each prev as p (p.chunk_id)}
    <article class="chunk chunk--prev">
      {#each paragraphSegments(p) as paragraph, pIdx (pIdx)}
        <p>
          {#each paragraph as seg, sIdx (sIdx)}
            {#if seg.kind === "atom"}
              <span
                class="atom atom--{seg.atom.atom_type}"
                data-atom-id={seg.atom.atom_id}
                data-atom-type={seg.atom.atom_type}
                title="{seg.atom.atom_type}: {seg.atom.surface_form}"
              >{seg.text}</span>
            {:else}{seg.text}{/if}
          {/each}
        </p>
      {/each}
    </article>
  {/each}

  <article
    class="chunk chunk--cited"
    bind:this={citedRef}
    aria-label="Cited passage"
  >
    {#each paragraphSegments(center) as paragraph, pIdx (pIdx)}
      <p>
        {#each paragraph as seg, sIdx (sIdx)}
          {#if seg.kind === "atom"}
            <span
              class="atom atom--{seg.atom.atom_type}"
              data-atom-id={seg.atom.atom_id}
              data-atom-type={seg.atom.atom_type}
              title="{seg.atom.atom_type}: {seg.atom.surface_form}"
            >{seg.text}</span>
          {:else}{seg.text}{/if}
        {/each}
      </p>
    {/each}
  </article>

  {#each next as n (n.chunk_id)}
    <article class="chunk chunk--next">
      {#each paragraphSegments(n) as paragraph, pIdx (pIdx)}
        <p>
          {#each paragraph as seg, sIdx (sIdx)}
            {#if seg.kind === "atom"}
              <span
                class="atom atom--{seg.atom.atom_type}"
                data-atom-id={seg.atom.atom_id}
                data-atom-type={seg.atom.atom_type}
                title="{seg.atom.atom_type}: {seg.atom.surface_form}"
              >{seg.text}</span>
            {:else}{seg.text}{/if}
          {/each}
        </p>
      {/each}
    </article>
  {/each}
</div>

<style>
  .chunk-renderer {
    display: flex;
    flex-direction: column;
    gap: 28px;
    padding: 24px 32px 48px;
    /* Soft fade at top + bottom — signals "you're mid-document,
       this is a window into a larger work." */
    mask-image: linear-gradient(
      to bottom,
      transparent 0,
      black 32px,
      black calc(100% - 48px),
      transparent 100%
    );
  }

  .chunk {
    /* Comfortable reading column — caps line length so prose
       doesn't sprawl across the full reading panel width on
       wide displays. */
    max-width: 68ch;
    margin: 0 auto;
    line-height: 1.65;
    font-size: 0.95rem;
    color: var(--text-secondary);
    transition: opacity 200ms ease, color 200ms ease;
  }

  .chunk--prev, .chunk--next {
    opacity: 0.55;
  }

  .chunk--cited {
    color: var(--text-primary);
    opacity: 1;
    /* Left-rail accent + faint tint mark this chunk as the one
       the librarian drew from. Unobtrusive enough to read past;
       findable enough to do its evidentiary job. */
    border-left: 3px solid var(--accent, #c9a84c);
    padding-left: 18px;
    background: linear-gradient(
      to right,
      rgba(201, 168, 76, 0.04),
      transparent 80%
    );
    border-radius: 0 4px 4px 0;
  }

  .chunk p {
    margin: 0 0 14px 0;
  }

  .chunk p:last-child {
    margin-bottom: 0;
  }

  /* ── Atom layer ──
     Subtle dotted underline marks terms anchored at atoms in the
     atlas. Quiet enough to read past, present enough to find on
     reach. Per atom-type tinting keeps the visual differentiation
     ambient — entities are the most common, so they're treated
     most neutrally; states get a slightly different hue. */
  .atom {
    text-decoration: underline dotted
      color-mix(in srgb, var(--accent, #c9a84c) 70%, transparent);
    text-underline-offset: 3px;
    cursor: pointer;
    transition: background-color 120ms ease;
    border-radius: 2px;
  }

  .atom:hover {
    background-color: color-mix(
      in srgb,
      var(--accent, #c9a84c) 16%,
      transparent
    );
  }

  /* State labels are short condition phrases — slightly cooler
     hue distinguishes them from named entities without shouting. */
  .atom--state {
    text-decoration-color: color-mix(
      in srgb,
      var(--lavender, #9b87c4) 60%,
      transparent
    );
  }

  .atom--state:hover {
    background-color: color-mix(
      in srgb,
      var(--lavender, #9b87c4) 14%,
      transparent
    );
  }
</style>
