<script lang="ts">
  import type { PreScanResult } from "../../../types";
  import NamedFileList from "./NamedFileList.svelte";

  interface Props {
    result: PreScanResult;
    onConfirm: () => void;
    onChooseAgain: () => void;
  }

  let { result, onConfirm, onChooseAgain }: Props = $props();

  let allReadable = $derived(
    result.scanned_pdfs.length === 0 &&
      result.protected_pdfs.length === 0 &&
      result.corrupt_files.length === 0 &&
      result.ignored_types === 0,
  );

  let skipTotal = $derived(
    result.scanned_pdfs.length +
      result.protected_pdfs.length +
      result.corrupt_files.length,
  );

  let pdfCount = $derived(
    result.readable.filter((f) => f.path.toLowerCase().endsWith(".pdf")).length,
  );
  let txtCount = $derived(result.readable.length - pdfCount);

  let largeCorpus = $derived(result.readable.length >= 500);
</script>

<section class="prescan">
  {#if result.readable.length === 0}
    <header class="head">
      <h2 class="title">No readable documents</h2>
      <p class="lede">
        Sovereign reads PDFs and text files.
        {#if result.ignored_types > 0}
          The files here are in formats it doesn't yet handle.
        {/if}
      </p>
    </header>
    <div class="actions">
      <button class="lk-btn lk-btn--mark" onclick={onChooseAgain}>
        Pick a different folder
      </button>
    </div>
  {:else if allReadable}
    <header class="head">
      <h2 class="title">
        <span class="lk-num">{result.readable.length}</span>
        {result.readable.length === 1 ? "document" : "documents"} ready
      </h2>
    </header>
    <dl class="breakdown">
      <div>
        <dt>PDFs</dt>
        <dd class="lk-num">{pdfCount}</dd>
      </div>
      <div>
        <dt>Text</dt>
        <dd class="lk-num">{txtCount}</dd>
      </div>
    </dl>
    {#if largeCorpus}
      <p class="note">Large collection — indexing continues in the background.</p>
    {/if}
    <div class="actions">
      <button class="lk-btn lk-btn--mark" onclick={onConfirm}>Index</button>
    </div>
  {:else}
    <header class="head">
      <h2 class="title">
        <span class="lk-num">{result.total_visited}</span>
        files scanned
      </h2>
    </header>

    <div class="split">
      <section class="will-idx">
        <p class="lk-label will-label">Will be indexed</p>
        <p class="big">
          <span class="lk-num">{result.readable.length}</span>
          <span class="big-unit">documents</span>
        </p>
      </section>

      <section class="will-skip">
        <p class="lk-label skip-label">Will be skipped</p>
        <p class="big">
          <span class="lk-num">{skipTotal + result.ignored_types}</span>
          <span class="big-unit">files</span>
        </p>

        {#if result.scanned_pdfs.length > 0}
          <div class="skip-group">
            <p class="skip-reason">
              {result.scanned_pdfs.length} scanned PDFs (no text layer)
            </p>
            <NamedFileList files={result.scanned_pdfs} showUpTo={2} />
          </div>
        {/if}

        {#if result.protected_pdfs.length > 0}
          <div class="skip-group">
            <p class="skip-reason">
              {result.protected_pdfs.length} password-protected PDFs
            </p>
            <NamedFileList files={result.protected_pdfs} showUpTo={1000} />
          </div>
        {/if}

        {#if result.corrupt_files.length > 0}
          <div class="skip-group">
            <p class="skip-reason">
              {result.corrupt_files.length} files couldn't be read
            </p>
            <NamedFileList files={result.corrupt_files} showUpTo={1000} />
          </div>
        {/if}

        {#if result.ignored_types > 0}
          <div class="skip-group">
            <p class="skip-reason">
              {result.ignored_types} unsupported files
            </p>
          </div>
        {/if}
      </section>
    </div>

    {#if largeCorpus}
      <p class="note">Large collection — indexing continues in the background.</p>
    {/if}

    <div class="actions">
      <button class="lk-btn lk-btn--quiet" onclick={onChooseAgain}>
        Pick different folder
      </button>
      <button class="lk-btn lk-btn--mark" onclick={onConfirm}>
        Index {result.readable.length} documents
      </button>
    </div>
  {/if}
</section>

<style>
  .prescan {
    padding: 4px 0;
    animation: lk-fade-in 260ms ease-out both;
  }
  .head { margin-bottom: 16px; }
  .title {
    margin: 0 0 4px;
    font-size: var(--lk-size-hero);
    font-weight: 600;
    line-height: 1.1;
    letter-spacing: -0.02em;
    color: var(--lk-ink);
  }
  .title .lk-num {
    color: var(--lk-stamp-ink);
    margin-right: 6px;
  }
  .lede {
    margin: 0;
    font-size: var(--lk-size-body);
    color: var(--lk-ink-soft);
    max-width: 58ch;
  }

  .breakdown {
    display: flex;
    gap: 40px;
    margin: 20px 0;
    padding: 14px 16px;
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    background: var(--lk-paper-deep);
  }
  .breakdown div {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .breakdown dt {
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .breakdown dd {
    margin: 0;
    font-size: 1.75rem;
    color: var(--lk-ink);
  }

  .split {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 20px;
    margin-bottom: 16px;
  }
  .will-idx,
  .will-skip {
    padding: 14px 16px;
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    background: var(--lk-paper-subtle);
    min-width: 0;
  }
  .will-label { color: var(--lk-stamp-ink); }
  .skip-label { color: var(--lk-ink-faded); }

  .big {
    margin: 6px 0 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .big .lk-num {
    font-size: 2.25rem;
    color: var(--lk-ink);
  }
  .big-unit {
    font-size: var(--lk-size-body);
    color: var(--lk-ink-soft);
  }

  .skip-group {
    margin-top: 12px;
    padding-top: 10px;
    border-top: 1px solid var(--lk-rule-soft);
  }
  .skip-reason {
    margin: 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink);
  }

  .note {
    margin: 10px 0;
    padding: 10px 12px;
    background: var(--lk-paper-deep);
    border-left: 2px solid var(--lk-stamp);
    border-radius: var(--radius);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }

  .actions {
    display: flex;
    gap: 10px;
    justify-content: flex-end;
    margin-top: 20px;
  }
</style>
