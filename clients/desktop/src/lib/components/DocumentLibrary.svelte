<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import {
    listDocumentAssets,
    uploadDocumentAsset,
    deleteDocumentAsset,
  } from "../api";
  import { documentIngestionStore } from "../stores/documentIngestion.svelte";
  import type { DocumentAsset, AssetState } from "../types";

  interface Props {
    onOpen: (asset: DocumentAsset) => void;
  }

  let { onOpen }: Props = $props();

  let documents: DocumentAsset[] = $state([]);
  let unsubscribeTerminal: (() => void) | undefined;

  onMount(async () => {
    try {
      documents = await listDocumentAssets();
    } catch (e) {
      console.error("Failed to load documents:", e);
    }

    // One shared `document:progress` listener lives in the store.
    // We only need to know about terminal transitions here (to
    // refetch the server-side asset list + pick up any late metadata
    // the progress stream didn't carry).
    await documentIngestionStore.init();
    unsubscribeTerminal = documentIngestionStore.onTerminal(() => {
      listDocumentAssets()
        .then((docs) => (documents = docs))
        .catch(() => {});
    });
  });

  onDestroy(() => unsubscribeTerminal?.());

  async function handleAdd() {
    const selected = await open({
      multiple: false,
      filters: [
        { name: "Documents", extensions: ["txt", "md", "pdf"] },
      ],
    });
    if (!selected) return;
    const filePath = typeof selected === "string" ? selected : String(selected);

    // Optimistic: add a pending entry immediately.
    const tempId = crypto.randomUUID();
    const filename =
      filePath.split("/").pop() || filePath.split("\\").pop() || "document";
    documents = [
      {
        id: tempId,
        title: filename.replace(/\.[^.]+$/, "").replace(/[_-]/g, " "),
        filename,
        file_size_mb: 0,
        word_count: 0,
        chunk_count: 0,
        document_type: "Unknown",
        ingested_at: new Date().toISOString(),
        index_id: "",
        skeleton: null,
        state: "Pending",
      },
      ...documents,
    ];

    try {
      const { asset } = await uploadDocumentAsset(filePath);
      // Replace the temp entry with the real one.
      documents = documents.map((d) => (d.id === tempId ? asset : d));
    } catch (e) {
      // Mark the temp entry as failed.
      documents = documents.map((d) =>
        d.id === tempId
          ? { ...d, state: { Failed: { reason: String(e) } } as AssetState }
          : d,
      );
    }
  }

  async function handleDelete(id: string, event: Event) {
    event.stopPropagation();
    try {
      await deleteDocumentAsset(id);
      documents = documents.filter((d) => d.id !== id);
    } catch (e) {
      console.error("Failed to delete document:", e);
    }
  }

  function isQueryable(s: AssetState): boolean {
    return (
      s === "PartiallyReady" ||
      s === "MultiHopReady" ||
      s === "Ready" ||
      (typeof s === "object" && "BuildingSkeleton" in s)
    );
  }

  function stateLabel(s: AssetState): string {
    if (s === "Pending") return "Waiting";
    if (s === "PartiallyReady") return "Partially ready";
    if (s === "MultiHopReady") return "Multi-hop ready";
    if (s === "Ready") return "Ready";
    if (typeof s === "object") {
      if ("Indexing" in s) return "Indexing";
      if ("BuildingSkeleton" in s) return "Building structure";
      if ("Failed" in s) return "Failed";
    }
    return "";
  }

  function progressFraction(s: AssetState): number {
    if (typeof s === "object" && "Indexing" in s) {
      return s.Indexing.chunks_total > 0
        ? (s.Indexing.chunks_done / s.Indexing.chunks_total) * 0.5
        : 0;
    }
    if (s === "PartiallyReady") return 0.5;
    if (typeof s === "object" && "BuildingSkeleton" in s) {
      return s.BuildingSkeleton.chunks_total > 0
        ? 0.5 +
            (s.BuildingSkeleton.chunks_done / s.BuildingSkeleton.chunks_total) *
              0.5
        : 0.5;
    }
    if (s === "MultiHopReady") return 0.7;
    if (s === "Ready") return 1.0;
    return 0;
  }

  function formatWords(n: number): string {
    if (n >= 1000) return `${(n / 1000).toFixed(0)}K`;
    return String(n);
  }
</script>

<div class="document-library">
  <div class="library-header">
    <span class="library-label">Documents</span>
    <button class="add-btn" onclick={handleAdd} title="Upload document">
      <svg
        width="11"
        height="11"
        viewBox="0 0 11 11"
        fill="none"
        aria-hidden="true"
      >
        <path
          d="M5.5 1v9M1 5.5h9"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
        />
      </svg>
    </button>
  </div>

  {#if documents.length === 0}
    <button class="drop-hint" onclick={handleAdd}>
      <span class="drop-icon">{"\u25C8"}</span>
      <p>Add a document</p>
      <p class="drop-sub">PDF, Markdown, or plain text</p>
    </button>
  {:else}
    <div class="document-list">
      {#each documents as doc (doc.id)}
        <div
          class="document-row"
          class:is-ready={doc.state === "Ready"}
          class:is-processing={doc.state !== "Ready" &&
            !(typeof doc.state === "object" && "Failed" in doc.state)}
          role="button"
          tabindex="0"
          onclick={() => isQueryable(doc.state) && onOpen(doc)}
          onkeydown={(e) =>
            e.key === "Enter" && isQueryable(doc.state) && onOpen(doc)}
        >
          <div class="doc-row-body">
            <span class="doc-title">{doc.title || doc.filename}</span>

            {#if doc.state === "Ready"}
              <span class="doc-meta">
                {formatWords(doc.word_count)} words
                {#if doc.skeleton}
                  &middot; {doc.skeleton.main_entities.length} entities
                {/if}
              </span>
            {:else if typeof doc.state === "object" && "Failed" in doc.state}
              <span class="doc-meta doc-error"
                >Failed &mdash; {doc.state.Failed.reason}</span
              >
            {:else}
              <div class="doc-progress">
                <div class="progress-bar">
                  <div
                    class="progress-fill"
                    style="width: {progressFraction(doc.state) * 100}%"
                  ></div>
                </div>
                <span class="progress-label">
                  {doc.state === "PartiallyReady"
                    ? "Questions available"
                    : stateLabel(doc.state)}
                </span>
              </div>
            {/if}
          </div>

          <button
            class="delete-btn"
            onclick={(e) => handleDelete(doc.id, e)}
            title="Delete"
          >
            <svg
              width="10"
              height="10"
              viewBox="0 0 10 10"
              fill="none"
              aria-hidden="true"
            >
              <path
                d="M1 1l8 8M9 1L1 9"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
              />
            </svg>
          </button>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .document-library {
    padding: 8px 0;
    min-height: 60px;
    border-top: 1px solid var(--border);
  }
  .library-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0 16px 6px;
  }
  .library-label {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .add-btn {
    color: var(--text-muted);
    background: none;
    border: none;
    cursor: pointer;
    padding: 2px 4px;
    line-height: 1;
    border-radius: 3px;
  }
  .add-btn:hover {
    color: var(--text-secondary);
    background: var(--bg-elevated);
  }
  .drop-hint {
    display: block;
    width: 100%;
    padding: 16px;
    text-align: center;
    color: var(--text-muted);
    background: none;
    border: none;
    cursor: pointer;
  }
  .drop-hint:hover {
    background: var(--bg-elevated);
  }
  .drop-icon {
    font-size: 20px;
    color: var(--accent);
  }
  .drop-hint p {
    margin: 4px 0;
    font-size: 12px;
  }
  .drop-sub {
    font-size: 11px;
  }
  .document-list {
    display: flex;
    flex-direction: column;
  }
  .document-row {
    display: flex;
    align-items: center;
    padding: 6px 16px;
    cursor: pointer;
    gap: 6px;
  }
  .document-row:hover {
    background: var(--bg-elevated);
  }
  .document-row.is-processing {
    opacity: 0.7;
  }
  .doc-row-body {
    flex: 1;
    min-width: 0;
  }
  .doc-title {
    display: block;
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-primary);
  }
  .doc-meta {
    font-size: 11px;
    color: var(--text-muted);
    margin-top: 2px;
  }
  .doc-error {
    color: var(--error);
  }
  .doc-progress {
    margin-top: 4px;
  }
  .progress-bar {
    height: 2px;
    background: var(--border);
    border-radius: 1px;
    overflow: hidden;
  }
  .progress-fill {
    height: 100%;
    background: var(--accent);
    transition: width 1s ease;
  }
  .progress-label {
    display: block;
    font-size: 10px;
    color: var(--text-muted);
    margin-top: 2px;
  }
  .delete-btn {
    flex-shrink: 0;
    color: var(--text-muted);
    background: none;
    border: none;
    cursor: pointer;
    opacity: 0;
    padding: 2px;
    border-radius: 3px;
  }
  .document-row:hover .delete-btn {
    opacity: 1;
  }
  .delete-btn:hover {
    color: var(--error);
    background: var(--bg-elevated);
  }
</style>
