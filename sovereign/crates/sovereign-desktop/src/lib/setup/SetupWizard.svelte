<script lang="ts">
  import { useMachine } from "@xstate/svelte";
  import { fromPromise } from "xstate";
  import { completeSetup, detectBootstrap } from "../api";
  import type { BootstrapSnapshot, SetupConfig } from "../types";
  import { setupWizardMachine } from "../machines/setupWizard.machine";
  import QuickModelSetup from "./QuickModelSetup.svelte";

  interface Props {
    onComplete: () => void;
  }

  let { onComplete }: Props = $props();

  // The wizard's state — accumulated config, error message — lives
  // on `setupWizardMachine`. The component reads `$snapshot` and
  // dispatches events; the only side effect it keeps is the
  // `onComplete` callback, wired to the machine's `done` terminal
  // state via a subscription below.
  const machine = setupWizardMachine.provide({
    actors: {
      completeSetup: fromPromise(
        async ({ input }: { input: { config: SetupConfig } }) => {
          await completeSetup(input.config);
        },
      ),
      detectBootstrap: fromPromise(
        async (): Promise<BootstrapSnapshot> => await detectBootstrap(),
      ),
    },
  });
  const { snapshot, send, actorRef } = useMachine(machine);

  // Fire `onComplete` exactly once when the machine reaches `done`.
  let hasCompleted = false;
  actorRef.subscribe((s) => {
    if (!hasCompleted && s.matches("done")) {
      hasCompleted = true;
      onComplete();
    }
  });

  let error = $derived($snapshot.context.errorMessage);

  function handleModelNext(config: SetupConfig) {
    send({ type: "PERSONA_CONFIGURED", config });
  }
</script>

<div class="wizard">

  <!-- ── Persistent header ── -->
  <header class="wizard-header">
    <div class="wizard-brand">
      <span class="wizard-mark" aria-hidden="true">◈</span>
      <span class="wizard-name">SOVEREIGN</span>
    </div>
  </header>

  <!-- ── Bootstrap probe — brief loading gate ── -->
  {#if $snapshot.matches("detecting")}
    <div class="finishing-screen">
      <div class="finishing-mark-wrap" aria-hidden="true">
        <div class="f-ring f-ring-1"></div>
        <div class="f-ring f-ring-2"></div>
        <div class="f-ring f-ring-3"></div>
        <div class="finishing-mark">◈</div>
      </div>
      <h2>Checking your setup</h2>
    </div>

  <!-- ── Model picker — the entire onboarding ── -->
  {:else if $snapshot.matches("modelSetup")}
    <div class="step-body">
      <QuickModelSetup
        onNext={handleModelNext}
        errorMessage={error}
      />
    </div>

  <!-- ── Finishing ── -->
  {:else if $snapshot.matches("finishing")}
    <div class="finishing-screen">
      {#if error}
        <p class="finishing-error">{error}</p>
      {:else}
        <div class="finishing-mark-wrap" aria-hidden="true">
          <div class="f-ring f-ring-1"></div>
          <div class="f-ring f-ring-2"></div>
          <div class="f-ring f-ring-3"></div>
          <div class="finishing-mark">◈</div>
        </div>
        <h2>Booting&hellip;</h2>
      {/if}
    </div>
  {/if}

</div>

<style>
  /* ── Wizard shell ── */
  .wizard {
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--bg-root);
    overflow: hidden;
  }

  /* ── Header ── */
  .wizard-header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 0 24px;
    height: 54px;
    flex-shrink: 0;
    border-bottom: 1px solid var(--border);
    background: var(--bg-secondary);
  }

  .wizard-brand {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }

  .wizard-mark {
    color: var(--accent);
    font-size: 1.05rem;
    filter: drop-shadow(0 0 5px rgba(201, 168, 76, 0.45));
  }

  .wizard-name {
    font-size: 0.68rem;
    font-weight: 700;
    letter-spacing: 0.22em;
    color: var(--text-secondary);
    text-transform: uppercase;
  }

  /* ── Form step ── */
  .step-body {
    flex: 1;
    overflow-y: auto;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 56px 24px 32px;
  }

  /* ── Finishing / loading screen ── */
  .finishing-screen {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0;
    text-align: center;
    padding: 2rem;
  }

  .finishing-mark-wrap {
    position: relative;
    width: 90px;
    height: 90px;
    display: flex;
    align-items: center;
    justify-content: center;
    margin-bottom: 24px;
  }

  .f-ring {
    position: absolute;
    border-radius: 50%;
    border: 1px solid rgba(201, 168, 76, 0.3);
    width: 48px;
    height: 48px;
    animation: wiz-ring-expand 3s ease-out infinite;
  }

  .f-ring-2 { animation-delay: 1s; }
  .f-ring-3 { animation-delay: 2s; }

  @keyframes wiz-ring-expand {
    0%   { transform: scale(0.6); opacity: 0.5; }
    100% { transform: scale(4);   opacity: 0; }
  }

  .finishing-mark {
    font-size: 2.6rem;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 16px rgba(201, 168, 76, 0.55));
    animation: wiz-breathe 2.8s ease-in-out infinite;
    position: relative;
    z-index: 1;
  }

  @keyframes wiz-breathe {
    0%, 100% { filter: drop-shadow(0 0 12px rgba(201, 168, 76, 0.4)); }
    50%       { filter: drop-shadow(0 0 28px rgba(201, 168, 76, 0.65)); }
  }

  .finishing-screen h2 {
    font-size: 1.3rem;
    font-weight: 400;
    color: var(--text-secondary);
    margin-bottom: 10px;
    letter-spacing: 0.04em;
  }

  .finishing-error {
    color: var(--error);
    font-size: 0.9rem;
    margin-bottom: 16px;
    max-width: 420px;
    text-align: center;
    line-height: 1.5;
  }
</style>
