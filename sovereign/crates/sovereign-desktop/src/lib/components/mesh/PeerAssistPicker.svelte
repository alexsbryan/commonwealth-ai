<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Peer picker for a peer-assisted ingest. Eligible peers are selectable
  checkboxes (checked by default); ineligible peers render dimmed with the
  reason they can't help — glassbox, never a silent omission.
-->
<script lang="ts">
  import type { AssistEligiblePeer } from "../../types";
  import { ineligibleReasonCopy } from "./assistFormat";

  interface Props {
    peers: AssistEligiblePeer[];
    /** Selected node_ids (owned by the parent offer). */
    selected: string[];
    onToggle: (nodeId: string) => void;
    onSelectAll: () => void;
    onClear: () => void;
  }

  let { peers, selected, onToggle, onSelectAll, onClear }: Props = $props();

  let eligible = $derived(peers.filter((p) => p.eligible));
  let ineligible = $derived(peers.filter((p) => !p.eligible));
  let selectedSet = $derived(new Set(selected));
</script>

<div class="picker">
  <div class="picker-head">
    <span class="picker-title">Who helps with this one job?</span>
    {#if eligible.length > 0}
      <div class="picker-actions">
        <button type="button" class="link" onclick={onSelectAll}>All</button>
        <span class="sep">·</span>
        <button type="button" class="link" onclick={onClear}>None</button>
      </div>
    {/if}
  </div>

  {#each eligible as p (p.node_id)}
    <label class="peer-row">
      <input
        type="checkbox"
        checked={selectedSet.has(p.node_id)}
        onchange={() => onToggle(p.node_id)}
      />
      <span class="dot online" aria-hidden="true"></span>
      <span class="peer-name">{p.name || p.node_id.slice(0, 8)}</span>
    </label>
  {/each}

  {#each ineligible as p (p.node_id)}
    <div class="peer-row ineligible" title={ineligibleReasonCopy(p.reason)}>
      <span class="dot" class:online={p.online} aria-hidden="true"></span>
      <span class="peer-name">{p.name || p.node_id.slice(0, 8)}</span>
      <span class="reason">{ineligibleReasonCopy(p.reason)}</span>
    </div>
  {/each}

  {#if peers.length === 0}
    <p class="empty">No other machines are on your mesh right now.</p>
  {/if}
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .picker-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 0.25rem;
  }
  .picker-title {
    font-size: 0.85rem;
    font-weight: 600;
  }
  .picker-actions {
    font-size: 0.8rem;
    opacity: 0.75;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    color: inherit;
    cursor: pointer;
    text-decoration: underline;
  }
  .sep {
    margin: 0 0.35rem;
    opacity: 0.5;
  }
  .peer-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.25rem 0;
    font-size: 0.9rem;
  }
  .peer-row.ineligible {
    opacity: 0.5;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--color-border, #999);
    flex: 0 0 auto;
  }
  .dot.online {
    background: var(--color-success, #37a169);
  }
  .peer-name {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .reason {
    font-size: 0.78rem;
    opacity: 0.85;
    font-style: italic;
  }
  .empty {
    font-size: 0.85rem;
    opacity: 0.7;
    margin: 0.25rem 0;
  }
</style>
