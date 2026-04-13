<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { listDocumentAssets, uploadDocumentAsset } from "../api";
  import type { DocumentAsset, AssetState } from "../types";

  interface Props {
    onSelect: (asset: DocumentAsset) => void;
    onClose: () => void;
  }

  let { onSelect, onClose }: Props = $props();

  let assets: DocumentAsset[] = $state([]);
  let isUploading = $state(false);
  let uploadError: string | null = $state(null);

  onMount(async () => {
    try {
      assets = await listDocumentAssets();
    } catch {
      // No assets yet — that's fine.
    }
  });

  async function handleUpload() {
    const selected = await open({
      multiple: false,
      filters: [
        { name: "Documents", extensions: ["txt", "md", "pdf"] },
      ],
    });
    if (!selected) return;

    const filePath = typeof selected === "string" ? selected : String(selected);
    isUploading = true;
    uploadError = null;

    try {
      const { asset } = await uploadDocumentAsset(filePath);
      onSelect(asset);
    } catch (e) {
      uploadError = String(e);
      // Refresh list in case partial asset was created.
      try { assets = await listDocumentAssets(); } catch {}
    } finally {
      isUploading = false;
    }
  }

  function isReady(s: AssetState): boolean {
    return (
      s === "Ready" ||
      s === "PartiallyReady" ||
      (typeof s === "object" && "BuildingSkeleton" in s)
    );
  }

  function stateLabel(s: AssetState): string {
    if (s === "Ready") return "";
    if (s === "PartiallyReady") return "partial";
    if (s === "Pending") return "pending";
    if (typeof s === "object") {
      if ("Indexing" in s) return "indexing";
      if ("BuildingSkeleton" in s) return "building";
      if ("Failed" in s) return "failed";
    }
    return "";
  }

  function formatWords(n: number): string {
    if (n >= 1000) return `${(n / 1000).toFixed(0)}K`;
    return String(n);
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="picker-backdrop" onclick={onClose}>
  <div class="picker-popover" onclick={(e) => e.stopPropagation()}>
    <div class="picker-header">
      <span class="picker-title">Attach document</span>
    </div>

    <button class="picker-upload" onclick={handleUpload} disabled={isUploading}>
      {#if isUploading}
        <span class="upload-spinner"></span>
        <span>Uploading...</span>
      {:else}
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none">
          <path d="M8 1v10M4 5l4-4 4 4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
          <path d="M2 12v2h12v-2" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
        <span>Upload new document</span>
      {/if}
    </button>

    {#if uploadError}
      <div class="picker-error">{uploadError}</div>
    {/if}

    {#if assets.length > 0}
      <div class="picker-divider"></div>
      <div class="picker-label">Recent documents</div>
      <div class="picker-list">
        {#each assets as asset (asset.id)}
          <button
            class="picker-item"
            disabled={!isReady(asset.state)}
            onclick={() => onSelect(asset)}
          >
            <div class="item-title">{asset.title || asset.filename}</div>
            <div class="item-meta">
              {formatWords(asset.word_count)} words
              {#if asset.skeleton}
                &middot; {asset.skeleton.main_entities.length} entities
              {/if}
              {#if stateLabel(asset.state)}
                <span class="item-state">&middot; {stateLabel(asset.state)}</span>
              {/if}
            </div>
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .picker-backdrop {
    position: fixed;
    inset: 0;
    z-index: 100;
  }
  .picker-popover {
    position: absolute;
    bottom: 80px;
    left: 24px;
    width: 280px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
    padding: 8px 0;
    z-index: 101;
  }
  .picker-header {
    padding: 6px 14px 4px;
  }
  .picker-title {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .picker-upload {
    display: flex;
    align-items: center;
    gap: 8px;
    width: calc(100% - 16px);
    margin: 4px 8px;
    padding: 8px 10px;
    background: none;
    border: 1px dashed var(--border-mid);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font-size: 12px;
    cursor: pointer;
    text-align: left;
  }
  .picker-upload:hover:not(:disabled) {
    background: var(--bg-surface);
    border-color: var(--accent);
    color: var(--text-primary);
  }
  .picker-upload:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .upload-spinner {
    width: 12px;
    height: 12px;
    border: 2px solid var(--border-mid);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
  .picker-error {
    padding: 4px 14px;
    font-size: 11px;
    color: var(--error);
  }
  .picker-divider {
    height: 1px;
    background: var(--border);
    margin: 6px 0;
  }
  .picker-label {
    padding: 2px 14px 4px;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .picker-list {
    max-height: 200px;
    overflow-y: auto;
  }
  .picker-item {
    display: block;
    width: 100%;
    text-align: left;
    padding: 6px 14px;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-primary);
  }
  .picker-item:hover:not(:disabled) {
    background: var(--bg-surface);
  }
  .picker-item:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .item-title {
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .item-meta {
    font-size: 10px;
    color: var(--text-muted);
    margin-top: 1px;
  }
  .item-state {
    color: var(--accent);
  }
</style>
