<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { invoke, invokePlugin } from "../invoke";
  import { prepareCrashReport } from "../api";

  interface Props {
    /** Take the user to the health check.
     *
     *  The banner asks; it does not navigate. Both halves of the move
     *  — queueing the Diagnostics tab and switching the view — belong
     *  to whoever owns the router, because that owner is the only one
     *  who knows when the move is refusable (mid-setup, it is). A
     *  banner that queued the tab itself would leave a stale request
     *  behind on every refusal. Unset = the route is hidden rather
     *  than dead. */
    onOpenDiagnostics?: () => void;
  }

  let { onOpenDiagnostics }: Props = $props();

  // Mirrors `crate::attach_watch::AttachDaemonState` — health of the daemon
  // this app talks to, which it does not own.
  //
  // There used to be a second source here: `supervisor-state`, emitted by a
  // supervisor the desktop ran over a daemon CHILD it had spawned, with its
  // own banner, its own Reconnect button (`supervisor_reconnect`) and a
  // `supervisor-fallback` notice for "running without crash protection this
  // session". All three are gone with the supervisor (sv-surface svt-2): the
  // app starts no daemon, so there is no child to be unhealthy, to wake, or
  // to fail over from. One daemon, one health signal, one recovery button.
  type AttachDaemonState =
    | { kind: "healthy"; client_port: number }
    | { kind: "down"; client_port: number; consecutive_failures: number };

  let attach: AttachDaemonState | null = $state(null);
  let attachDismissed = $state(false);
  let restartBusy = $state(false);
  let restartError: string | null = $state(null);
  let unlistenAttach: UnlistenFn | null = null;
  let sendBusy = $state(false);
  let lastReportPath: string | null = $state(null);
  let lastReportError: string | null = $state(null);

  onMount(async () => {
    unlistenAttach = await listen<AttachDaemonState>(
      "attach-daemon-state",
      (event) => {
        if (event.payload.kind === "healthy") {
          // Recovery is automatic — attach-mode calls are stateless HTTP, so
          // the moment the daemon answers again everything works.
          attach = null;
          attachDismissed = false;
          restartError = null;
        } else {
          attach = event.payload;
        }
      },
    );
  });

  onDestroy(() => {
    if (unlistenAttach) unlistenAttach();
  });

  async function handleReportProblem() {
    if (sendBusy) return;
    sendBusy = true;
    lastReportError = null;
    try {
      const info = await prepareCrashReport();
      lastReportPath = info.report_path;
      // Open the project's GitHub Issues page via the shell plugin.
      // The locally-saved report path is surfaced below so the user
      // can attach it to the issue they open. Nothing auto-uploads.
      try {
        await invokePlugin("plugin:shell|open", { path: info.issues_url });
      } catch {
        // Shell open failed (e.g. no default browser). The path is
        // still visible so the user can open an issue and attach it.
      }
    } catch (e) {
      lastReportError = e instanceof Error ? e.message : String(e);
    } finally {
      sendBusy = false;
    }
  }

  async function handleAttachRestart() {
    // Best-effort service-manager restart of the daemon. The banner clears on
    // the attach watcher's healthy transition, not here — restart success is
    // judged by the daemon answering.
    if (restartBusy) return;
    restartBusy = true;
    restartError = null;
    try {
      await invoke("attach_restart_daemon");
    } catch (e) {
      restartError = e instanceof Error ? e.message : String(e);
    } finally {
      restartBusy = false;
    }
  }

  let attachVisible: boolean = $derived.by(
    () => attach?.kind === "down" && !attachDismissed,
  );
</script>

{#if attachVisible}
  <div class="banner banner-failed" role="status">
    <span class="banner-text">
      <!-- This used to end with "or run `svrn daemon restart`", which
           is the exact move a non-developer cannot make. The button
           beside it does the same thing, and the health check explains
           the rest. -->
      Lost the connection to the engine (port {attach?.kind === "down"
        ? attach.client_port
        : ""}). If it's restarting, this clears by itself.
    </span>
    <div class="banner-actions">
      <button
        class="action action-primary"
        onclick={handleAttachRestart}
        disabled={restartBusy}
      >
        {restartBusy ? "Restarting…" : "Restart the engine"}
      </button>
      {#if onOpenDiagnostics}
        <button class="action" onclick={() => onOpenDiagnostics?.()}>
          Check my setup
        </button>
      {/if}
      <!-- The crash-report affordance used to hang off the supervisor's
           Failed banner, which is gone. It moved here rather than out:
           this is now the only banner a user sees when the backend is
           unreachable, and "Report problem" is the move that survives
           when "Restart the engine" did not help. -->
      <button class="action" onclick={handleReportProblem} disabled={sendBusy}>
        {sendBusy ? "Preparing…" : "Report problem"}
      </button>
      <button class="action" onclick={() => (attachDismissed = true)}>
        Dismiss
      </button>
    </div>
  </div>
  {#if restartError}
    <div class="report-info report-error">
      Couldn't restart via the service manager: {restartError}
    </div>
  {/if}
  {#if lastReportPath}
    <div class="report-info">
      Crash report saved at: <code>{lastReportPath}</code> — attach it to
      the GitHub issue that just opened.
    </div>
  {/if}
  {#if lastReportError}
    <div class="report-info report-error">
      Couldn't prepare report: {lastReportError}
    </div>
  {/if}
{/if}

<style>
  /* Banner sits at the top of the viewport without pushing layout
     around (fixed position). Subtle by default; `banner-failed`
     intensifies it for the unreachable-daemon case. */
  .banner {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 8px 16px;
    background: oklch(95% 0.04 80);
    color: oklch(28% 0.08 50);
    font-family: var(--font-sans);
    font-size: 0.85rem;
    border-bottom: 1px solid oklch(82% 0.06 70 / 0.6);
    -webkit-font-smoothing: antialiased;
  }

  .banner-failed {
    background: oklch(94% 0.06 25);
    color: oklch(28% 0.15 25);
    border-bottom-color: oklch(78% 0.10 25 / 0.6);
  }

  .banner-text {
    flex: 1 1 auto;
  }

  .banner-actions {
    display: flex;
    gap: 8px;
  }

  .action {
    font-family: inherit;
    font-size: 0.78rem;
    font-weight: 500;
    letter-spacing: 0.05em;
    color: inherit;
    background: none;
    border: 1px solid currentColor;
    padding: 4px 12px;
    border-radius: 4px;
    cursor: pointer;
    transition: background 160ms ease;
  }

  .action:hover:not(:disabled) {
    background: oklch(50% 0.02 250 / 0.06);
  }

  .action:disabled {
    opacity: 0.6;
    cursor: progress;
  }

  .action-primary {
    background: oklch(100% 0 0 / 0.35);
  }

  .report-info {
    position: fixed;
    top: 44px;
    left: 16px;
    right: 16px;
    z-index: 999;
    padding: 6px 12px;
    background: oklch(98% 0.005 250);
    border: 1px solid oklch(82% 0.010 250 / 0.6);
    border-radius: 4px;
    font-family: var(--font-sans);
    font-size: 0.78rem;
    color: oklch(35% 0.012 250);
  }

  .report-info code {
    font-family: var(--font-mono);
    background: oklch(94% 0.008 250);
    padding: 1px 5px;
    border-radius: 3px;
  }

  .report-error {
    color: oklch(40% 0.12 25);
    border-color: oklch(78% 0.10 25 / 0.6);
  }
</style>
