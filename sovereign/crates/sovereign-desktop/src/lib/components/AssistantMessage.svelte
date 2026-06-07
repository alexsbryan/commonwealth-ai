<script lang="ts">
  import {
    parseAssistantContent,
    ThinkBlock,
    RoutingMeta,
    SourceAttribution,
    SourcePopover,
    NextStepButtons,
  } from "@sovereign/chat-ui";
  import { renderMarkdown } from "../utils/markdown";
  import { insightStore } from "../stores/insights.svelte";
  import { clipInsight } from "../api";
  import { readingSession } from "../stores/readingSession.svelte";
  import type {
    InsightSource,
    NextStepOffer,
    SearchAugmentation,
  } from "../types";

  interface Props {
    content: string;
    messageId: string;
    conversationId: string;
    metadata?: Record<string, unknown>;
    isStreaming?: boolean;
    /** True while the user has triggered a post-stream refinement
     *  (paste-submit or web-search) but the runtime hasn't yet
     *  emitted MESSAGE_REFINED. Drives the "Refining…" overlay so
     *  the in-place rewrite is not a surprise flash. */
    refining?: boolean;
    /** Set on bubbles whose refinement came from the search-now
     *  affordance — drives the "Augmented via web search" footer
     *  with clickable source URLs. */
    searchAugmentation?: SearchAugmentation;
    onNextStep?: (offer: NextStepOffer) => void;
    /** Fired when the user clicks "Continue from here" on the cutoff
     *  chip — meaning provenance.finish_reason was "length" and the
     *  prior reply ended mid-thought. ChatView resends as a fresh
     *  turn instructing the model to pick up from the cutoff. */
    onContinue?: () => void;
  }

  let {
    content,
    messageId,
    conversationId,
    metadata,
    isStreaming,
    refining,
    searchAugmentation,
    onNextStep,
    onContinue,
  }: Props = $props();

  // rAF-coalesce parse work. `parseAssistantContent` is O(n) over
  // the full growing message string and is read transitively by
  // `blocks`, `thinkBlocks`, `proseText` and `proseHtml`. At fast
  // token rates several chunks can land in distinct microtasks
  // within a single frame; only the latest one matters for the
  // pixel that will be painted. We mirror `content` into
  // `renderContent` on the next animation frame so the parse runs
  // at most once per frame regardless of inbound chunk cadence.
  //
  // Streaming-complete safety: when `isStreaming` flips false the
  // effect bypasses the coalesce and assigns synchronously so the
  // formatted markdown branch reads the FINAL text in the same
  // frame as the {#if isStreaming} swap above — otherwise the
  // formatted prose would pop in one frame late.
  let renderContent: string = $state("");
  let coalesceScheduled = false;
  // $effect.pre runs synchronously before DOM updates, so the first
  // mount paints with `renderContent === content` (not the empty
  // initial). Subsequent re-runs track `content` reactively.
  $effect.pre(() => {
    const next = content;
    if (!isStreaming) {
      renderContent = next;
      return;
    }
    if (coalesceScheduled) return;
    coalesceScheduled = true;
    requestAnimationFrame(() => {
      renderContent = content;
      coalesceScheduled = false;
    });
  });
  let blocks = $derived(parseAssistantContent(renderContent));

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
          sources?: {
            origin: string;
            count: number;
            from_peer?: string;
            display_name?: string;
          }[];
          inference_backend: string;
          oicp_match?: string;
          total_latency_ms: number;
          tokens_used: number;
          coarse_intent?: string;
          self_assessment?: string;
          // Folder-ingest v1 §6.3 — per-turn coverage assessment.
          // `kind === "thin"` means at least one folder corpus
          // returned fewer than `thin_threshold` chunks; the chip
          // enumerates them so the user sees the gap immediately.
          coverage?: {
            kind: string;
            thin_threshold: number;
            thin_folders: Array<{
              corpus_id: string;
              display_name: string;
              chunks: number;
              skipped_files: number;
              failed_files: number;
            }>;
          };
          // Why the streaming generation stopped. "length" means the
          // model was cut off at `max_tokens_budget` mid-thought —
          // surfaced as the cutoff chip so the user can act (raise
          // budget, continue, or retry) instead of guessing.
          finish_reason?: string;
          max_tokens_budget?: number;
          completion_tokens?: number;
        }
      | undefined,
  );

  // Cutoff chip — fires only when the runtime tagged the stream as
  // length-truncated. `tokens_used` (an estimate today; see
  // `runtime.rs` for the chars-per-token heuristic) gives a rough
  // sense of where the budget ran out so the user can decide
  // whether to raise it or just ask for a continuation.
  let cutoffInfo = $derived.by(() => {
    if (provenance?.finish_reason !== "length") return null;
    return {
      budget: provenance.max_tokens_budget ?? null,
      used: provenance.completion_tokens ?? null,
    };
  });

  // Compose the chip text from the provenance coverage payload.
  // Empty string when no coverage note is attached, which hides
  // the chip entirely.
  let coverageChip = $derived.by(() => {
    const cov = provenance?.coverage;
    if (!cov || cov.kind !== "thin" || cov.thin_folders.length === 0) {
      return "";
    }
    const top = cov.thin_folders.slice(0, 2);
    const phrases = top.map((f) => {
      const bits: string[] = [`${f.chunks} hit${f.chunks === 1 ? "" : "s"}`];
      if (f.skipped_files > 0) {
        bits.push(`${f.skipped_files} unsupported`);
      }
      if (f.failed_files > 0) {
        bits.push(`${f.failed_files} extraction-failed`);
      }
      return `your "${f.display_name}" folder (${bits.join(", ")})`;
    });
    return `Thin coverage: ${phrases.join("; ")}.`;
  });

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
      // PPR provenance (A3-lite, spec CONV_TIERED_PORT.md). Carries
      // `ppr_seed` (entity that diffused mass to this chunk) and
      // `ppr_mass_norm` (normalised blended mass in [0,1]) when the
      // conv-tiered PPR rerank touched this chunk. Other metadata
      // keys may appear in the future — keep the shape open.
      metadata?: Record<string, string>;
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

  async function handleCodeCopy(btn: HTMLButtonElement) {
    const encoded = btn.dataset.code;
    if (!encoded) return;
    try {
      const bin = atob(encoded);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      const text = new TextDecoder().decode(bytes);
      await navigator.clipboard.writeText(text);
      const original = btn.textContent ?? "Copy";
      btn.textContent = "Copied";
      btn.classList.add("copied");
      setTimeout(() => {
        if (btn.isConnected) {
          btn.textContent = original;
          btn.classList.remove("copied");
        }
      }, 1500);
    } catch (err) {
      console.error("Failed to copy code:", err);
    }
  }

  // Message-level copy — the whole answer, not just a code block.
  // Copies `proseText` (the prose with `<think>` reasoning stripped),
  // mirroring the per-block clipboard pattern above. Markdown source is
  // what's copied (readable + structure-preserving), consistent with
  // how the code-block button copies raw text.
  let copyLabel = $state("Copy");
  async function handleMessageCopy() {
    if (!proseText) return;
    try {
      await navigator.clipboard.writeText(proseText);
      copyLabel = "Copied";
      setTimeout(() => {
        copyLabel = "Copy";
      }, 1500);
    } catch (err) {
      console.error("Failed to copy message:", err);
    }
  }

  function handleProseClick(e: MouseEvent) {
    const target = e.target as HTMLElement;

    // Code-block copy button — handled by event delegation so
    // streaming-coalesced re-renders don't need per-block listeners.
    const copyBtn = target.closest<HTMLButtonElement>(".code-block-copy");
    if (copyBtn) {
      e.preventDefault();
      void handleCodeCopy(copyBtn);
      return;
    }

    if (!target.classList.contains("source-citation")) return;

    // Numeric-citation fallback. The prompt forbids `[N]` (see
    // KNOWLEDGE_SYNTHESIS_SYSTEM in runtime.rs) but smaller models
    // sometimes emit it anyway. The chip lets the reader still get
    // *somewhere* useful: map N to retrievedChunks[N-1] when in
    // range, otherwise surface the "unresolved" toast so the
    // failure is legible instead of silently picking the wrong
    // chunk.
    const numericIdxAttr = target.getAttribute("data-citation-index");
    if (numericIdxAttr) {
      const idx = parseInt(numericIdxAttr, 10);
      const numChunk =
        Number.isFinite(idx) && idx >= 1 && idx <= retrievedChunks.length
          ? retrievedChunks[idx - 1]
          : null;
      if (!numChunk) {
        showUnresolved(`[${idx}]`);
        return;
      }
      if (numChunk.chunk_id != null && numChunk.corpus_id) {
        void readingSession.openCitation(
          numChunk.corpus_id,
          numChunk.chunk_id,
          originLabel,
        );
        return;
      }
      const rect = target.getBoundingClientRect();
      popoverAnchor = { x: rect.left, y: rect.bottom };
      popoverChunk = numChunk;
      return;
    }

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

<div
  class="sv-ai-msg"
  class:redirected={redirectedAway}
  class:refining
  data-message-id={messageId}
>
  <div class="role-label">
    &#x25C8; SOVEREIGN
    {#if redirectedAway}
      <span class="redirected-note">• redirected to a different approach</span>
    {/if}
    {#if refining}
      <span class="refining-note">
        <span class="pulse-dot" aria-hidden="true">◌</span>
        Refining with new information…
      </span>
    {/if}
  </div>

  <RoutingMeta {provenance} {retrievedChunks} />

  {#if coverageChip}
    <div class="coverage-chip" role="note">
      <span class="dot" aria-hidden="true">◐</span>
      {coverageChip}
    </div>
  {/if}

  {#each thinkBlocks as block}
    <ThinkBlock content={block.text} />
  {/each}

  {#if proseText}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="sv-prose"
      class:prose-refining={refining}
      onclick={handleProseClick}
    >
      {#if isStreaming}
        <!-- During streaming we render plain text into a stable div so
             the prose subtree is not torn down and re-mounted per word.
             That tear-down + the `transform: translateY` fade animation
             below were the cause of the streaming jank: every word
             flipped the text into a GPU compositor layer, disabling
             subpixel antialiasing and re-running marked.parse over the
             full growing message. Plain text during the stream → the
             formatted markdown swaps in once on completion via the
             {#key content} block below. -->
        <div class="prose-streaming">{proseText}</div>
      {:else}
        {#key content}
          <div class="prose-content-fade">
            {@html proseHtml}
          </div>
        {/key}
      {/if}
    </div>
  {/if}

  {#if searchAugmentation}
    <div class="search-augmentation" class:aug-refining={refining} role="note">
      <div class="aug-header">
        <span class="aug-mark" aria-hidden="true">⌕</span>
        <span class="aug-label">
          Augmented via web search ({searchAugmentation.backend_id})
        </span>
      </div>
      <div class="aug-query">"{searchAugmentation.query}"</div>
      {#if refining}
        <!-- Active-work indicator: search returned, model is now
             folding the results into the new answer. Cleared on
             MESSAGE_REFINED. The aria-live region announces the
             transition for screen readers without forcing focus. -->
        <div class="aug-refining-status" aria-live="polite">
          <span class="aug-spinner" aria-hidden="true">◌</span>
          Refining your answer with these {searchAugmentation.sources.length}
          source{searchAugmentation.sources.length === 1 ? "" : "s"}…
        </div>
      {/if}
      <ul class="aug-sources">
        {#each searchAugmentation.sources as src}
          <li>
            <a href={src.url} target="_blank" rel="noopener noreferrer">
              {src.title || src.url}
            </a>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  <SourceAttribution {content} {retrievedChunks} />

  {#if cutoffInfo && !isStreaming}
    <div class="cutoff-chip" role="note">
      <div class="cutoff-line">
        <span class="cutoff-mark" aria-hidden="true">⊣</span>
        <span class="cutoff-text">
          Response was cut off mid-thought
          {#if cutoffInfo.budget}
            — hit the {cutoffInfo.budget.toLocaleString()}-token limit
            {#if cutoffInfo.used}
              (~{cutoffInfo.used.toLocaleString()} generated)
            {/if}
          {/if}.
        </span>
      </div>
      <div class="cutoff-actions">
        {#if onContinue}
          <button
            type="button"
            class="cutoff-btn primary"
            onclick={() => onContinue?.()}
          >
            Continue from here
          </button>
        {/if}
        <span class="cutoff-hint">
          To get longer answers, raise the response length in
          Settings → Models.
        </span>
      </div>
    </div>
  {/if}

  {#if showNextSteps}
    <NextStepButtons
      offers={nextStepOffers}
      onselect={(offer) => onNextStep?.(offer)}
    />
  {/if}

  {#if proseText && !isStreaming}
    <div class="message-actions">
      <button
        type="button"
        class="msg-action-btn"
        onclick={handleMessageCopy}
        title="Copy this answer to the clipboard"
      >
        {copyLabel}
      </button>
    </div>
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
  /* Assistant messages used to carry a 2px lavender border-left and
     a 14px gutter to keep prose off the rail. The role-label row
     ("◈ SOVEREIGN") already announces who's speaking, so the rail
     was redundant chrome that ate ~16px of horizontal real estate
     on every message. Dropping it lets the prose breathe to the
     full conversation width. Per-state signals that used to ride
     the border (refining tint, redirected dashed) move to body
     opacity / header pills instead. */
  .sv-ai-msg {
    align-self: stretch;
    padding: 0;
    max-width: 100%;
    margin-bottom: 18px;
  }

  .sv-ai-msg.redirected {
    opacity: 0.55;
  }

  .redirected-note {
    margin-left: 8px;
    font-weight: 400;
    color: var(--text-muted);
    letter-spacing: 0.06em;
    text-transform: none;
  }

  /* "Refining…" indicator — shown on the role-label row while the
     post-stream refinement is in flight (user clicked search-now or
     submitted paste content on the InformationRequestCard). The
     pulse on the dot signals motion without competing with the
     prose for attention. */
  .refining-note {
    margin-left: 8px;
    font-weight: 400;
    color: var(--accent, #c9a84c);
    letter-spacing: 0.06em;
    text-transform: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 0.78em;
  }

  .pulse-dot {
    display: inline-block;
    animation: refining-pulse 1.3s ease-in-out infinite;
  }

  @keyframes refining-pulse {
    0%, 100% { opacity: 0.35; transform: scale(0.9); }
    50%      { opacity: 1;    transform: scale(1.05); }
  }

  /* `.sv-ai-msg.refining` carries no additional styling: the
     refining signal lives in the `.refining-note` pill (pulsing
     dot in the header) + `.prose-refining` body fade below. Class
     hook stays via the template so a future surface can layer
     state without re-introducing the lavender rail. */

  .prose-refining {
    opacity: 0.55;
    filter: saturate(0.6);
    transition: opacity 0.3s ease, filter 0.3s ease;
  }

  /* Fade-in for refined content — fires every time {#key content}
     remounts the inner div, including the post-refine swap. Only
     reached on the non-streaming branch in the template above, so
     the translateY transform never engages mid-stream (which is
     what was disabling subpixel antialiasing per word). */
  .prose-content-fade {
    animation: refine-fade-in 0.45s ease-out;
  }

  /* Streaming branch — no animation, no transform, stable subtree.
     Text content is updated in place via Svelte's reactive update
     so the WebView keeps the layer on the document plane (subpixel
     AA preserved) instead of promoting it to a GPU compositor
     layer per word. Inherits typography from `.sv-prose`. */
  .prose-streaming {
    white-space: pre-wrap;
    word-wrap: break-word;
  }

  @keyframes refine-fade-in {
    from { opacity: 0; transform: translateY(2px); }
    to   { opacity: 1; transform: translateY(0); }
  }

  /* Augmentation footer — appears under the prose when the
     refinement was sourced from the search-now affordance. Shows
     the operator-chosen backend, the query that was issued, and
     the source URLs that fed the re-synthesis. Persistent: stays
     on the bubble for the lifetime of the conversation so a later
     reader can tell which answers were web-augmented. */
  .search-augmentation {
    margin-top: 12px;
    padding: 10px 14px;
    background: color-mix(in srgb, var(--accent, #c9a84c) 5%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent, #c9a84c) 25%, transparent);
    border-radius: var(--radius);
    font-size: 0.78rem;
    line-height: 1.5;
  }
  /* While the post-search refinement is in flight, the whole block
     pulses subtly via box-shadow so the user can tell the model is
     actively working on the answer (not just "search done, here
     are the links"). Pairs with the inline `aug-refining-status`
     row that names what's happening. */
  .search-augmentation.aug-refining {
    animation: aug-block-pulse 1.6s ease-in-out infinite;
  }
  @keyframes aug-block-pulse {
    0%, 100% {
      box-shadow: 0 0 0 0 color-mix(in srgb, var(--accent, #c9a84c) 0%, transparent);
    }
    50% {
      box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent, #c9a84c) 18%, transparent);
    }
  }
  .search-augmentation .aug-header {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--accent, #c9a84c);
    font-weight: 600;
    letter-spacing: 0.04em;
    margin-bottom: 4px;
  }
  .search-augmentation .aug-refining-status {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 6px 0 8px;
    padding: 4px 8px;
    background: color-mix(in srgb, var(--accent, #c9a84c) 8%, transparent);
    border-radius: 4px;
    color: var(--text-secondary);
    font-size: 0.78em;
    font-style: italic;
  }
  .search-augmentation .aug-spinner {
    color: var(--accent, #c9a84c);
    display: inline-block;
    animation: refining-pulse 1.3s ease-in-out infinite;
  }
  .search-augmentation .aug-mark {
    font-size: 0.95em;
  }
  .search-augmentation .aug-query {
    color: var(--text-secondary);
    font-style: italic;
    margin-bottom: 6px;
  }
  .search-augmentation .aug-sources {
    margin: 0;
    padding-left: 18px;
    color: var(--text-secondary);
  }
  .search-augmentation .aug-sources li {
    margin: 2px 0;
  }
  .search-augmentation .aug-sources a {
    color: var(--accent, #c9a84c);
    text-decoration: none;
    border-bottom: 1px dotted color-mix(in srgb, var(--accent, #c9a84c) 50%, transparent);
  }
  .search-augmentation .aug-sources a:hover {
    border-bottom-style: solid;
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

  /* Coverage chip — quietly announces when the user's folder
     corpora returned fewer than `thin_threshold` chunks for this
     turn. Sits above the prose so the gap is legible before the
     answer reads. Folder-ingest v1 §6.3. */
  .coverage-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 9px;
    margin-bottom: 8px;
    font-size: 0.7rem;
    color: var(--text-muted);
    background: var(--bg-surface);
    border: 0.5px solid var(--border-mid);
    border-radius: 100px;
    font-family: var(--font-sans);
    line-height: 1.45;
  }

  .coverage-chip .dot {
    color: var(--accent, #c9a84c);
    font-size: 0.85em;
  }

  /* Cutoff chip — surfaces length-truncation honestly instead of
     leaving the user staring at a sentence that ended mid-clause.
     Two affordances: the Continue button re-prompts the model to
     resume from where it stopped, the hint points at the budget
     setting so the next answer can fit without trimming. */
  .cutoff-chip {
    margin-top: 12px;
    padding: 10px 14px;
    border: 1px solid color-mix(in srgb, var(--warning, #c9a84c) 35%, transparent);
    background: color-mix(in srgb, var(--warning, #c9a84c) 6%, transparent);
    border-radius: var(--radius);
    font-size: 0.82rem;
    line-height: 1.5;
  }
  .cutoff-chip .cutoff-line {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    color: var(--text-primary);
    margin-bottom: 8px;
  }
  .cutoff-chip .cutoff-mark {
    color: var(--warning, #c9a84c);
    font-size: 1.05em;
    line-height: 1.4;
  }
  .cutoff-chip .cutoff-text {
    flex: 1;
  }
  .cutoff-chip .cutoff-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
  }
  .cutoff-btn {
    padding: 5px 12px;
    border-radius: 4px;
    border: 1px solid var(--accent, #c9a84c);
    background: transparent;
    color: var(--accent, #c9a84c);
    font: inherit;
    font-size: 0.82rem;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .cutoff-btn.primary {
    background: var(--accent, #c9a84c);
    color: var(--bg-surface, #1a1a1a);
  }
  .cutoff-btn:hover {
    background: color-mix(in srgb, var(--accent, #c9a84c) 80%, white);
  }
  .cutoff-hint {
    color: var(--text-muted);
    font-size: 0.76rem;
  }

  /* Message-level action row (Copy). Quiet by default — a muted,
     monospace affordance that brightens on hover so it never competes
     with the answer, matching the restraint of the RoutingMeta chips. */
  .message-actions {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
  .msg-action-btn {
    padding: 2px 10px;
    border: 0.5px solid var(--border-mid);
    border-radius: 100px;
    background: transparent;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.65rem;
    letter-spacing: 0.02em;
    cursor: pointer;
    transition:
      color 0.15s,
      border-color 0.15s;
  }
  .msg-action-btn:hover {
    color: var(--text-primary);
    border-color: var(--border-bright);
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
