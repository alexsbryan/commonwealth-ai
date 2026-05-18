<script lang="ts">
  import InkStamp from "../components/onboarding/InkStamp.svelte";

  interface Props {
    onBegin: () => void;
  }

  let { onBegin }: Props = $props();
</script>

<div class="threshold">
  <div class="threshold-content">
    <div class="mark"><InkStamp size="md" active={false} /></div>
    <p class="line line-primary">A place to write, think, and remember.</p>
    <p class="line line-secondary">Everything stays on this machine.</p>
    <p class="line line-tertiary">Let's prepare it.</p>
    <button class="begin-btn" onclick={onBegin}>Begin</button>
  </div>
</div>

<style>
  /* Lavender Court substrate — matches loading, setup, and the
     post-onboarding app surfaces. The previous off-white was an
     orphan that created a dark→light→dark flip across the flow. */
  .threshold {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    background: var(--bg-primary);
  }

  .threshold-content {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 4px;
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
    margin: 0;
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

  .begin-btn {
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
    -webkit-font-smoothing: antialiased;
  }

  .begin-btn:hover {
    border-color: var(--accent);
    color: var(--accent-light);
    background: var(--bg-surface);
  }

  .begin-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }
</style>
