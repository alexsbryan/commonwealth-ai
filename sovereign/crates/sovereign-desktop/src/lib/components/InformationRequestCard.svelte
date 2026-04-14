<script lang="ts">
  import { submitInformationResponse } from "../api";
  import type { InformationRequestPayload } from "../types";

  interface Props {
    request: InformationRequestPayload | null;
    onHandled: () => void;
  }

  let { request, onHandled }: Props = $props();

  let pasteValue = $state("");
  let submitting = $state(false);

  async function handleSubmit() {
    if (!request || submitting || !pasteValue.trim()) return;
    submitting = true;
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

    <div class="info-actions">
      <button
        class="btn skip"
        onclick={handleSkip}
        disabled={submitting}
      >
        Skip — proceed with current knowledge
      </button>
      <button
        class="btn submit"
        onclick={handleSubmit}
        disabled={submitting || !pasteValue.trim()}
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

  .submit {
    background: var(--accent);
    color: var(--bg-primary);
    border: 1px solid var(--accent);
  }
  .submit:hover:not(:disabled) {
    background: var(--accent-bright, var(--accent));
  }
</style>
