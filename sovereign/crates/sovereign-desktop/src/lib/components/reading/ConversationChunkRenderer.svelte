<!--
  ConversationChunkRenderer — chosen by ReadingSurface when the cited
  chunk's `corpus_id == "conversation-history"`. Renders the chunk's
  role-tagged segments as mini message bubbles instead of a single
  paragraph, surfaces the conversation's title + last-updated date as
  the reading-card header, and exposes a "View conversation" button
  that bounces back to the live chat.

  Conv-tiered enrichment (spec CONV_TIERED_PORT.md §A2) populates a
  top-N entity chip row above the bubbles. Chips come from the
  conv's RAPTOR primary_entities via the
  `atlas_get_conv_entities` Tauri command. Tiny synthetic convs
  return an empty chip list and the row simply collapses.

  No atom-span layer yet (per-message span tagging). When that
  ships, entity/state pills can inline inside `<ConversationBubble>`
  the same way `ChunkRenderer` overlays them on prose.
-->
<script lang="ts">
  import {
    readingSession,
    type ChunkRecord,
    type ConversationChunkMeta,
    type ConversationSegment,
  } from "../../stores/readingSession.svelte";
  import { atlasGetConvEntities } from "../../api";
  import type { ConvEntityChip } from "../../types";

  interface Props {
    prev: ChunkRecord[];
    center: ChunkRecord;
    next: ChunkRecord[];
  }

  let { prev, center, next }: Props = $props();

  // The center chunk MUST carry a `conversation` payload to land in
  // this renderer — `ReadingSurface`'s switch checks for it. The
  // assertion is for the type-narrow only; ReadingSurface guarantees
  // it at the call site.
  let conv = $derived(center.conversation as ConversationChunkMeta);

  /// Resolve the title shown at the top of the card. Conversations
  /// that haven't been auto-titled yet fall back to "Conversation"
  /// rather than "untitled" (which reads as a label, not a noun).
  let displayTitle = $derived(conv.title ?? "Conversation");

  /// Format the conversation's last-updated epoch as "Last
  /// updated <date>". Drops to empty string when not available.
  function formatUpdatedAt(epoch: number | null | undefined): string {
    if (epoch == null) return "";
    const date = new Date(epoch * 1000);
    const now = new Date();
    const sameYear = date.getFullYear() === now.getFullYear();
    return date.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
      year: sameYear ? undefined : "numeric",
    });
  }

  let updatedLabel = $derived(formatUpdatedAt(conv.updated_at));

  // Conv-tiered entity chips (A2). Fetched async on mount; empty
  // until the response lands so first paint isn't blocked. Tiny
  // synthetic convs return an empty list, collapsing the chip row.
  let entityChips: ConvEntityChip[] = $state([]);
  /** "Just my chats" toggle for chip clicks. When true, chip search
   *  scopes to conv corpora; otherwise cross-corpus default. */
  let scopeToChats = $state(false);
  $effect(() => {
    const corpusId = center.corpus_id;
    const convUuid = conv.conversation_id;
    if (!corpusId || !convUuid) {
      entityChips = [];
      return;
    }
    let cancelled = false;
    atlasGetConvEntities(corpusId, convUuid)
      .then((chips) => {
        if (!cancelled) {
          entityChips = chips;
        }
      })
      .catch(() => {
        // Conv corpus may not have tiered enrichment yet (e.g.
        // legacy conversation-history); silently render no chips.
        if (!cancelled) {
          entityChips = [];
        }
      });
    return () => {
      cancelled = true;
    };
  });

  /** Click feedback state — shows a 1.2s "copied" pulse on the
   *  clicked chip so the user knows something happened. */
  let copiedChip = $state<string | null>(null);

  function handleChipClick(name: string) {
    // Until the chat surface exposes a "search with scope" hook, the
    // most useful affordance is clipboard-copy of the (optionally
    // scoped) query string so the user can paste it into a new chat
    // without retyping. Honest about the wiring being v1; full
    // search-store integration tracked in CONV_TIERED_PORT.md.
    const prefix = scopeToChats ? "in:conversations " : "";
    const text = prefix + name;
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      void navigator.clipboard
        .writeText(text)
        .then(() => {
          copiedChip = name;
          setTimeout(() => {
            if (copiedChip === name) {
              copiedChip = null;
            }
          }, 1200);
        })
        .catch(() => {
          // Clipboard access can fail under non-secure contexts /
          // permission denial — swallow rather than alert.
        });
    }
  }

  /// Robust segment extraction: prefer the backend-parsed segments,
  /// but fall back to a single user-bubble carrying the raw chunk
  /// text if (a) the chunk is malformed or (b) a future ingest path
  /// produces conversation chunks without segment markers. Either
  /// way the user sees content rather than a blank card.
  function segmentsFor(chunk: ChunkRecord): ConversationSegment[] {
    const segs = chunk.conversation?.segments ?? [];
    if (segs.length > 0) return segs;
    if (chunk.content.trim().length === 0) return [];
    return [{ role: "user", content: chunk.content }];
  }

  /// The cited chunk is centred via scrollIntoView on mount, same
  /// pattern as ChunkRenderer. Without the post-scroll nudge the
  /// header sits flush at the top and prev becomes invisible above
  /// the fold.
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

  function handleViewConversation() {
    readingSession.openConversation(conv.conversation_id);
  }

  /// "user" / "assistant" / "system" → CSS class. Other strings
  /// fall through to a neutral pill so unknown roles don't break.
  function roleClass(role: string): string {
    switch (role) {
      case "user":
        return "bubble-user";
      case "assistant":
        return "bubble-assistant";
      case "system":
        return "bubble-system";
      default:
        return "bubble-other";
    }
  }
</script>

<div class="conv-card">
  <div class="conv-card-head">
    <div class="conv-title-row">
      <span class="conv-icon" aria-hidden="true">◇</span>
      <span class="conv-title">{displayTitle}</span>
    </div>
    {#if updatedLabel}
      <span class="conv-updated">Last updated {updatedLabel}</span>
    {/if}
    <button
      type="button"
      class="conv-jump-btn"
      onclick={handleViewConversation}
      title="Open this conversation in the chat sidebar"
    >
      View conversation →
    </button>
  </div>

  {#if entityChips.length > 0}
    <div class="conv-entity-row" data-testid="conv-entity-chips">
      <span class="entity-row-label">this conversation:</span>
      {#each entityChips as chip (chip.name)}
        <button
          type="button"
          class="entity-chip"
          class:copied={copiedChip === chip.name}
          onclick={() => handleChipClick(chip.name)}
          title={`salience ${chip.salience.toFixed(2)} · ${chip.occurrence_count} cluster${chip.occurrence_count === 1 ? "" : "s"} · click to copy`}
        >
          {copiedChip === chip.name ? "copied!" : chip.name}
        </button>
      {/each}
      <label class="scope-toggle" title="Restrict chip search to conversation corpora">
        <input type="checkbox" bind:checked={scopeToChats} />
        <span>just my chats</span>
      </label>
    </div>
  {/if}

  {#each prev as chunk (chunk.chunk_id)}
    <div class="conv-block faded" data-chunk-id={chunk.chunk_id}>
      {#each segmentsFor(chunk) as seg, i (i)}
        <div class={`bubble ${roleClass(seg.role)}`}>
          <div class="role">{seg.role}</div>
          <div class="content">{seg.content}</div>
        </div>
      {/each}
    </div>
  {/each}

  <div
    class="conv-block cited"
    bind:this={citedRef}
    data-chunk-id={center.chunk_id}
    data-cited="true"
  >
    {#each segmentsFor(center) as seg, i (i)}
      <div class={`bubble ${roleClass(seg.role)}`}>
        <div class="role">{seg.role}</div>
        <div class="content">{seg.content}</div>
      </div>
    {/each}
  </div>

  {#each next as chunk (chunk.chunk_id)}
    <div class="conv-block faded" data-chunk-id={chunk.chunk_id}>
      {#each segmentsFor(chunk) as seg, i (i)}
        <div class={`bubble ${roleClass(seg.role)}`}>
          <div class="role">{seg.role}</div>
          <div class="content">{seg.content}</div>
        </div>
      {/each}
    </div>
  {/each}
</div>

<style>
  .conv-card {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px 18px 28px;
  }

  .conv-card-head {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 12px 14px;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg, 8px);
  }

  .conv-title-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .conv-icon {
    color: var(--accent);
    font-size: 1rem;
    line-height: 1;
  }

  .conv-title {
    font-weight: 600;
    color: var(--text-primary);
    font-size: 0.95rem;
  }

  .conv-updated {
    font-size: 0.72rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    letter-spacing: 0.04em;
  }

  .conv-jump-btn {
    align-self: flex-start;
    margin-top: 6px;
    padding: 5px 12px;
    background: transparent;
    border: 1px solid var(--accent);
    border-radius: 999px;
    color: var(--accent);
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    cursor: pointer;
    transition: background 160ms ease;
  }

  .conv-jump-btn:hover,
  .conv-jump-btn:focus-visible {
    background: var(--accent-dim, color-mix(in oklab, var(--accent, #c4a46a) 12%, transparent));
    outline: none;
  }

  .conv-block {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .conv-block.faded {
    opacity: 0.62;
  }

  .conv-block.cited {
    padding: 10px 12px;
    margin: 0 -12px;
    border-left: 2px solid var(--accent);
    background: color-mix(in oklab, var(--accent, #c4a46a) 5%, transparent);
    border-radius: 4px;
  }

  .bubble {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 8px 12px;
    border-radius: 10px;
    border: 1px solid var(--border-mid);
    max-width: 92%;
    word-wrap: break-word;
    white-space: pre-wrap;
    line-height: 1.55;
    font-size: 0.92rem;
  }

  .bubble.bubble-user {
    align-self: flex-end;
    background: var(--user-bubble, var(--bg-surface));
  }

  .bubble.bubble-assistant {
    align-self: flex-start;
    background: var(--bg-elevated, var(--bg-surface));
  }

  .bubble.bubble-system,
  .bubble.bubble-other {
    align-self: center;
    background: transparent;
    border-style: dashed;
    color: var(--text-secondary);
    font-size: 0.86rem;
  }

  .bubble .role {
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
    font-weight: 600;
  }

  .bubble .content {
    color: var(--text-primary);
  }

  /* A2 conv-tiered entity chip row */
  .conv-entity-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    align-items: center;
    padding: 0.5rem 0.2rem 0.6rem;
    border-bottom: 1px dashed var(--border, #333);
    margin-bottom: 0.4rem;
  }
  .entity-row-label {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
    margin-right: 0.3rem;
  }
  .entity-chip {
    background: var(--lavender-dim);
    color: var(--lavender-light);
    border: none;
    border-radius: var(--radius);
    padding: 0.15rem 0.6rem;
    font-size: 0.78rem;
    cursor: pointer;
    transition: background 120ms ease, color 120ms ease;
  }
  .entity-chip:hover {
    background: var(--lavender-glow);
    color: var(--text-primary);
  }
  .entity-chip.copied {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .scope-toggle {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.72rem;
    color: var(--text-muted);
    cursor: pointer;
  }
  .scope-toggle input {
    margin: 0;
  }
</style>
