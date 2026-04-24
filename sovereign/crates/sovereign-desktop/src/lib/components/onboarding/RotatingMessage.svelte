<!--
  RotatingMessage — cycles through a list of short status strings
  with a letterpress fade-through effect. Used on "preparing your
  atlas" and similar indeterminate phases where we want the user to
  feel machinery, not a blank wait.

  Slow cycle (2.4s per message) so reading feels unhurried. Pauses
  on the final message — doesn't loop infinitely past the last
  entry — so the copy doesn't feel empty if init takes longer.
-->
<script lang="ts">
  import { onDestroy } from "svelte";

  interface Props {
    messages: string[];
    /// ms per message. Default 2400 matches the stamp's 2.8s
    /// breathing cadence without locking to it.
    intervalMs?: number;
    /// When true, loop back to the first message after the last.
    /// Defaults to false — land and hold on the final line.
    loop?: boolean;
  }

  let { messages, intervalMs = 2400, loop = false }: Props = $props();

  let idx = $state(0);
  let timer: ReturnType<typeof setInterval> | null = null;

  function tick() {
    if (idx + 1 >= messages.length) {
      if (!loop && timer) {
        clearInterval(timer);
        timer = null;
      }
      if (loop) idx = 0;
      return;
    }
    idx = idx + 1;
  }

  $effect(() => {
    // Reset on prop change. Clear prior timer before starting a new
    // one so we don't leak.
    if (timer) { clearInterval(timer); timer = null; }
    idx = 0;
    if (messages.length > 1) {
      timer = setInterval(tick, intervalMs);
    }
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
  });
</script>

<span class="rot-msg" aria-live="polite">
  {#key idx}
    <span class="rot-line">{messages[idx] ?? ""}</span>
  {/key}
</span>

<style>
  .rot-msg {
    display: inline-block;
    min-height: 1.4em;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
  }
  .rot-line {
    display: inline-block;
    animation: rot-in 360ms cubic-bezier(0.2, 0.8, 0.2, 1) both;
  }
  @keyframes rot-in {
    from {
      opacity: 0;
      transform: translateY(4px);
      filter: blur(0.5px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
      filter: blur(0);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .rot-line { animation: none; }
  }
</style>
