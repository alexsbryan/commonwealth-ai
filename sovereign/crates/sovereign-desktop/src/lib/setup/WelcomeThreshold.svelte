<script lang="ts">
  interface Props {
    onBegin: () => void;
  }

  let { onBegin }: Props = $props();
</script>

<div class="threshold">
  <div class="threshold-content">
    <p class="line line-primary">A place to write, think, and remember.</p>
    <p class="line line-secondary">Everything stays on this machine.</p>
    <p class="line line-tertiary">Let's prepare it.</p>
    <button class="begin-btn" onclick={onBegin}>Begin</button>
  </div>
</div>

<style>
  /* Same conditioned-page substrate as Inner Work — off-white, cool-blue ink.
     The threshold is the first thing the user sees; it sets the register
     for everything that follows. */
  .threshold {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    background: oklch(98% 0.006 250);
  }

  .threshold-content {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    max-width: 420px;
    padding: 0 32px;
  }

  .line {
    font-family: "Outfit", system-ui, -apple-system, "Segoe UI", sans-serif;
    font-size: 1.05rem;
    font-weight: 400;
    line-height: 1.5;
    margin: 0 0 4px;
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

  .begin-btn {
    font-family: "Outfit", system-ui, -apple-system, "Segoe UI", sans-serif;
    font-size: 0.82rem;
    font-weight: 500;
    letter-spacing: 0.07em;
    color: oklch(22% 0.015 250);
    background: none;
    border: 1px solid oklch(72% 0.010 250 / 0.55);
    padding: 10px 28px;
    border-radius: 5px;
    cursor: pointer;
    transition: border-color 180ms ease, background 180ms ease;
    -webkit-font-smoothing: antialiased;
  }

  .begin-btn:hover {
    border-color: oklch(45% 0.012 250 / 0.8);
    background: oklch(96% 0.005 250);
  }

  .begin-btn:focus-visible {
    outline: 2px solid oklch(55% 0.04 250);
    outline-offset: 3px;
  }
</style>
