<script lang="ts">
  /// Soft welcome hints for the inner-work surface. Staggered fade-in,
  /// linger, fade-out — each cue plays the same keyframe animation
  /// with a different `animation-delay` so they appear one after the
  /// other and quietly leave together.
  ///
  /// Tone (per design ask): "gentle maternal therapist" — lowercase,
  /// suggestive rather than instructional, anchored to the bottom of
  /// the surface so the writing column stays the protagonist. The
  /// cues are pointer-events:none so they never intercept clicks; the
  /// surface's Esc handler dismisses them instantly when the user
  /// wants quiet.
  ///
  /// Suppression: the parent only mounts this component when the
  /// surface has no prior turns AND no resumed draft, so a returning
  /// entry doesn't get re-welcomed. The parent also gates per
  /// `innerWorkSession.hintsShown` (window-scoped) so the hints play
  /// at most once per page load.

  interface Hint {
    body: string;
    chord: string;
  }

  interface Props {
    hints: Hint[];
  }

  let { hints }: Props = $props();
</script>

<ol class="cues" aria-hidden="true">
  {#each hints as hint, i (i)}
    <li class="cue" style="animation-delay: {i * 1400}ms;">
      {#if hint.chord}
        <kbd class="chord">{hint.chord}</kbd>
      {/if}
      <span class="text">{hint.body}</span>
    </li>
  {/each}
</ol>

<style>
  /* Inherits the inner-work palette (`--inner-ink-muted`, etc.) from
     the surface's `.root`. The cues are fixed to the viewport so
     they don't scroll with the document; positioned above the
     local-indicator so they read as marginalia rather than chrome. */
  .cues {
    position: fixed;
    bottom: 3.5rem;
    left: 50%;
    transform: translateX(-50%);
    margin: 0;
    padding: 0;
    list-style: none;
    z-index: 5;
    pointer-events: none;
    display: flex;
    flex-direction: column;
    gap: 0.55em;
    align-items: center;
    max-width: min(60ch, 90vw);
    text-align: center;
  }

  .cue {
    display: inline-flex;
    align-items: baseline;
    gap: 0.55em;
    color: var(--inner-ink-muted);
    font-style: italic;
    font-size: 0.93em;
    letter-spacing: 0.005em;
    opacity: 0;
    /* Single keyframe per cue: fade-in (0→8%), linger (8%→78%),
       fade-out (78%→100%). At 9.5s total that's ~760ms in,
       ~6.6s lingering, ~2.1s out per cue — long enough to read
       comfortably, short enough to step out of the way. The
       `animation-delay` on each li staggers the entrances. */
    animation: hint-cue 9500ms ease-out both;
  }

  @keyframes hint-cue {
    0% {
      opacity: 0;
      transform: translateY(6px);
      filter: blur(2px);
    }
    8% {
      opacity: 1;
      transform: translateY(0);
      filter: blur(0);
    }
    78% {
      opacity: 1;
    }
    100% {
      opacity: 0;
      transform: translateY(-2px);
      filter: blur(0.5px);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .cue {
      animation-duration: 5000ms;
      animation-timing-function: linear;
    }
    @keyframes hint-cue {
      0%, 100% { opacity: 0; }
      15%, 85% { opacity: 1; }
    }
  }

  .chord {
    display: inline-block;
    padding: 1px 6px;
    font-family: "JetBrains Mono", "SF Mono", Menlo, monospace;
    font-size: 0.85em;
    font-style: normal;
    color: var(--inner-ink);
    background: oklch(from var(--inner-bg-cool) calc(l - 0.025) c h);
    border-radius: 3px;
    box-shadow: 0 0 0 1px var(--inner-rule);
  }
</style>
