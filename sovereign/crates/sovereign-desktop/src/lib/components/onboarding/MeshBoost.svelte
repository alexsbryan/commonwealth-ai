<!--
  MeshBoost — small callout that nudges toward mesh participation as
  a way to accelerate atlas builds. V1 is aspirational copy; when
  mesh-GPU delegation of the enrichment subprocess lands (separate
  feature), this component upgrades to show live mesh state and an
  active "delegate to peer" button.

  Props let callers pick the tone:
    - `emphasis = "passive"` (default) → one quiet line
    - `emphasis = "active"` → fuller callout block with CTA
-->
<script lang="ts">
  interface Props {
    emphasis?: "passive" | "active";
    /// Optional minutes-saved estimate to surface concretely ("~3 min
    /// saved with 2 peers"). Callers compute this from their own
    /// context; we just render what they give us.
    minutesSaved?: number | null;
  }

  let { emphasis = "passive", minutesSaved = null }: Props = $props();
</script>

{#if emphasis === "passive"}
  <p class="mb-line">
    A mesh of friends can cut this time.
    <span class="mb-line-soft">Mesh delegation coming soon.</span>
  </p>
{:else}
  <aside class="mb-box">
    <p class="mb-title">Faster with friends</p>
    <p class="mb-body">
      Atlas builds are embarrassingly parallel per phase. A mesh of
      peers can split Phase 1 (the slow one) across GPUs, roughly
      dividing wall-clock by the number of participating nodes.
      {#if minutesSaved !== null && minutesSaved > 0}
        You'd save about <strong>{minutesSaved} min</strong> with your
        current mesh.
      {/if}
    </p>
    <p class="mb-note">Mesh-delegated enrichment is in progress.</p>
  </aside>
{/if}

<style>
  .mb-line {
    margin: 6px 0 0;
    font-size: 0.68rem;
    color: var(--text-muted, var(--lk-ink-faded, #888));
  }
  .mb-line-soft {
    font-style: italic;
    opacity: 0.8;
    margin-left: 6px;
  }

  .mb-box {
    margin-top: 8px;
    padding: 10px 14px;
    background: var(--bg-surface, var(--lk-paper-deep, #191919));
    border-left: 2px solid var(--accent, #c4a46a);
    border-radius: 6px;
  }
  .mb-title {
    margin: 0 0 3px;
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--accent, #c4a46a);
    font-weight: 600;
  }
  .mb-body {
    margin: 0;
    font-size: 0.76rem;
    color: var(--text-secondary, var(--lk-ink-soft, #bbb));
    line-height: 1.45;
  }
  .mb-body strong {
    color: var(--text-primary, var(--lk-ink, #eee));
    font-weight: 600;
  }
  .mb-note {
    margin: 3px 0 0;
    font-size: 0.66rem;
    color: var(--text-muted, var(--lk-ink-faded, #888));
    font-style: italic;
  }
</style>
