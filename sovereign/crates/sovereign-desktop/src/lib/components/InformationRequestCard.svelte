<script lang="ts">
  import {
    submitInformationResponse,
    submitInformationSearch,
  } from "../api";
  import type {
    InformationRequestPayload,
    SearchAugmentation,
  } from "../types";

  interface Props {
    request: InformationRequestPayload | null;
    onHandled: () => void;
    /** Fired the moment the user kicks off a refinement (paste-submit
     *  or search-submit). ChatView uses this to mark the targeted
     *  assistant bubble as `refining: true` so AssistantMessage can
     *  render a "Refining…" overlay until the corresponding
     *  MESSAGE_REFINED event arrives. Optional — leaving it unset
     *  preserves the original no-indicator behaviour. */
    onRefiningStarted?: () => void;
    /** Fired after a successful `submitInformationSearch` so the
     *  ChatView can stash the search provenance on the targeted
     *  message. The post-refine bubble then renders the
     *  "Augmented via web search" footer. */
    onSearchAugmented?: (augmentation: SearchAugmentation) => void;
  }

  let {
    request,
    onHandled,
    onRefiningStarted,
    onSearchAugmented,
  }: Props = $props();

  let pasteValue = $state("");
  let submitting = $state(false);
  /// Reflects the loading state of the web-search affordance.
  /// Separate from `submitting` because a failed search leaves the
  /// card live (user can paste / skip / retry); we want the spinner
  /// off and the other buttons re-enabled while the gap text stays.
  let searching = $state(false);
  /// Surfaced inline when a search affordance fails (zero results,
  /// network error). Cleared on the next interaction.
  let searchError = $state("");

  async function handleSubmit() {
    if (!request || submitting || !pasteValue.trim()) return;
    submitting = true;
    // Mark the targeted bubble as refining BEFORE the Tauri call so
    // the UI swaps to the "Refining…" state immediately rather than
    // after the round-trip. The runtime's post-stream refinement
    // fires once the channel resolves, so the indicator covers the
    // entire wait.
    onRefiningStarted?.();
    try {
      await submitInformationResponse(request.key, pasteValue.trim());
    } catch (e) {
      console.error("Failed to submit information response:", e);
    }
    submitting = false;
    pasteValue = "";
    onHandled();
  }

  async function handleSkip() {
    if (!request || submitting) return;
    submitting = true;
    try {
      await submitInformationResponse(request.key, null);
    } catch (e) {
      console.error("Failed to submit skip:", e);
    }
    submitting = false;
    pasteValue = "";
    onHandled();
  }

  /// Run a web search against the gap text. On success the daemon
  /// resolves the same pending channel that `handleSubmit` would
  /// resolve, so the runtime gets identical re-synthesis input — no
  /// new wire path for the search result. On failure (zero results
  /// from a bot-blocked DDG fallback, or a backend error), surface
  /// the message inline and leave the card live.
  ///
  /// The returned `SearchAugmentation` is forwarded to ChatView so
  /// the post-refine bubble can render an "Augmented via web search:
  /// <query> (N sources)" footer with the source URLs clickable.
  async function handleSearch() {
    if (!request || submitting || searching) return;
    searchError = "";
    searching = true;
    // Same pre-call indicator-on as `handleSubmit` — the bubble
    // shows "Refining…" the moment the user commits to the action.
    onRefiningStarted?.();
    try {
      const augmentation = await submitInformationSearch(
        request.key,
        request.gap,
      );
      if (augmentation.accepted) {
        onSearchAugmented?.(augmentation);
      }
      // Daemon resolved the pending channel; tear down the card.
      onHandled();
    } catch (e) {
      // Tauri command-handler errors arrive as strings.
      searchError =
        typeof e === "string"
          ? e
          : (e as { message?: string })?.message || "Web search failed";
      console.error("Web search affordance failed:", e);
    }
    searching = false;
  }
</script>

{#if request}
  <div class="info-card">
    <div class="info-header">
      <span class="header-mark">◈</span>
      <span class="header-label">Information request</span>
    </div>

    {#if request.current_understanding}
      <section class="info-section">
        <div class="section-label">Current understanding</div>
        <p class="section-text serif">{request.current_understanding}</p>
      </section>
    {/if}

    <section class="info-section emphasis">
      <div class="section-label">What I need</div>
      <p class="section-text serif">{request.gap}</p>
    </section>

    {#if request.relevance}
      <section class="info-section">
        <div class="section-label">Why it matters</div>
        <p class="section-text serif">{request.relevance}</p>
      </section>
    {/if}

    {#if request.satisfying_source}
      <section class="info-section">
        <div class="section-label">What would satisfy this</div>
        <p class="section-text serif">{request.satisfying_source}</p>
      </section>
    {/if}

    {#if request.search_hints && request.search_hints.length > 0}
      <section class="info-section">
        <div class="section-label">Where to look</div>
        <ul class="hints">
          {#each request.search_hints as hint}
            <li>{hint}</li>
          {/each}
        </ul>
      </section>
    {/if}

    <section class="info-section input-section">
      <div class="section-label">Paste relevant content</div>
      <textarea
        bind:value={pasteValue}
        placeholder="Paste a passage, paragraph, or source here. The agent will integrate it with attribution."
        rows="6"
        disabled={submitting}
      ></textarea>
    </section>

    {#if searchError}
      <div class="search-error" role="alert">{searchError}</div>
    {/if}

    <div class="info-actions">
      <button
        class="btn skip"
        onclick={handleSkip}
        disabled={submitting || searching}
      >
        Skip — proceed with current knowledge
      </button>
      <button
        class="btn search"
        onclick={handleSearch}
        disabled={submitting || searching}
        title="Run a web search using the gap text and feed results back to the agent"
      >
        {searching ? "Searching…" : "Search the web"}
      </button>
      <button
        class="btn submit"
        onclick={handleSubmit}
        disabled={submitting || searching || !pasteValue.trim()}
      >
        Submit
      </button>
    </div>
  </div>
{/if}

<style>
  .info-card {
    background: var(--bg-secondary);
    border: 1px solid var(--accent);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-lg);
    margin-bottom: 12px;
    overflow: hidden;
    /* `.messages` is a `flex-direction: column` container. `overflow:
     * hidden` (above, needed so children's square borders get clipped
     * to the rounded corners) disables flex's `min-height: auto`
     * default, so without an explicit `flex-shrink: 0` the card
     * collapses to zero height whenever the messages above fill the
     * viewport — and `scrollToBottom()` then can't scroll to a card
     * whose scrollHeight reports 0. `flex-shrink: 0` keeps the card
     * at its natural content height; the outer `.messages` container
     * remains the scroll surface. */
    flex-shrink: 0;
  }

  .info-header {
    display: flex;
    align-items: center;
    gap: 8px;
    background: rgba(201, 168, 76, 0.08);
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
  }
  .header-mark {
    color: var(--accent);
    font-size: 0.95rem;
    line-height: 1;
  }
  .header-label {
    font-size: 0.78rem;
    font-weight: 700;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--accent);
  }

  .info-section {
    padding: 10px 16px;
    border-bottom: 1px dashed var(--border);
  }
  .info-section:last-of-type {
    border-bottom: none;
  }
  .info-section.emphasis {
    background: rgba(201, 168, 76, 0.04);
  }

  .section-label {
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--text-muted);
    margin-bottom: 4px;
  }

  .section-text {
    margin: 0;
    font-size: 0.92rem;
    line-height: 1.55;
    color: var(--text-primary, var(--text-secondary));
  }
  .section-text.serif {
    font-family: var(--font-serif, Georgia, serif);
  }

  .emphasis .section-text {
    font-weight: 500;
  }

  .hints {
    margin: 4px 0 0 0;
    padding-left: 18px;
    color: var(--text-secondary);
    font-size: 0.88rem;
  }
  .hints li {
    margin: 2px 0;
  }

  .input-section textarea {
    width: 100%;
    padding: 10px 12px;
    background: var(--bg-input, var(--bg-primary));
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-family: var(--font-sans);
    font-size: 0.9rem;
    line-height: 1.5;
    resize: vertical;
    outline: none;
  }
  .input-section textarea:focus {
    border-color: var(--accent);
  }

  .info-actions {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--border);
    background: rgba(0, 0, 0, 0.02);
  }

  .btn {
    padding: 7px 16px;
    border-radius: var(--radius);
    font-weight: 500;
    font-size: 0.88rem;
    transition: background 0.2s, border-color 0.2s;
    cursor: pointer;
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .skip {
    background: transparent;
    border: 1px solid var(--border-mid);
    color: var(--text-muted);
  }
  .skip:hover:not(:disabled) {
    color: var(--text-secondary);
    border-color: var(--border-bright);
  }

  .search {
    background: transparent;
    border: 1px solid var(--accent);
    color: var(--accent);
  }
  .search:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  .search-error {
    margin: 0 16px 8px;
    padding: 8px 12px;
    background: color-mix(in srgb, crimson 6%, transparent);
    border: 1px solid color-mix(in srgb, crimson 30%, transparent);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font-size: 0.8rem;
    line-height: 1.45;
  }

  .submit {
    background: var(--accent);
    color: var(--bg-primary);
    border: 1px solid var(--accent);
  }
  .submit:hover:not(:disabled) {
    background: var(--accent-bright, var(--accent));
  }
</style>
