<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { recordFirstMeshConsent } from "../api";
  import BrandMark from "../components/BrandMark.svelte";

  interface Props {
    onChoice: () => void;
  }

  let { onChoice }: Props = $props();

  let busy = $state(false);
  let errorMessage: string | null = $state(null);

  async function decide(shareGpu: boolean) {
    if (busy) return;
    busy = true;
    errorMessage = null;
    try {
      await recordFirstMeshConsent(shareGpu);
      onChoice();
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
      busy = false;
    }
  }
</script>

<div class="gate">
  <div class="gate-content">
    <div class="mark"><BrandMark size={56} /></div>
    <p class="line line-primary">A mesh is a network of friends.</p>
    <p class="line line-secondary">
      When their machines need help with a thought, your machine could lend a
      hand — gently, only when you're not using it yourself.
    </p>
    <p class="line line-tertiary">
      Off unless you choose it. You'll see every time your machine helps a peer
      in Settings → Activity &amp; Sharing, and can pause or change the limit
      there any time.
    </p>

    <div class="choices">
      <button
        class="choice choice-primary"
        onclick={() => decide(true)}
        disabled={busy}
        aria-busy={busy}
      >
        Share idle GPU
      </button>
      <button
        class="choice choice-secondary"
        onclick={() => decide(false)}
        disabled={busy}
      >
        Keep all compute local
      </button>
    </div>

    {#if errorMessage}
      <p class="error" role="alert">{errorMessage}</p>
    {/if}
  </div>
</div>

<style>
  /* Matches WelcomeThreshold + SetupFlow — same Lavender Court
     substrate so the consent gate feels like the natural next
     sentence after "Ready." */
  .gate {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    background: var(--bg-primary);
  }

  .gate-content {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    max-width: 460px;
    padding: 0 32px;
  }

  .mark {
    margin-bottom: 22px;
  }

  .line {
    font-family: var(--font-sans);
    font-size: 1.05rem;
    font-weight: 400;
    line-height: 1.5;
    margin: 0 0 6px;
    letter-spacing: -0.005em;
  }

  .line-primary {
    color: var(--text-primary);
  }

  .line-secondary {
    color: var(--text-secondary);
  }

  .line-tertiary {
    color: var(--text-muted);
    margin-bottom: 32px;
  }

  .choices {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
  }

  .choice {
    font-family: var(--font-sans);
    font-size: 0.82rem;
    font-weight: 500;
    letter-spacing: 0.07em;
    color: var(--text-primary);
    background: none;
    border: 1px solid var(--border-bright);
    padding: 10px 24px;
    border-radius: var(--radius);
    cursor: pointer;
    transition: border-color 180ms ease, background 180ms ease,
      color 180ms ease;
    -webkit-font-smoothing: antialiased;
  }

  .choice:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent-light);
    background: var(--bg-surface);
  }

  .choice:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }

  .choice:disabled {
    opacity: 0.5;
    cursor: progress;
  }

  /* Both buttons read as equal weight — the consent decision is
     genuinely the user's, not a "recommended path." Secondary is
     tinted slightly cooler so the pair reads as two options, not
     one CTA + one cancel. */
  .choice-secondary {
    color: var(--text-secondary);
  }

  .error {
    margin-top: 16px;
    color: var(--error);
    font-family: var(--font-sans);
    font-size: 0.9rem;
  }
</style>
