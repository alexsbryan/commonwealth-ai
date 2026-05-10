<!--
  ProgressRule — letterpress progress indicator.

  Two modes:
    • determinate: a 0..1 `value` drives a gold fill between a
      hairline double rule. Tabular counters render in Syne Mono.
    • indeterminate: a slow sweep along the rule, for phases where
      we don't have a quotient yet (scanning, `enrich init`
      subprocess, etc.). No spinners — the sweep implies work.

  Designed to read as a printed register rather than a UI control.
-->
<script lang="ts">
  interface Props {
    /// 0..1 — when null, renders the indeterminate sweep.
    value?: number | null;
    /// Optional label rendered above the rule (e.g. "Scanning").
    label?: string;
    /// Optional counter fragment on the right ("42 / 58", "63%").
    /// Prefer tabular numerals.
    counter?: string;
    /// Semantic tint. `neutral` = gold, `error` = rose, `rest` =
    /// lavender. Defaults to gold.
    tone?: "neutral" | "rest" | "error";
  }

  let {
    value = null,
    label,
    counter,
    tone = "neutral",
  }: Props = $props();

  let pctClamped = $derived.by(() => {
    if (value === null || value === undefined) return 0;
    if (!Number.isFinite(value)) return 0;
    return Math.max(0, Math.min(1, value)) * 100;
  });

  let indeterminate = $derived(value === null || value === undefined);
</script>

<div class="rule-block" class:is-indet={indeterminate} data-tone={tone}>
  {#if label || counter}
    <div class="rule-head">
      {#if label}<span class="rule-label">{label}</span>{/if}
      {#if counter}<span class="rule-counter">{counter}</span>{/if}
    </div>
  {/if}
  <div class="rule-track" role="progressbar" aria-valuemin={0} aria-valuemax={100}
       aria-valuenow={indeterminate ? undefined : Math.round(pctClamped)}>
    {#if indeterminate}
      <div class="rule-sweep"></div>
    {:else}
      <div class="rule-fill" style="width: {pctClamped}%"></div>
    {/if}
  </div>
</div>

<style>
  .rule-block {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .rule-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .rule-label {
    font-family: var(--font-mono);
    font-size: 0.66rem;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-secondary);
  }
  .rule-counter {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
  }
  .rule-track {
    position: relative;
    height: 4px;
    /* Double-rule register: 1px hairline frame around an inset
       channel. The fill sits inside the channel so the outer rule
       is always visible — a printed-page affordance, not a
       material-ui bar. */
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 2px;
    overflow: hidden;
  }
  .rule-fill {
    position: absolute;
    top: 0; bottom: 0; left: 0;
    background: linear-gradient(
      90deg,
      var(--accent) 0%,
      var(--accent-light) 50%,
      var(--accent) 100%
    );
    background-size: 200% 100%;
    animation: rule-shimmer 3.6s ease-in-out infinite;
    transition: width 360ms cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  .rule-sweep {
    position: absolute;
    top: 0; bottom: 0;
    left: -30%;
    width: 30%;
    background: linear-gradient(
      90deg,
      transparent 0%,
      var(--accent-dim) 18%,
      var(--accent) 50%,
      var(--accent-dim) 82%,
      transparent 100%
    );
    animation: rule-sweep 2.4s cubic-bezier(0.55, 0.1, 0.45, 0.95) infinite;
  }

  [data-tone="rest"] .rule-fill,
  [data-tone="rest"] .rule-sweep {
    background: linear-gradient(
      90deg,
      var(--lavender) 0%,
      var(--lavender-light) 50%,
      var(--lavender) 100%
    );
    background-size: 200% 100%;
  }
  [data-tone="error"] .rule-fill {
    background: var(--error);
    animation: none;
  }
  [data-tone="error"] .rule-sweep {
    background: linear-gradient(90deg, transparent, var(--error), transparent);
  }

  @keyframes rule-shimmer {
    0%, 100% { background-position: 0% 0%; }
    50%      { background-position: 100% 0%; }
  }
  @keyframes rule-sweep {
    0%   { left: -30%; }
    100% { left: 100%; }
  }

  @media (prefers-reduced-motion: reduce) {
    .rule-fill { animation: none; }
    .rule-sweep { animation-duration: 4s; }
  }
</style>
