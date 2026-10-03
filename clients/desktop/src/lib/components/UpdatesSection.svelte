<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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

    - `check_for_update` has three distinct outcomes and we render each
      differently (they are NOT conflated — conflating "check failed" with
      "up to date" hid three real updater bugs, 2026-07-15):
        · UpdateInfo  → in-section banner (see below)
        · null        → genuinely up to date; passive toast, no modal
        · throws      → the check failed (offline / endpoint down); a calm,
                        retryable "Couldn't check for updates" notice

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
    | { kind: "error"; title: string; message: string };

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
          body: appVersion ? `svrnmesh ${appVersion}` : undefined,
        });
      }
    } catch (e) {
      // A failed check now surfaces here (the backend no longer masks errors
      // as "up to date" — see update_commands.rs). Render a calm, retryable
      // notice, NOT a scary "update failed" — the check just couldn't reach
      // the service; nothing is broken on the user's machine.
      const message = e instanceof Error ? e.message : String(e);
      phase = { kind: "error", title: "Couldn't check for updates", message };
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
        title: "Update failed",
        message: "Update installed but the app did not restart. Quit and reopen manually.",
      };
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      phase = { kind: "error", title: "Update failed", message };
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
          <p class="banner-title">svrnmesh {phase.info.version} is available</p>
          {#if phase.info.date}
            {@const published = formatDate(phase.info.date)}
            {#if published}
              <p class="banner-meta">Published {published}</p>
            {/if}
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
      <span>Downloading and installing — the app will restart when it's done.</span>
    </div>

  {:else if phase.kind === "error"}
    <div class="install-error" role="alert">
      <p class="error-title">{phase.title}</p>
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
        Updates come from <code>svrnme.sh</code>. Each release is signed and
        verified on this machine before it installs — nothing happens in the
        background.
      </p>
    </div>
  {/if}
</div>

<script module lang="ts">
  // Returns null when the date can't be parsed. `new Date(bad)` yields an
  // Invalid Date object rather than throwing, so a plain try/catch never
  // fires and toLocaleDateString renders the literal "Invalid Date". Older
  // backends serialized the publish date via time::OffsetDateTime's Display
  // (`2026-07-15 10:45:13.0 +00:00:00`), which JS can't parse — for those we
  // hide the line entirely instead of showing a broken date.
  function formatDate(iso: string): string | null {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return null;
    // Long-form to feel chancery rather than ISO-stamped.
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "long",
      day: "numeric",
    });
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
