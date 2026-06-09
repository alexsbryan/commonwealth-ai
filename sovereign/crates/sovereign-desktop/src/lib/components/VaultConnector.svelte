<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { sinkStore } from "../stores/sinks.svelte";
</script>

<button
  class="sv-vault-btn"
  class:connected={sinkStore.anyConnected}
  disabled
  title="Obsidian sync coming soon"
>
  &#x2B21; {sinkStore.anyConnected ? "Obsidian" : "Connect vault"}
</button>

<style>
  .sv-vault-btn {
    font-size: 11px;
    border-radius: 999px;
    padding: 2px 8px;
    cursor: not-allowed;
    font-family: var(--font-sans);
    border: 0.5px solid var(--border-mid);
    background: none;
    color: var(--text-muted);
    opacity: 0.6;
  }

  .sv-vault-btn.connected {
    color: var(--pos-compat-text);
    background: rgba(99, 153, 34, 0.08);
    border-color: var(--pos-compat-border);
    opacity: 1;
    cursor: pointer;
  }
</style>
