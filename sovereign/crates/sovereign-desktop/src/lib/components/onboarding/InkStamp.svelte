<!--
  InkStamp — the pulsing ◈ mark. The brand heartbeat during any
  long-running phase. Subtle (2.8s cycle), never strobing.

  `size` picks one of three weights (sm/md/lg); `active` toggles
  the breathing animation (off for terminal or paused states).
-->
<script lang="ts">
  interface Props {
    size?: "sm" | "md" | "lg";
    active?: boolean;
  }
  let { size = "md", active = true }: Props = $props();
</script>

<span
  class="stamp"
  data-size={size}
  class:is-active={active}
  aria-hidden="true"
>◈</span>

<style>
  .stamp {
    display: inline-block;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 10px rgba(201, 168, 76, 0.4));
    transition: filter 280ms ease;
  }
  .stamp[data-size="sm"] { font-size: 1rem;   }
  .stamp[data-size="md"] { font-size: 1.85rem; }
  .stamp[data-size="lg"] { font-size: 2.6rem;  }

  .stamp.is-active {
    animation: stamp-breathe 2.8s ease-in-out infinite;
  }

  @keyframes stamp-breathe {
    0%, 100% {
      transform: scale(1);
      filter: drop-shadow(0 0 10px rgba(201, 168, 76, 0.35));
    }
    50% {
      transform: scale(1.06);
      filter: drop-shadow(0 0 26px rgba(201, 168, 76, 0.65));
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .stamp.is-active { animation: none; }
  }
</style>
