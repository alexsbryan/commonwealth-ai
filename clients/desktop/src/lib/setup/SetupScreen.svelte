<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  SetupScreen — the PURE view for the auto-setup flow: one sentence, one
  ProgressRule, the breathing BrandMark, and (on failure) a retry. It owns
  NO backend wiring. `SetupFlow.svelte` drives it with live `setup-progress`
  events; the dev screen gallery drives it with per-phase fixtures. That
  split is what lets you audit every phase's copy without a real setup.
-->
<script lang="ts">
  import BrandMark from "../components/BrandMark.svelte";
  import ProgressRule from "../components/onboarding/ProgressRule.svelte";
  import type { Progress, Provenance, SlotProvenance } from "./setupTypes";

  interface Props {
    progress: Progress;
    failed: { message: string; recoverable: boolean } | null;
    onRetry: () => void;
    /// What's being installed (read-only, supplied by SetupFlow) — lets the
    /// ledger show provenance the setup-progress event doesn't carry.
    /// Optional so the gallery / fixtures render without it.
    provenance?: Provenance | null;
  }
  let { progress, failed, onRetry, provenance = null }: Props = $props();

  // The model tied to the current download phase, for the provenance line.
  let currentSlot = $derived.by<SlotProvenance | null>(() => {
    if (!provenance) return null;
    const k = progress.phase.kind;
    if (k === "downloading_primary") return provenance.primary;
    if (k === "downloading_fast") return provenance.fast;
    if (k === "downloading_embed") return provenance.embed;
    return null;
  });

  function fmtGb(n: number): string {
    if (!n || n <= 0) return "";
    return n < 1 ? `${Math.round(n * 1024)} MB` : `${n.toFixed(n < 10 ? 1 : 0)} GB`;
  }

  function fmtEta(secs: number | null): string | null {
    if (!secs || secs <= 0) return null;
    if (secs < 60) return `~${secs}s remaining`;
    const m = Math.round(secs / 60);
    return `~${m} min remaining`;
  }

  function fmtCounter(p: Progress): string | undefined {
    if (p.fraction == null) return undefined;
    return `${Math.round(p.fraction * 100)}%`;
  }

  function isDownloadPhase(kind: string): boolean {
    return (
      kind === "downloading_primary" ||
      kind === "downloading_fast" ||
      kind === "downloading_embed"
    );
  }
</script>

<div class="setup-flow">
  <div class="content">
    <div class="mark" class:breathing={!failed}><BrandMark size={56} /></div>

    <p class="sentence">{failed?.message ?? progress.message}</p>

    {#if !failed}
      <div class="rule">
        <ProgressRule
          value={progress.indeterminate ? null : progress.fraction}
          counter={fmtCounter(progress)}
          tone="neutral"
        />
      </div>
      {#if currentSlot}
        <p class="prov">
          {currentSlot.name} · {currentSlot.quant} · {fmtGb(currentSlot.size_gb)}
          · from <code>{currentSlot.repo}</code> → <code>{provenance?.modelsDir}</code>
        </p>
      {/if}
      {#if isDownloadPhase(progress.phase.kind) && fmtEta(progress.eta_seconds)}
        <p class="eta">{fmtEta(progress.eta_seconds)}</p>
      {/if}
      {#if provenance && (provenance.primary || provenance.fast || provenance.embed)}
        <details class="ledger">
          <summary>What's being set up</summary>
          <ul>
            {#each [provenance.primary, provenance.fast, provenance.embed] as s, i (i)}
              {#if s}
                <li><b>{s.name}</b> · {s.quant} · {fmtGb(s.size_gb)} · {s.repo}</li>
              {/if}
            {/each}
            <li class="dest">→ {provenance.modelsDir}</li>
          </ul>
        </details>
      {/if}
      <p class="default-note">
        Chosen to fit your hardware — you can change models anytime in Settings.
      </p>
    {:else}
      <!-- Even non-recoverable backend errors get a retry path: many
           "unrecoverable" diagnoses (mkdir, save-config) are actually
           transient permission / disk-space races, and giving the user
           *some* action beats stranding them on an error sentence. The
           hint line below tells them where to look if retry keeps failing. -->
      <button class="retry" onclick={onRetry}>Try again</button>
      {#if !failed.recoverable}
        <p class="report-hint">
          If this keeps happening, please share <code>~/.svrnmesh/logs</code>
          when reporting.
        </p>
      {/if}
    {/if}
  </div>
</div>

<style>
  .setup-flow {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    background: var(--bg-primary);
  }

  .content {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 22px;
    max-width: 460px;
    padding: 0 32px;
  }

  .mark {
    margin-bottom: 4px;
  }

  /* Breathing heartbeat while setup runs (paused on failure). Scales the
     wrapper so the BrandMark's own gold glow rides along — a calm 2.8s
     cycle, never strobing. */
  .mark.breathing {
    animation: mark-breathe 2.8s ease-in-out infinite;
  }

  @keyframes mark-breathe {
    0%, 100% { transform: scale(1); }
    50% { transform: scale(1.06); }
  }

  @media (prefers-reduced-motion: reduce) {
    .mark.breathing { animation: none; }
  }

  .sentence {
    font-family: var(--font-sans);
    font-size: 1.05rem;
    font-weight: 400;
    line-height: 1.5;
    letter-spacing: -0.005em;
    color: var(--text-primary);
    margin: 0;
  }

  .rule {
    width: 100%;
    max-width: 360px;
  }

  .eta {
    font-family: var(--font-mono);
    font-size: 0.66rem;
    letter-spacing: 0.08em;
    color: var(--text-muted);
    margin: 0;
  }

  .prov {
    font-size: 0.78rem;
    line-height: 1.5;
    color: var(--text-secondary);
    margin: 0;
    max-width: 380px;
  }
  .prov code {
    font-family: var(--font-mono);
    font-size: 0.92em;
    color: var(--text-muted);
    word-break: break-all;
  }
  .ledger {
    width: 100%;
    max-width: 380px;
  }
  .ledger summary {
    cursor: pointer;
    color: var(--text-muted);
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .ledger ul {
    list-style: none;
    padding: 8px 0 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 0.78rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .ledger b {
    color: var(--text-primary);
    font-weight: 600;
  }
  .ledger .dest {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    color: var(--text-muted);
    word-break: break-all;
  }

  /* Quiet reassurance that the auto-picked models are a sensible default,
     not a locked-in choice. */
  .default-note {
    font-family: var(--font-sans);
    font-size: 0.78rem;
    line-height: 1.5;
    color: var(--text-muted);
    margin: 0;
    max-width: 360px;
  }

  .retry {
    font-family: var(--font-sans);
    font-size: 0.82rem;
    font-weight: 500;
    letter-spacing: 0.07em;
    color: var(--text-primary);
    background: none;
    border: 1px solid var(--border-bright);
    padding: 10px 28px;
    border-radius: var(--radius);
    cursor: pointer;
    transition: border-color 180ms ease, background 180ms ease,
      color 180ms ease;
  }

  .retry:hover {
    border-color: var(--accent);
    color: var(--accent-light);
    background: var(--bg-surface);
  }

  .retry:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }

  .report-hint {
    font-family: var(--font-mono);
    font-size: 0.7rem;
    line-height: 1.5;
    letter-spacing: 0.02em;
    color: var(--text-muted);
    margin: 6px 0 0;
  }

  .report-hint code {
    font-family: var(--font-mono);
    background: var(--bg-surface);
    padding: 1px 5px;
    border-radius: 3px;
    color: var(--text-secondary);
  }
</style>
