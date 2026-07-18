<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { prepareCrashReport } from "../api";

  // Mirrors `crate::supervisor::SupervisorState`. The Rust side
  // emits `#[serde(tag = "kind", rename_all = "snake_case")]` so we
  // get a discriminant in `kind`.
  type SupervisorState =
    | { kind: "starting" }
    | { kind: "healthy"; pid: number; since_unix: number }
    | { kind: "unhealthy"; pid: number; consecutive_failures: number }
    | {
        kind: "restarting";
        attempt: number;
        after_secs: number;
        reason: string;
      }
    | { kind: "failed"; reason: string; last_crash_log: string | null };

  // Mirrors `crate::attach_watch::AttachDaemonState` — health of an
  // EXTERNALLY-owned daemon in Attach mode (no supervisor).
  type AttachDaemonState =
    | { kind: "healthy"; client_port: number }
    | { kind: "down"; client_port: number; consecutive_failures: number };

  let current: SupervisorState | null = $state(null);
  let attach: AttachDaemonState | null = $state(null);
  let attachDismissed = $state(false);
  let restartBusy = $state(false);
  let restartError: string | null = $state(null);
  let unlisten: UnlistenFn | null = null;
  let unlistenAttach: UnlistenFn | null = null;
  let unlistenFallback: UnlistenFn | null = null;
  let sendBusy = $state(false);
  let reconnectBusy = $state(false);
  let reconnectError: string | null = $state(null);
  let lastReportPath: string | null = $state(null);
  let lastReportError: string | null = $state(null);
  // Set when the backend fell back to the in-process daemon (crash
  // isolation off) — surfaced instead of the old silent revert.
  let fallbackReason: string | null = $state(null);

  // Visible only for non-healthy states. Starting + Healthy stay
  // silent — banners that pop up for normal operations train users
  // to dismiss them, which is the worst outcome.
  let visible: boolean = $derived.by(() => {
    const s = current;
    if (s === null) return false;
    return s.kind !== "starting" && s.kind !== "healthy";
  });

  let summary: string = $derived.by(() => {
    const s = current;
    if (s === null) return "";
    if (s.kind === "unhealthy") {
      return `Daemon not responding (${s.consecutive_failures} failed checks)`;
    }
    if (s.kind === "restarting") {
      return `Restarting daemon — attempt ${s.attempt}, retrying in ${s.after_secs}s`;
    }
    if (s.kind === "failed") {
      return `Daemon stopped: ${s.reason}`;
    }
    return "";
  });

  let isFailed: boolean = $derived.by(() => current?.kind === "failed");

  onMount(async () => {
    unlisten = await listen<SupervisorState>("supervisor-state", (event) => {
      current = event.payload;
      if (event.payload.kind === "healthy") reconnectError = null;
    });
    unlistenFallback = await listen<{ reason: string }>(
      "supervisor-fallback",
      (event) => {
        fallbackReason = event.payload.reason;
      },
    );
    unlistenAttach = await listen<AttachDaemonState>(
      "attach-daemon-state",
      (event) => {
        if (event.payload.kind === "healthy") {
          // Recovery is automatic in attach mode — clear everything.
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
    if (unlisten) unlisten();
    if (unlistenAttach) unlistenAttach();
    if (unlistenFallback) unlistenFallback();
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
        await invoke("plugin:shell|open", { path: info.issues_url });
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

  async function handleReconnect() {
    // Wake the crash-loop-latched supervisor for another spawn
    // attempt. The banner stays up until the supervisor emits a
    // healthy state (which hides it via `visible`).
    if (reconnectBusy) return;
    reconnectBusy = true;
    reconnectError = null;
    try {
      await invoke("supervisor_reconnect");
    } catch (e) {
      reconnectError = e instanceof Error ? e.message : String(e);
    } finally {
      reconnectBusy = false;
    }
  }

  function handleDismiss() {
    current = null;
  }

  async function handleAttachRestart() {
    // Best-effort service-manager restart of the external daemon.
    // The banner clears on the attach watcher's healthy transition,
    // not here — restart success is judged by the daemon answering.
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

{#if visible}
  <div class="banner" class:banner-failed={isFailed} role="status">
    <span class="banner-text">{summary}</span>
    <div class="banner-actions">
      {#if isFailed}
        <button
          class="action action-primary"
          onclick={handleReconnect}
          disabled={reconnectBusy}
        >
          {reconnectBusy ? "Reconnecting…" : "Reconnect"}
        </button>
        <button class="action" onclick={handleReportProblem} disabled={sendBusy}>
          {sendBusy ? "Preparing…" : "Report problem"}
        </button>
        <button class="action" onclick={handleDismiss}>Dismiss</button>
      {/if}
    </div>
  </div>

  {#if reconnectError}
    <div class="report-info report-error">
      Reconnect failed: {reconnectError}
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

{#if attachVisible && !visible}
  <div class="banner banner-failed" role="status">
    <span class="banner-text">
      Lost the connection to your daemon (port {attach?.kind === "down"
        ? attach.client_port
        : ""}). If it's restarting, this clears by itself; otherwise
      restart it — or run <code>svrn daemon restart</code>.
    </span>
    <div class="banner-actions">
      <button
        class="action action-primary"
        onclick={handleAttachRestart}
        disabled={restartBusy}
      >
        {restartBusy ? "Restarting…" : "Restart daemon"}
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
{/if}

{#if fallbackReason && !visible && !attachVisible}
  <div class="banner" role="status">
    <span class="banner-text">
      Running without crash protection this session ({fallbackReason}) — a
      model crash would close the app. Restarting the app retries.
    </span>
    <div class="banner-actions">
      <button class="action" onclick={() => (fallbackReason = null)}>
        Dismiss
      </button>
    </div>
  </div>
{/if}

<style>
  /* Banner sits at the top of the viewport without pushing layout
     around (fixed position). Subtle by default; intensifies once
     the supervisor latches Failed. */
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
