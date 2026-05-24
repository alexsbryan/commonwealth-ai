<!--
  Settings → About → "Check for updates" affordance.

  Wraps the two backend commands defined in src-tauri/src/update_commands.rs:
    - check_for_update  → returns `UpdateInfo | null`
    - install_update    → downloads + verifies + installs + restarts

  UX contract:
    - "Check now" button is the only entry point. We deliberately do NOT
      auto-poll on app launch — that's a separate decision (see TODO in
      module footer); for v0.1.0 the explicit-action model keeps things
      quiet for users on metered connections.

    - If the endpoint returns `null` (up to date OR transient error), we
      surface a passive toast. No modal. Users can re-click without
      cognitive friction.

    - If the endpoint returns `UpdateInfo`, we render an in-section banner
      with version + (truncated) release notes + Install button. Clicking
      Install kicks off download+install+restart with no further confirm —
      the user already opted in by clicking. A scary modal here would feel
      bureaucratic.

    - During download we replace the "Install" button with a spinner-style
      status line. Errors swap that for a retry affordance.

  Why not a native dialog? The Lavender Court visual language carries
  through better with the in-section banner; native dialogs read as
  "system interrupting the app" rather than "tool the user is operating."
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { checkForUpdate, installUpdate, type UpdateInfo } from "../api";
  import { toastStore } from "../stores/toast.svelte";

  type Phase =
    | { kind: "idle" }
    | { kind: "checking" }
    | { kind: "available"; info: UpdateInfo }
    | { kind: "installing" }
    | { kind: "error"; message: string };

  let phase = $state<Phase>({ kind: "idle" });
  let appVersion = $state<string | null>(null);

  onMount(async () => {
    try {
      appVersion = await getVersion();
    } catch {
      // getVersion() is infallible in practice; fall back silently so
      // the version line just won't render rather than tripping a state.
      appVersion = null;
    }
  });

  async function check() {
    phase = { kind: "checking" };
    try {
      const info = await checkForUpdate();
      if (info) {
        phase = { kind: "available", info };
      } else {
        phase = { kind: "idle" };
        toastStore.notify({
          title: "You're up to date",
          body: appVersion ? `Sovereign ${appVersion}` : undefined,
        });
      }
    } catch (e) {
      // The backend soft-fails to `null` on most errors; reaching this
      // branch means something unusual (e.g., plugin not configured).
      // Show an actionable message rather than a stack trace.
      const message = e instanceof Error ? e.message : String(e);
      phase = { kind: "error", message };
    }
  }

  async function install() {
    phase = { kind: "installing" };
    try {
      await installUpdate();
      // On success the backend calls app.restart() — we never reach
      // this line. Reaching it means the install resolved without a
      // restart, which is a Tauri-version surprise; treat as error.
      phase = {
        kind: "error",
        message: "Update installed but the app did not restart. Quit and reopen manually.",
      };
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      phase = { kind: "error", message };
    }
  }
</script>

<div class="updates-section">
  <dl class="version-block">
    <dt>Installed version</dt>
    <dd>
      {#if appVersion}
        <span class="version-value">{appVersion}</span>
      {:else}
        <span class="version-pending">…</span>
      {/if}
    </dd>
  </dl>

  {#if phase.kind === "available"}
    <div class="update-banner" role="status">
      <div class="banner-head">
        <span class="banner-marker" aria-hidden="true">●</span>
        <div class="banner-text">
          <p class="banner-title">Sovereign {phase.info.version} is available</p>
          {#if phase.info.date}
            <p class="banner-meta">Published {formatDate(phase.info.date)}</p>
          {/if}
        </div>
      </div>
      {#if phase.info.body}
        <p class="banner-notes">{phase.info.body}</p>
      {/if}
      <div class="banner-actions">
        <button class="btn-install" onclick={install}>
          Download & install
        </button>
        <button class="btn-defer" onclick={() => (phase = { kind: "idle" })}>
          Later
        </button>
      </div>
    </div>

  {:else if phase.kind === "installing"}
    <div class="install-status" role="status" aria-live="polite">
      <span class="spinner" aria-hidden="true"></span>
      <span>Downloading and installing… the app will restart when complete.</span>
    </div>

  {:else if phase.kind === "error"}
    <div class="install-error" role="alert">
      <p class="error-title">Update failed</p>
      <p class="error-body">{phase.message}</p>
      <button class="btn-install" onclick={check}>Try again</button>
    </div>

  {:else}
    <div class="check-row">
      <button
        class="btn-check"
        onclick={check}
        disabled={phase.kind === "checking"}
      >
        {phase.kind === "checking" ? "Checking…" : "Check for updates"}
      </button>
      <p class="check-meta">
        Updates are served from <code>svrnme.sh</code>. The plugin verifies
        every release against a key embedded at build time — no automatic
        installs, no background downloads.
      </p>
    </div>
  {/if}
</div>

<script module lang="ts">
  function formatDate(iso: string): string {
    try {
      const d = new Date(iso);
      // Long-form to feel chancery rather than ISO-stamped.
      return d.toLocaleDateString(undefined, {
        year: "numeric",
        month: "long",
        day: "numeric",
      });
    } catch {
      return iso;
    }
  }
</script>

<style>
  .updates-section {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }

  /* ─── Version block ─── */

  .version-block {
    display: grid;
    grid-template-columns: 180px 1fr;
    align-items: baseline;
    gap: 12px;
    margin: 0;
  }
  .version-block dt {
    font-size: 0.88rem;
    color: var(--text-secondary);
    font-family: var(--font-sans);
  }
  .version-block dd {
    margin: 0;
  }
  .version-value {
    font-family: var(--font-mono);
    font-size: 0.95rem;
    color: var(--text-primary);
    letter-spacing: -0.005em;
  }
  .version-pending {
    color: var(--text-muted);
  }

  /* ─── "Check for updates" idle row ─── */

  .check-row {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-surface);
  }
  .check-meta {
    margin: 0;
    font-size: 0.84rem;
    color: var(--text-secondary);
    line-height: 1.5;
    max-width: 60ch;
  }
  .check-meta code {
    font-family: var(--font-mono);
    font-size: 0.82rem;
    color: var(--lavender-light);
    background: var(--lavender-glow);
    padding: 1px 5px;
    border-radius: 4px;
  }
  .btn-check {
    align-self: flex-start;
    padding: 8px 18px;
    background: transparent;
    color: var(--text-primary);
    border: 1px solid var(--border-bright);
    border-radius: 999px;
    font-family: var(--font-sans);
    font-size: 0.9rem;
    cursor: pointer;
    transition: border-color 120ms ease, background 120ms ease;
  }
  .btn-check:hover:not(:disabled) {
    border-color: var(--lavender);
    background: var(--lavender-dim);
  }
  .btn-check:disabled {
    opacity: 0.6;
    cursor: progress;
  }

  /* ─── Update-available banner ─── */
  /*
     Visually quotes the toast/letterpress-seal treatment: gold border,
     warm background, subtle inset highlight. Reads as "something has
     arrived" rather than "system message".
  */

  .update-banner {
    padding: 18px;
    background: var(--bg-elevated);
    border: 1px solid var(--accent);
    border-radius: var(--radius-lg);
    box-shadow:
      inset 0 1px 0 rgba(223, 192, 104, 0.18),
      0 0 32px var(--accent-glow);
  }
  .banner-head {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }
  .banner-marker {
    color: var(--accent);
    font-size: 1.2rem;
    line-height: 1;
    margin-top: 2px;
  }
  .banner-text {
    flex: 1;
    min-width: 0;
  }
  .banner-title {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 1.08rem;
    font-style: italic;
    font-weight: 500;
    color: var(--accent-light);
    letter-spacing: -0.005em;
  }
  .banner-meta {
    margin: 4px 0 0 0;
    font-size: 0.82rem;
    color: var(--text-muted);
  }
  .banner-notes {
    margin: 14px 0 0 0;
    padding-top: 14px;
    border-top: 1px solid var(--border);
    font-size: 0.88rem;
    line-height: 1.55;
    color: var(--text-secondary);
    white-space: pre-wrap;
    max-height: 240px;
    overflow-y: auto;
  }
  .banner-actions {
    display: flex;
    gap: 10px;
    margin-top: 16px;
  }
  .btn-install {
    padding: 9px 20px;
    background: var(--accent);
    color: var(--text-on-accent);
    border: 1px solid var(--accent);
    border-radius: 999px;
    font-family: var(--font-sans);
    font-size: 0.9rem;
    font-weight: 500;
    cursor: pointer;
    transition: background 120ms ease, border-color 120ms ease;
  }
  .btn-install:hover {
    background: var(--accent-hover);
    border-color: var(--accent-hover);
  }
  .btn-defer {
    padding: 9px 16px;
    background: transparent;
    color: var(--text-secondary);
    border: 1px solid transparent;
    border-radius: 999px;
    font-family: var(--font-sans);
    font-size: 0.9rem;
    cursor: pointer;
    transition: color 120ms ease, border-color 120ms ease;
  }
  .btn-defer:hover {
    color: var(--text-primary);
    border-color: var(--border-mid);
  }

  /* ─── Installing status ─── */

  .install-status {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 16px 18px;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    color: var(--text-secondary);
    font-size: 0.92rem;
  }
  .spinner {
    width: 14px;
    height: 14px;
    border: 2px solid var(--border-bright);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 800ms linear infinite;
    flex-shrink: 0;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  /* ─── Error state ─── */

  .install-error {
    padding: 16px 18px;
    background: var(--bg-surface);
    border: 1px solid var(--error);
    border-radius: var(--radius-lg);
  }
  .error-title {
    margin: 0 0 4px 0;
    font-family: var(--font-sans);
    font-size: 0.95rem;
    font-weight: 500;
    color: var(--error);
  }
  .error-body {
    margin: 0 0 12px 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
    line-height: 1.5;
  }
</style>
