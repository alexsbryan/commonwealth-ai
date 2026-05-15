<script lang="ts">
  import { recordFirstMeshConsent } from "../api";

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
    <p class="line line-primary">A mesh is a network of friends.</p>
    <p class="line line-secondary">
      When their machines need help with a thought, your machine could lend a
      hand — gently, only when you're not using it yourself.
    </p>
    <p class="line line-tertiary">
      You decide. You can always change your mind later in Settings.
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
  /* Matches WelcomeThreshold — same conditioned-page substrate so
     the consent dialog feels like the natural next sentence after
     "let's prepare it." */
  .gate {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    background: oklch(98% 0.006 250);
  }

  .gate-content {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    max-width: 460px;
    padding: 0 32px;
  }

  .line {
    font-family: "Outfit", system-ui, -apple-system, "Segoe UI", sans-serif;
    font-size: 1.05rem;
    font-weight: 400;
    line-height: 1.5;
    margin: 0 0 6px;
    letter-spacing: -0.005em;
  }

  .line-primary {
    color: oklch(22% 0.015 250);
  }

  .line-secondary {
    color: oklch(45% 0.012 250);
  }

  .line-tertiary {
    color: oklch(60% 0.010 250);
    margin-bottom: 32px;
  }

  .choices {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
  }

  .choice {
    font-family: "Outfit", system-ui, -apple-system, "Segoe UI", sans-serif;
    font-size: 0.82rem;
    font-weight: 500;
    letter-spacing: 0.07em;
    color: oklch(22% 0.015 250);
    background: none;
    border: 1px solid oklch(72% 0.010 250 / 0.55);
    padding: 10px 24px;
    border-radius: 5px;
    cursor: pointer;
    transition: border-color 180ms ease, background 180ms ease;
    -webkit-font-smoothing: antialiased;
  }

  .choice:hover:not(:disabled) {
    border-color: oklch(45% 0.012 250 / 0.8);
    background: oklch(96% 0.005 250);
  }

  .choice:focus-visible {
    outline: 2px solid oklch(55% 0.04 250);
    outline-offset: 3px;
  }

  .choice:disabled {
    opacity: 0.5;
    cursor: progress;
  }

  /* Both buttons read as equal weight in the UI — the consent decision
     is genuinely the user's, not a "recommended path." */
  .choice-secondary {
    color: oklch(45% 0.012 250);
  }

  .error {
    margin-top: 16px;
    color: oklch(50% 0.15 25);
    font-size: 0.9rem;
    font-family: "Outfit", system-ui, sans-serif;
  }
</style>
