<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // An opaque-bytes object referenced from the atom graph by content
  // hash — an attachment, a folder-walked binary, a calendar export.
  //
  // `asset_kind` is the dispatcher's self-identification and `mime` is
  // what the source claimed; both are shown because they disagree
  // often enough that collapsing them would hide the disagreement.
  import type { AssetData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: AssetData;
  }

  let { data }: Props = $props();

  /** Byte count in the unit a human reads it in. Binary units (1024)
   *  to match what a file manager reports for the same file. */
  function humanSize(bytes: number): string {
    if (!Number.isFinite(bytes) || bytes < 0) return String(bytes);
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let n = bytes;
    let u = 0;
    while (n >= 1024 && u < units.length - 1) {
      n /= 1024;
      u += 1;
    }
    return `${u === 0 ? n : n.toFixed(1)} ${units[u]}`;
  }
</script>

<div class="body">
  <p class="filename">
    {data.original_filename || `${data.asset_kind} asset`}
  </p>

  <dl class="fields">
    <dt>Kind</dt>
    <dd class="kind">{data.asset_kind}</dd>

    <dt>MIME</dt>
    <dd class="mono">{data.mime}</dd>

    <dt>Size</dt>
    <dd>{humanSize(data.size)}</dd>

    <dt>SHA-256</dt>
    <dd class="mono hash" title={data.sha256}>{data.sha256}</dd>

    {#if data.described_by}
      <dt>Described by</dt>
      <dd><AtomLink atomId={data.described_by} /></dd>
    {/if}

    {#if data.parsed_form}
      <dt>Parsed form</dt>
      <dd class="mono">{data.parsed_form}</dd>
    {/if}

    {#if data.first_seen_source_doc_id}
      <dt>First carried by</dt>
      <dd class="mono">{data.first_seen_source_doc_id}</dd>
    {/if}
  </dl>
</div>

<style>
  .body { display: flex; flex-direction: column; gap: 16px; }
  .filename { margin: 0; font-size: 1rem; }
  .fields {
    display: grid;
    grid-template-columns: 130px 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 0.85rem;
  }
  .fields dt { color: var(--text-muted); font-size: 0.78rem; letter-spacing: 0.02em; }
  .fields dd { margin: 0; }
  .kind { text-transform: capitalize; }
  .mono { font-family: var(--font-mono, monospace); font-size: 0.78rem; }
  .hash { overflow-wrap: anywhere; color: var(--text-secondary); }
</style>
