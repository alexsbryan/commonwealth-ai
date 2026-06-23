<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { dialogFocus } from "@sovereign/chat-ui";
  import {
    meshPreviewJoinLink,
    meshJoin,
    meshIsRunning,
    meshLeave,
  } from "../api";
  import type { JoinConfirmation } from "../types";

  interface Props {
    /** The raw `sovereign://join/...` URL the OS handed us. */
    link: string;
    /** Called when the user dismisses the dialog (joined or cancelled). */
    onClose: () => void;
    /** Called after a successful join with the mesh name. */
    onJoined?: (meshName: string) => void;
  }

  let { link, onClose, onJoined }: Props = $props();

  let confirmation = $state<JoinConfirmation | null>(null);
  let parseError = $state<string | null>(null);
  let joining = $state(false);
  let joinError = $state<string | null>(null);
  /** True when this user is already in a mesh — joining the new one
   *  will leave the current one first. Drives an extra "you'll
   *  leave X" line in the dialog so the action isn't surprising. */
  let alreadyInMesh = $state(false);

  onMount(async () => {
    try {
      confirmation = await meshPreviewJoinLink(link);
    } catch (e) {
      parseError = `${e}`;
    }
    // Probe whether we're already in a mesh — drives the
    // "leave-first" copy + the extra meshLeave call below. We don't
    // call meshGetState here to keep the dialog's load fast; the
    // boolean is sufficient for the UX hint.
    try {
      alreadyInMesh = await meshIsRunning();
    } catch {
      // If we can't tell, assume we're not in one — joining will
      // surface a clear "AlreadyRunning" error from the daemon
      // rather than producing a stale stub.
      alreadyInMesh = false;
    }
  });

  async function confirmJoin() {
    if (joining) return;
    joining = true;
    joinError = null;
    try {
      // `join_mesh` on the daemon auto-leaves any existing mesh
      // (including the silent solo mesh `sovereign setup` creates on
      // first boot). Calling `meshLeave` here first would race with
      // the launchd-managed daemon's auto-restart — the HTTP listener
      // dies between the two round-trips and the join arrives at
      // connection-refused or a freshly-recreated solo mesh. One
      // atomic HTTP call to /v1/mesh/join is the load-bearing path.
      const result = await meshJoin(link);
      if (onJoined) onJoined(result.mesh_name);
      onClose();
    } catch (e) {
      joinError = `${e}`;
    }
    joining = false;
  }
</script>

<!-- Presentation backdrop: click-outside is a convenience; keyboard
     users dismiss via Escape (use:dialogFocus on the modal) or the
     visible Cancel button. -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="modal-backdrop" onclick={onClose} role="presentation">
  <!-- use:dialogFocus owns Escape, the Tab trap, and focus restore on
       close. tabindex="-1" stays so focus can pin to the dialog during
       the "Reading join link…" state (no tabbable controls yet). -->
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-labelledby="join-title"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    use:dialogFocus={{ onEscape: onClose }}
  >
    {#if parseError}
      <h4 id="join-title">Invalid Join Link</h4>
      <p class="error">{parseError}</p>
      <p class="muted code">{link}</p>
      <div class="actions">
        <button class="primary" onclick={onClose}>Close</button>
      </div>
    {:else if !confirmation}
      <p class="muted">Reading join link…</p>
    {:else}
      <h4 id="join-title">Join "{confirmation.mesh_name}"?</h4>
      {#if confirmation.invited_by}
        <p class="lead">
          {confirmation.invited_by} invited you to a community mesh. Your
          machines will share AI resources so everyone gets smarter answers.
        </p>
      {:else}
        <p class="lead">
          You've been invited to a community mesh. Your machines will share
          AI resources so everyone gets smarter answers.
        </p>
      {/if}

      <div class="info-row">
        <h5>What you share</h5>
        <ul>
          <li>Spare computing power (when your machine isn't busy)</li>
          <li>Knowledge base indexes (Wikipedia, etc.)</li>
        </ul>
      </div>

      <div class="info-row">
        <h5>What stays private</h5>
        <ul>
          <li>Your conversations</li>
          <li>Your personal files</li>
          <li>Your memories and notes</li>
        </ul>
      </div>

      {#if alreadyInMesh}
        <p class="leave-first">
          Joining will leave the mesh you're currently in.
        </p>
      {/if}

      {#if confirmation.relay_hint}
        <p class="muted small">
          Connecting via relay: <code>{confirmation.relay_hint}</code>
        </p>
      {/if}

      {#if confirmation.iroh_dial}
        <p class="encrypted-note small">
          <strong>Encrypted mesh.</strong> Your join runs over an encrypted,
          key-verified connection to the founder.
        </p>
      {/if}

      {#if joinError}
        <p class="error">Failed to join: {joinError}</p>
      {/if}

      <div class="actions">
        <button class="secondary" onclick={onClose} disabled={joining}>
          Not Now
        </button>
        <button class="primary" onclick={confirmJoin} disabled={joining}>
          {joining ? "Joining…" : "Join"}
        </button>
      </div>
    {/if}
  </div>
</div>

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 200;
  }

  .modal {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 24px 28px;
    max-width: 460px;
    width: 90%;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  }

  h4 {
    font-size: 1.1rem;
    font-weight: 600;
    margin-bottom: 12px;
  }

  h5 {
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-muted);
    font-weight: 600;
    margin-bottom: 6px;
  }

  .lead {
    color: var(--text-secondary);
    line-height: 1.5;
    margin-bottom: 16px;
  }

  .info-row {
    margin-bottom: 14px;
  }

  .info-row ul {
    list-style: none;
    padding: 0;
    margin: 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
  }

  .info-row li {
    padding: 3px 0;
  }

  .info-row li::before {
    content: "•";
    color: var(--text-muted);
    margin-right: 8px;
  }

  .muted {
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  .muted.small {
    font-size: 0.78rem;
    margin-top: 8px;
  }

  .muted.code {
    font-family: var(--font-mono);
    background: var(--bg-input);
    padding: 6px 10px;
    border-radius: var(--radius);
    word-break: break-all;
    margin-top: 8px;
  }

  code {
    font-family: var(--font-mono);
    font-size: 0.78rem;
  }

  .error {
    color: var(--error);
    font-size: 0.85rem;
    margin: 8px 0;
  }

  .leave-first {
    margin: 8px 0;
    padding: 8px 12px;
    background: rgba(220, 165, 60, 0.08);
    border: 1px solid rgba(220, 165, 60, 0.25);
    color: var(--text-primary);
    border-radius: var(--radius);
    font-size: 0.82rem;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 20px;
  }

  button.primary {
    padding: 9px 18px;
    background: var(--accent);
    color: var(--text-on-accent);
    border-radius: var(--radius);
    font-weight: 500;
  }

  button.primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  button.primary:disabled,
  button.secondary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.secondary {
    padding: 9px 18px;
    background: var(--bg-surface);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  button.secondary:hover:not(:disabled) {
    background: var(--bg-input);
  }
</style>
