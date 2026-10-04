<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { dialogFocus } from "@sovereign/chat-ui";
  import { lcWatchDocument } from "../../api";
  import type { WatchedFolderDocumentResponse } from "../../types";

  interface Props {
    corpusId: string;
    docId: string;
    onClose: () => void;
  }

  let { corpusId, docId, onClose }: Props = $props();

  let doc: WatchedFolderDocumentResponse | null = $state(null);
  let loadError: string | null = $state(null);
  let busy = $state(true);

  onMount(async () => {
    try {
      doc = await lcWatchDocument(corpusId, docId);
    } catch (e) {
      loadError = String(e);
    }
    busy = false;
  });

  function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024)
      return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  }

  function formatMtime(unix: number): string {
    if (unix === 0) return "—";
    return new Date(unix * 1000).toLocaleString();
  }

  function handleBackdropClick(event: MouseEvent) {
    if (event.target === event.currentTarget) onClose();
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions a11y_no_noninteractive_element_to_interactive_role -->
<div
  class="backdrop"
  onclick={handleBackdropClick}
  role="presentation"
>
  <div
    class="panel"
    role="dialog"
    aria-label="Document inspector"
    aria-modal="true"
    tabindex="-1"
    use:dialogFocus={{ onEscape: onClose }}
  >
    <header class="head">
      <h3 class="title">Document inspector</h3>
      <button class="close" onclick={onClose} aria-label="Close inspector">
        ×
      </button>
    </header>

    {#if busy}
      <p class="muted">Loading document detail…</p>
    {/if}
    {#if loadError}
      <p class="error">{loadError}</p>
    {/if}

    {#if doc}
      <div class="doc-id" title={doc.absolute_path}>
        <span class="doc-id-label">Path</span>
        <code class="doc-id-value">{doc.absolute_path}</code>
      </div>

      <dl class="meta">
        <dt>Size</dt>
        <dd>{formatSize(doc.size_bytes)}</dd>
        <dt>Modified</dt>
        <dd>{formatMtime(doc.mtime_unix)}</dd>
        <dt>Content hash</dt>
        <dd class="mono">{doc.content_hash}</dd>
        <dt>Passages</dt>
        <dd>
          {doc.chunk_count}
          {#if doc.chunk_count === 0}
            <span class="hint">— extraction pending or failed</span>
          {/if}
        </dd>
        <dt>Map elements</dt>
        <dd>
          {doc.atoms.length}
          {#if doc.atoms.length === 0}
            <span class="hint">— enable folder enrichment to populate</span>
          {/if}
        </dd>
      </dl>

      {#if doc.first_chunk_preview}
        <section class="preview-section">
          <h4 class="preview-title">First chunk preview</h4>
          <pre class="preview">{doc.first_chunk_preview}</pre>
          <p class="preview-foot">
            Showing the first chunk only. svrnmesh indexes the entire
            document; the preview is a sanity check on what extraction
            actually saw.
          </p>
        </section>
      {/if}

      {#if doc.atoms.length > 0}
        <section class="atoms">
          <h4 class="preview-title">Atom contributions</h4>
          <ul>
            {#each doc.atoms as atom (atom.atom_id)}
              <li>
                <span class="atom-type">{atom.atom_type}</span>
                <span class="atom-label">{atom.label}</span>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    justify-content: flex-end;
    z-index: 50;
    animation: backdrop-in 150ms ease-out both;
  }
  @keyframes backdrop-in {
    from { background: rgba(0, 0, 0, 0); }
    to   { background: rgba(0, 0, 0, 0.4); }
  }
  .panel {
    width: min(480px, 100%);
    height: 100%;
    overflow-y: auto;
    background: var(--lk-paper);
    border-left: 1px solid var(--lk-rule);
    padding: 24px 28px;
    box-shadow: -4px 0 24px rgba(0, 0, 0, 0.18);
    animation: panel-in 200ms ease-out both;
  }
  @keyframes panel-in {
    from { transform: translateX(20px); opacity: 0; }
    to   { transform: translateX(0); opacity: 1; }
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }
  .title {
    margin: 0;
    font-size: var(--lk-size-lead);
    color: var(--lk-ink);
    font-weight: 500;
  }
  .close {
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: 1px solid var(--lk-rule);
    border-radius: 4px;
    color: var(--lk-ink-soft);
    cursor: pointer;
    font-size: 18px;
    line-height: 1;
  }
  .close:hover { color: var(--lk-ink); border-color: var(--lk-ink-soft); }

  .doc-id {
    margin-bottom: 16px;
    padding: 10px 12px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .doc-id-label {
    display: block;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-bottom: 4px;
  }
  .doc-id-value {
    display: block;
    font-family: var(--lk-font-mono, monospace);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink);
    overflow-wrap: anywhere;
  }

  .meta {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 6px 16px;
    margin: 0 0 20px;
    font-size: var(--lk-size-meta);
  }
  .meta dt {
    color: var(--lk-ink-faded);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .meta dd {
    margin: 0;
    color: var(--lk-ink);
  }
  .meta dd.mono {
    font-family: var(--lk-font-mono, monospace);
    overflow-wrap: anywhere;
  }
  .hint {
    margin-left: 4px;
    color: var(--lk-ink-faded);
    font-style: italic;
  }

  .preview-section { margin-bottom: 16px; }
  .preview-title {
    margin: 0 0 8px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .preview {
    margin: 0;
    padding: 12px 14px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    font-family: var(--lk-font-mono, monospace);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 240px;
    overflow-y: auto;
  }
  .preview-foot {
    margin: 6px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }

  .atoms ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .atoms li {
    display: flex;
    gap: 8px;
    padding: 4px 8px;
    background: var(--lk-paper-deep);
    border-radius: 4px;
    font-size: var(--lk-size-meta);
  }
  .atom-type {
    color: var(--lk-ink-faded);
    font-family: var(--lk-font-mono, monospace);
    min-width: 80px;
  }
  .atom-label { color: var(--lk-ink); }

  .muted { color: var(--lk-ink-faded); font-size: var(--lk-size-meta); }
  .error {
    padding: 8px 12px;
    border-left: 3px solid var(--lk-err);
    background: var(--lk-err-wash);
    color: var(--lk-ink);
    font-size: var(--lk-size-meta);
  }
</style>
