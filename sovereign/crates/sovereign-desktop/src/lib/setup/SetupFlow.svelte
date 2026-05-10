<!--
  SetupFlow — the only multi-step screen in the entire app.

  Listens to a single `setup-progress` Tauri event channel and
  invokes `complete_setup_auto`, which runs the whole setup chain
  (hardware probe → 3-model download → DB open → model load → smoke
  test) with no user choices. Renders one sentence, one
  ProgressRule, and the breathing InkStamp — nothing else. No
  cancel button: closing the window pauses (downloads resume from
  `.part` on relaunch).

  Themed with the app's Lavender Court palette — dark plum
  background, lavender-cream ink, gold accent — so the transition
  into chat is unbroken. The breathing ◈ and the progress rule
  already render in gold by default.
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { completeSetupAuto } from "../api";
  import InkStamp from "../components/onboarding/InkStamp.svelte";
  import ProgressRule from "../components/onboarding/ProgressRule.svelte";

  interface Props {
    onComplete: () => void;
  }

  let { onComplete }: Props = $props();

  type SetupPhase =
    | { kind: "detecting_hardware" }
    | { kind: "preparing_data_dir" }
    | { kind: "downloading_primary"; mb_total: number | null }
    | { kind: "downloading_fast" }
    | { kind: "downloading_embed" }
    | { kind: "opening_database" }
    | { kind: "loading_model" }
    | { kind: "smoke_testing" }
    | { kind: "ready" }
    | { kind: "failed"; recoverable: boolean };

  type Progress = {
    phase: SetupPhase;
    message: string;
    fraction: number | null;
    eta_seconds: number | null;
    indeterminate: boolean;
  };

  let progress = $state<Progress>({
    phase: { kind: "detecting_hardware" },
    message: "Reading what this machine can do.",
    fraction: null,
    eta_seconds: null,
    indeterminate: true,
  });
  let failed = $state<{ message: string; recoverable: boolean } | null>(null);
  let unlisten: UnlistenFn | null = null;

  onMount(async () => {
    unlisten = await listen<Progress>("setup-progress", (e) => {
      progress = e.payload;
      if (e.payload.phase.kind === "failed") {
        failed = {
          message: e.payload.message,
          recoverable: e.payload.phase.recoverable,
        };
      } else {
        failed = null;
      }
    });
    try {
      await completeSetupAuto();
      onComplete();
    } catch (e) {
      // Backend will already have emitted Failed; this catch handles
      // the pathological case where it didn't.
      if (!failed) {
        failed = { message: String(e), recoverable: false };
      }
    }
  });

  onDestroy(() => {
    unlisten?.();
  });

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

  async function retry() {
    failed = null;
    progress = {
      phase: { kind: "detecting_hardware" },
      message: "Reading what this machine can do.",
      fraction: null,
      eta_seconds: null,
      indeterminate: true,
    };
    try {
      await completeSetupAuto();
      onComplete();
    } catch (e) {
      if (!failed) {
        failed = { message: String(e), recoverable: false };
      }
    }
  }
</script>

<div class="setup-flow">
  <div class="content">
    <div class="mark"><InkStamp size="md" active={!failed} /></div>

    <p class="sentence">{failed?.message ?? progress.message}</p>

    {#if !failed}
      <div class="rule">
        <ProgressRule
          value={progress.indeterminate ? null : progress.fraction}
          counter={fmtCounter(progress)}
          tone="neutral"
        />
      </div>
      {#if isDownloadPhase(progress.phase.kind) && fmtEta(progress.eta_seconds)}
        <p class="eta">{fmtEta(progress.eta_seconds)}</p>
      {/if}
    {:else if failed.recoverable}
      <button class="retry" onclick={retry}>Try again</button>
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

  .sentence {
    font-family: "Outfit", system-ui, -apple-system, "Segoe UI", sans-serif;
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

  .retry {
    font-family: "Outfit", system-ui, -apple-system, "Segoe UI", sans-serif;
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
</style>
