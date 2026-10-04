<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  The run, seen from everywhere else in the app.

  A deep-research run outlives the surface that started it — by design, and
  now in fact. That leaves one obligation: while it is going, the operator
  must be able to tell from any screen that it is going, and must not be able
  to miss it when it lands. Before this, leaving the deep-research view left
  no trace of the run anywhere in the app, and a report that finished while
  the user was in chat was never surfaced at all.

  Three states, and they are the store's, not this component's:
    • running    — a beat, the stage, round N of M, elapsed. Click to watch.
    • no-signal  — the backend stopped ticking. Said plainly, in amber.
    • finished   — held until acknowledged, because the whole point is that
                   work completed while they were looking elsewhere.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import {
    deepResearchStore,
    formatElapsed,
  } from "../../stores/deepResearch.svelte";

  interface Props {
    /** Take the operator to the deep-research surface. The pill asks; it
     *  does not navigate — the router's owner decides when the move is
     *  refusable, the same contract `ReconnectBanner` follows. */
    onOpen: () => void;
    /** Suppressed while the deep-research surface itself is showing: the
     *  run is already fully rendered there, and a floating echo of it is
     *  noise. */
    hidden?: boolean;
  }

  let { onOpen, hidden = false }: Props = $props();

  // A run may already be in flight when the app starts — the backend
  // survives a webview reload. Ask once on mount so the pill can appear
  // without anyone opening the deep-research view first.
  onMount(() => {
    void deepResearchStore.recover();
    // Closing the window kills the run's task. The backend refuses the
    // close while research is in flight and hands the decision here.
    void deepResearchStore.initQuitGuard();
  });

  let active = $derived(deepResearchStore.active);
  let liveness = $derived(deepResearchStore.liveness);
  let signalAge = $derived(deepResearchStore.signalAgeSecs);
  let unseen = $derived(deepResearchStore.unseenFinished);
  let quitBlocked = $derived(deepResearchStore.quitBlocked);

  function openRun() {
    onOpen();
  }

  function openFinished() {
    // Leave `seen` to the view: it marks the report seen as it renders it,
    // so a click that somehow fails to navigate does not silently retire
    // the only notice that the run finished.
    onOpen();
  }
</script>

<!-- The quit guard is NOT suppressed by `hidden`: it is a refusal to close
     the whole app, and it has to be answered from wherever the operator
     happens to be, including the deep-research surface itself. -->
{#if quitBlocked}
  <div class="dr-quit-scrim" data-testid="dr-quit-blocked" role="alertdialog" aria-modal="true">
    <div class="dr-quit-box">
      <h2>Research is still running</h2>
      <p>
        {#if active}
          “{active.question}”{active.round !== null
            ? ` — round ${active.round}${active.maxRounds ? ` of ${active.maxRounds}` : ""}`
            : ""}{active.lastBeatMs !== null
            ? `, ${formatElapsed(active.elapsedSecs)} in`
            : ""}.
        {:else}
          A deep-research run is in flight.
        {/if}
      </p>
      <p class="dr-quit-consequence">
        Closing the app stops it. Everything it has gathered is kept on
        disk, so you can resume the run next time — but this round's work
        in progress is lost.
      </p>
      <div class="dr-quit-buttons">
        <button
          type="button"
          class="dr-quit-stay"
          onclick={() => deepResearchStore.dismissQuitBlock()}
          data-testid="dr-quit-stay"
        >
          Keep researching
        </button>
        <button
          type="button"
          class="dr-quit-go"
          onclick={() => void deepResearchStore.quitAnyway()}
          data-testid="dr-quit-anyway"
        >
          Close anyway
        </button>
      </div>
    </div>
  </div>
{/if}

{#if !hidden && unseen}
  <div class="dr-presence finished" data-testid="dr-presence-finished" role="status">
    <div class="dr-presence-text">
      <strong>
        {unseen.error
          ? unseen.stopRequested
            ? "Research stopped"
            : "Research failed"
          : unseen.stopRequested
            ? "Research stopped — findings kept"
            : "Research finished"}
      </strong>
      <span class="dr-presence-sub">{unseen.question || unseen.runId}</span>
    </div>
    <button
      type="button"
      class="dr-presence-action"
      onclick={openFinished}
      data-testid="dr-presence-open"
    >
      {unseen.error ? "See what happened" : "Read the report"}
    </button>
    <button
      type="button"
      class="dr-presence-dismiss"
      onclick={() => deepResearchStore.clearFinished()}
      aria-label="Dismiss"
      data-testid="dr-presence-dismiss"
    >
      ×
    </button>
  </div>
{:else if !hidden && active}
  <button
    type="button"
    class="dr-presence running"
    class:stalled={liveness === "no-signal"}
    data-testid="dr-presence"
    data-liveness={liveness}
    onclick={openRun}
    title={active.question}
  >
    <span
      class="dr-presence-beat"
      class:stalled={liveness === "no-signal"}
      aria-hidden="true"
    ></span>
    <span class="dr-presence-text">
      <strong data-testid="dr-presence-label">
        {liveness === "no-signal" ? `No signal for ${signalAge}s` : "Researching"}
      </strong>
      <span class="dr-presence-sub" data-testid="dr-presence-detail">
        {#if active.round !== null}
          round {active.round}{active.maxRounds ? ` of ${active.maxRounds}` : ""}
        {:else}
          starting
        {/if}
        {#if active.lastBeatMs !== null}
          · {formatElapsed(active.elapsedSecs)}
        {/if}
      </span>
    </span>
  </button>
{/if}

<style>
  .dr-presence {
    /* Top-right, not bottom-right. Bottom-right lands squarely on the chat
       composer — the app's primary input — which a run lasting twenty
       minutes would sit on top of the whole time. The only other fixed
       element up here is the daemon attach badge, which is transient by
       design; a run in flight outranks it. */
    position: fixed;
    right: 12px;
    top: 12px;
    z-index: 900;
    max-width: min(380px, calc(100vw - 84px));
    display: flex;
    align-items: center;
    gap: 10px;
    border-radius: 999px;
    padding: 8px 14px;
    font-size: 13px;
    text-align: left;
    border: 1px solid var(--border, #333);
    background: var(--surface, #17171b);
    color: inherit;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
  }
  .dr-presence.running {
    cursor: pointer;
  }
  .dr-presence.running.stalled {
    border-color: #c9a227;
  }
  .dr-presence.finished {
    border-radius: 10px;
    border-color: var(--accent, #4a9eff);
    padding: 10px 12px;
  }
  .dr-presence-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .dr-presence-text strong {
    font-size: 13px;
  }
  .dr-presence-sub {
    color: var(--muted, #888);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    max-width: 34ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Keeps time with the backend's heartbeat: when the signal stops, the
     dot stops. A frozen dot and a frozen run are the same picture on
     purpose — this is the one indicator that must not lie by animating. */
  .dr-presence-beat {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent, #4a9eff);
    flex: none;
    animation: dr-presence-beat 2s ease-in-out infinite;
  }
  .dr-presence-beat.stalled {
    background: #c9a227;
    animation: none;
  }
  @keyframes dr-presence-beat {
    0%,
    100% {
      opacity: 0.25;
      transform: scale(0.8);
    }
    50% {
      opacity: 1;
      transform: scale(1.15);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dr-presence-beat {
      animation: none;
      opacity: 1;
    }
  }
  .dr-presence-action {
    border: 1px solid var(--accent, #4a9eff);
    background: none;
    color: var(--accent, #4a9eff);
    border-radius: 6px;
    padding: 4px 10px;
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
  }
  /* ── The quit guard ────────────────────────────────────────────────── */
  .dr-quit-scrim {
    position: fixed;
    inset: 0;
    z-index: 1200;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.55);
  }
  .dr-quit-box {
    max-width: 460px;
    margin: 16px;
    padding: 20px 22px;
    border-radius: 12px;
    border: 1px solid var(--border, #333);
    background: var(--surface, #17171b);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
  }
  .dr-quit-box h2 {
    margin: 0 0 8px;
    font-size: 17px;
  }
  .dr-quit-box p {
    margin: 0 0 8px;
    font-size: 14px;
  }
  .dr-quit-consequence {
    color: var(--muted, #888);
  }
  .dr-quit-buttons {
    display: flex;
    gap: 10px;
    justify-content: flex-end;
    margin-top: 16px;
  }
  .dr-quit-stay {
    border: none;
    border-radius: 6px;
    padding: 7px 14px;
    font-size: 13px;
    cursor: pointer;
    background: var(--accent, #4a9eff);
    color: #fff;
    font-weight: 600;
  }
  .dr-quit-go {
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    padding: 7px 14px;
    font-size: 13px;
    cursor: pointer;
    background: none;
    color: inherit;
  }
  .dr-presence-dismiss {
    background: none;
    border: none;
    color: var(--muted, #888);
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
    padding: 0 2px;
  }
</style>
