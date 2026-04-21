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

<div class="prescan">
  {#if result.readable.length === 0}
    <div class="prescan-empty">
      <p class="heading">No readable documents found in this folder.</p>
      <p class="hint">
        Sovereign can read PDFs and text files.
        {#if result.ignored_types > 0}
          The files here are in formats Sovereign doesn't read yet.
        {/if}
      </p>
      <button class="btn-primary" onclick={onChooseAgain}>
        Choose a different folder
      </button>
    </div>
  {:else if allReadable}
    <div class="prescan-happy">
      <p class="heading">Found {result.readable.length} documents</p>
      <div class="type-breakdown">
        <span>PDFs &nbsp;&nbsp; {pdfCount}</span>
        <span>Text &nbsp;&nbsp; {txtCount}</span>
      </div>
      {#if largeCorpus}
        <p class="large-note">
          This is a large collection — indexing will continue in the background.
        </p>
      {/if}
      <button class="btn-primary" onclick={onConfirm}>Start indexing</button>
    </div>
  {:else}
    <div class="prescan-mixed">
      <p class="heading">Found {result.total_visited} files</p>

      <div class="will-index">
        Will index &nbsp; <strong>{result.readable.length}</strong> documents
      </div>

      <div class="skips">
        Will skip &nbsp; <strong>{skipTotal + result.ignored_types}</strong> files

        {#if result.scanned_pdfs.length > 0}
          <div class="skip-group">
            <span class="skip-reason">
              {result.scanned_pdfs.length} scanned PDFs (no text layer)
            </span>
            <NamedFileList files={result.scanned_pdfs} showUpTo={2} />
          </div>
        {/if}

        {#if result.protected_pdfs.length > 0}
          <div class="skip-group">
            <span class="skip-reason">
              {result.protected_pdfs.length} password-protected PDFs
            </span>
            <!-- Always named in full: spec §5.5. User needs to know which. -->
            <NamedFileList files={result.protected_pdfs} showUpTo={1000} />
          </div>
        {/if}

        {#if result.corrupt_files.length > 0}
          <div class="skip-group">
            <span class="skip-reason">
              {result.corrupt_files.length} files couldn't be read
            </span>
            <NamedFileList files={result.corrupt_files} showUpTo={1000} />
          </div>
        {/if}

        {#if result.ignored_types > 0}
          <div class="skip-group">
            <span class="skip-reason">
              {result.ignored_types} images and other unsupported files
            </span>
          </div>
        {/if}
      </div>

      {#if largeCorpus}
        <p class="large-note">
          This is a large collection — indexing will continue in the background.
        </p>
      {/if}

      <div class="actions">
        <button class="btn-secondary" onclick={onChooseAgain}>
          Choose different folder
        </button>
        <button class="btn-primary" onclick={onConfirm}>
          Index {result.readable.length} documents
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .prescan {
    padding: 16px 0;
  }
  .heading {
    font-size: 16px;
    font-weight: 500;
    margin: 0 0 12px;
  }
  .type-breakdown {
    display: flex;
    gap: 24px;
    color: var(--color-text-muted, #6b6b6b);
    font-size: 13px;
    margin-bottom: 16px;
  }
  .will-index,
  .skips {
    margin-bottom: 16px;
    font-size: 14px;
  }
  .skip-group {
    margin-top: 10px;
    padding-left: 12px;
    border-left: 2px solid var(--color-surface-subtle, #eee);
  }
  .skip-reason {
    display: block;
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin-bottom: 4px;
  }
  .large-note {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    font-style: italic;
    margin: 12px 0;
  }
  .hint {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin-bottom: 12px;
  }
  .actions {
    display: flex;
    gap: 12px;
    margin-top: 20px;
  }
  .btn-primary,
  .btn-secondary {
    padding: 8px 16px;
    border-radius: 6px;
    font-size: 14px;
    cursor: pointer;
    border: none;
  }
  .btn-primary {
    background: var(--color-accent, #3a5fc9);
    color: #fff;
  }
  .btn-primary:hover {
    background: var(--color-accent-hover, #2f4fb3);
  }
  .btn-secondary {
    background: transparent;
    color: var(--color-text, #1a1a1a);
    border: 1px solid var(--color-border, #d4d4d4);
  }
  .btn-secondary:hover {
    background: var(--color-surface-subtle, #f4f4f4);
  }
</style>
