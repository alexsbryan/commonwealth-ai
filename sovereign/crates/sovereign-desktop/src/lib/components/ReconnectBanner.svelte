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

  let current: SupervisorState | null = $state(null);
  let unlisten: UnlistenFn | null = null;
  let sendBusy = $state(false);
  let lastReportPath: string | null = $state(null);
  let lastReportError: string | null = $state(null);

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
    });
  });

  onDestroy(() => {
    if (unlisten) unlisten();
  });

  async function handleSendReport() {
    if (sendBusy) return;
    sendBusy = true;
    lastReportError = null;
    try {
      const info = await prepareCrashReport();
      lastReportPath = info.report_path;
      // Open the mailto URL via the shell plugin. Falls back to the
      // user manually attaching from the path we surface below.
      try {
        await invoke("plugin:shell|open", { path: info.mailto_url });
      } catch {
        // Shell open failed (e.g. no default mail client). The path
        // is still visible so the user can attach manually.
      }
    } catch (e) {
      lastReportError = e instanceof Error ? e.message : String(e);
    } finally {
      sendBusy = false;
    }
  }

  async function handleReconnect() {
    // Reconnect is a manual signal the supervisor exposes via
    // request_reconnect(); there's no Tauri command for it yet
    // (the supervisor handle lives in AppState but no command
    // surfaces request_reconnect). For now this button just clears
    // the banner; the supervisor's auto-relaunch will pick the
    // daemon back up. If it doesn't, the user can restart the app.
    current = null;
  }
</script>

{#if visible}
  <div class="banner" class:banner-failed={isFailed} role="status">
    <span class="banner-text">{summary}</span>
    <div class="banner-actions">
      {#if isFailed}
        <button
          class="action action-primary"
          onclick={handleSendReport}
          disabled={sendBusy}
        >
          {sendBusy ? "Preparing…" : "Send report"}
        </button>
        <button class="action" onclick={handleReconnect}>Dismiss</button>
      {/if}
    </div>
  </div>

  {#if lastReportPath}
    <div class="report-info">
      Crash report ready at: <code>{lastReportPath}</code>
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
    font-family: "Outfit", system-ui, -apple-system, "Segoe UI", sans-serif;
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
    font-family: "Outfit", system-ui, sans-serif;
    font-size: 0.78rem;
    color: oklch(35% 0.012 250);
  }

  .report-info code {
    font-family: "JetBrains Mono", "SF Mono", Menlo, monospace;
    background: oklch(94% 0.008 250);
    padding: 1px 5px;
    border-radius: 3px;
  }

  .report-error {
    color: oklch(40% 0.12 25);
    border-color: oklch(78% 0.10 25 / 0.6);
  }
</style>
