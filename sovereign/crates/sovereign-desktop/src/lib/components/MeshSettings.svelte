<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    getConfig,
    meshCreate,
    meshGetState,
    meshIsRunning,
    meshLeave,
    saveConfig,
  } from "../api";
  import { joinLinkStore } from "../stores/joinLink.svelte";
  import MeshDiagnosticsPanel from "./MeshDiagnosticsPanel.svelte";
  import type {
    CreateMeshResponse,
    DesktopConfig,
    MeshStateResponse,
    MeshMember,
  } from "../types";

  // `sovereign://join/cwth-XXXX-XXXX-XXXX` with optional query params.
  // Cheap client-side guard; the real parser lives in
  // `commonwealth-discovery::deep_link::parse_deep_link`. Kept in sync
  // with `membership::generate_join_key`'s 3×4-hex-segment format.
  const JOIN_LINK_PATTERN =
    /^sovereign:\/\/join\/cwth-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}(\?.*)?$/i;

  // Paste-link form state — dev-mode bypass for the OS scheme handler.
  let joinLinkInput = $state("");
  let joinLinkError = $state("");

  // Node name — what this machine advertises to other mesh members.
  // Loaded lazily from DesktopConfig; empty string means "use system
  // hostname at join time" (the backend's `resolve_node_name`
  // helper handles that fallback).
  let config: DesktopConfig | null = $state(null);
  let nodeNameInput = $state("");
  let nodeNameSaving = $state(false);
  let nodeNameSaved = $state(false);

  async function loadConfig() {
    try {
      config = await getConfig();
      nodeNameInput = config.node_name ?? "";
    } catch (e) {
      console.error("Failed to load config for node name:", e);
    }
  }

  async function saveNodeName() {
    if (!config || nodeNameSaving) return;
    nodeNameSaving = true;
    nodeNameSaved = false;
    try {
      const next: DesktopConfig = { ...config, node_name: nodeNameInput.trim() };
      await saveConfig(next);
      config = next;
      nodeNameSaved = true;
      // Flash success indicator briefly, then hide.
      setTimeout(() => { nodeNameSaved = false; }, 2500);
    } catch (e) {
      console.error("Failed to save node name:", e);
    } finally {
      nodeNameSaving = false;
    }
  }

  // ── State ───────────────────────────────────────────────
  let running = $state(false);
  let meshState = $state<MeshStateResponse | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Create-mesh form state
  let showCreateForm = $state(false);
  let meshNameInput = $state("");
  let creating = $state(false);
  let createResult = $state<CreateMeshResponse | null>(null);
  let copyFeedback = $state("");

  // Leave-mesh confirmation
  let showLeaveConfirm = $state(false);
  let leaving = $state(false);

  let pollHandle: ReturnType<typeof setInterval> | null = null;

  // ── Lifecycle ──────────────────────────────────────────
  onMount(async () => {
    await refresh();
    await loadConfig();
    // Poll mesh state every 5s while running so the member list and
    // contribution numbers stay current without WebSocket plumbing.
    pollHandle = setInterval(async () => {
      if (running) {
        try {
          meshState = await meshGetState();
        } catch (e) {
          console.error("Failed to refresh mesh state:", e);
        }
      }
    }, 5000);
  });

  onDestroy(() => {
    if (pollHandle) clearInterval(pollHandle);
  });

  async function refresh() {
    loading = true;
    error = null;
    try {
      running = await meshIsRunning();
      if (running) {
        meshState = await meshGetState();
      } else {
        meshState = null;
      }
    } catch (e) {
      error = `Failed to load mesh state: ${e}`;
    }
    loading = false;
  }

  // ── Create flow ────────────────────────────────────────
  function openCreateForm() {
    showCreateForm = true;
    meshNameInput = "";
    createResult = null;
  }

  /** Validate then hand a pasted `sovereign://join/...` URL to the
   *  `joinLinkStore` — App.svelte picks it up and pops
   *  `MeshJoinDialog`, reusing the exact flow the OS deep-link
   *  listener drives in release builds. */
  function submitJoinLink() {
    joinLinkError = "";
    const link = joinLinkInput.trim();
    if (!link) {
      joinLinkError = "Paste a join link first.";
      return;
    }
    if (!JOIN_LINK_PATTERN.test(link)) {
      joinLinkError =
        "That doesn't look like a Sovereign join link. Expected `sovereign://join/cwth-xxxx-xxxx-xxxx`.";
      return;
    }
    joinLinkStore.set(link);
    joinLinkInput = "";
  }

  function cancelCreate() {
    showCreateForm = false;
    meshNameInput = "";
  }

  async function submitCreate() {
    const name = meshNameInput.trim();
    if (!name || creating) return;
    creating = true;
    error = null;
    try {
      createResult = await meshCreate(name);
      // Daemon is now running. Refresh state so the success view shows
      // the live members + share link.
      await refresh();
    } catch (e) {
      error = `Failed to create mesh: ${e}`;
    }
    creating = false;
  }

  function dismissCreateResult() {
    createResult = null;
    showCreateForm = false;
  }

  // ── Copy / share helpers ───────────────────────────────
  async function copyToClipboard(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copyFeedback = "Copied!";
      setTimeout(() => (copyFeedback = ""), 1500);
    } catch (e) {
      copyFeedback = "Copy failed";
      setTimeout(() => (copyFeedback = ""), 1500);
    }
  }

  // ── Leave flow ─────────────────────────────────────────
  function openLeaveConfirm() {
    showLeaveConfirm = true;
  }

  function cancelLeave() {
    showLeaveConfirm = false;
  }

  async function confirmLeave() {
    if (leaving) return;
    leaving = true;
    error = null;
    try {
      await meshLeave();
      showLeaveConfirm = false;
      await refresh();
    } catch (e) {
      error = `Failed to leave mesh: ${e}`;
    }
    leaving = false;
  }

  // ── Helpers ────────────────────────────────────────────
  function statusDot(member: MeshMember): string {
    switch (member.status) {
      case "online":
        return "online";
      case "busy":
        return "busy";
      case "away":
        return "away";
      default:
        return "offline";
    }
  }

  function bar(level: number): string {
    // 0–5 dots representation, e.g. ●●●○○ for level 3.
    const filled = Math.max(0, Math.min(5, level));
    return "●".repeat(filled) + "○".repeat(5 - filled);
  }
</script>

<div class="mesh-settings">
  {#if loading}
    <div class="muted">Checking mesh status…</div>
  {:else if error}
    <div class="alert error">{error}</div>
  {/if}

  <!-- ─── Idle state: not in a mesh ─────────────────────── -->
  {#if !running && !showCreateForm}
    <div class="empty">
      <p class="lead">
        Pool AI resources with people you trust. A mesh shares spare
        compute and knowledge bases across machines so everyone gets
        better answers.
      </p>
      <div class="actions">
        <button class="primary" onclick={openCreateForm}>Create a mesh</button>
      </div>

      <div class="join-section">
        <p class="section-label">Joining a friend's mesh?</p>
        <p class="hint">
          Open the <code>sovereign://join/…</code> link they shared, or
          paste it below (useful when running from source).
        </p>
        <div class="join-row">
          <input
            type="text"
            class="join-input"
            placeholder="sovereign://join/cwth-xxxx-xxxx-xxxx"
            bind:value={joinLinkInput}
            onkeydown={(e) => e.key === "Enter" && submitJoinLink()}
          />
          <button class="primary" onclick={submitJoinLink}>Preview</button>
        </div>
        {#if joinLinkError}
          <div class="alert error small">{joinLinkError}</div>
        {/if}
      </div>
    </div>
  {/if}

  <!-- ─── Create form ───────────────────────────────────── -->
  {#if showCreateForm && !createResult}
    <div class="form-card">
      <h4>Create a Community Mesh</h4>
      <p class="muted">
        Pick a name your group will recognize. You'll get a link to share.
      </p>
      <label>
        <span>Mesh name</span>
        <input
          type="text"
          placeholder="Lab Squad"
          bind:value={meshNameInput}
          onkeydown={(e) => e.key === "Enter" && submitCreate()}
        />
      </label>
      <div class="form-actions">
        <button class="secondary" onclick={cancelCreate} disabled={creating}>
          Cancel
        </button>
        <button
          class="primary"
          onclick={submitCreate}
          disabled={creating || !meshNameInput.trim()}
        >
          {creating ? "Creating…" : "Create"}
        </button>
      </div>
    </div>
  {/if}

  <!-- ─── Create success: share dialog ──────────────────── -->
  {#if createResult}
    <div class="share-card">
      <h4>"{createResult.mesh_name}" is live</h4>
      <p class="muted">
        Share this link with people you trust. They'll tap it once and
        be in the mesh — no terminal, no setup.
      </p>
      <div class="link-row">
        <code class="link">{createResult.join_link}</code>
        <button class="copy-btn" onclick={() => copyToClipboard(createResult!.join_link)}>
          Copy
        </button>
      </div>
      {#if copyFeedback}
        <span class="copy-feedback">{copyFeedback}</span>
      {/if}
      <details class="advanced">
        <summary>Or share the join key directly</summary>
        <div class="link-row">
          <code class="link">{createResult.join_key}</code>
          <button class="copy-btn" onclick={() => copyToClipboard(createResult!.join_key)}>
            Copy
          </button>
        </div>
      </details>
      <div class="form-actions">
        <button class="primary" onclick={dismissCreateResult}>Done</button>
      </div>
    </div>
  {/if}

  <!-- ─── Active mesh: status + members ─────────────────── -->
  {#if running && meshState && !createResult}
    <div class="status-card">
      <div class="status-header">
        <div>
          <h4>{meshState.status.name}</h4>
          <div class="status-line">
            <span class="dot online"></span>
            {meshState.status.members_online} of
            {meshState.status.members_total} online
            {#if meshState.status.model_name}
              · Model: {meshState.status.model_name}
            {/if}
          </div>
        </div>
        <button class="leave-btn" onclick={openLeaveConfirm}>Leave</button>
      </div>

      {#if meshState.status.knowledge_corpora.length > 0}
        <div class="corpora-row">
          <span class="label">Shared knowledge:</span>
          {#each meshState.status.knowledge_corpora as corpus}
            <span class="corpus-pill">{corpus}</span>
          {/each}
        </div>
      {/if}
    </div>

    <div class="members-card">
      <h5>Members</h5>
      {#if meshState.members.length === 0}
        <div class="muted">No members yet. Share your join link.</div>
      {:else}
        <ul class="members-list">
          {#each meshState.members as member}
            <li class="member-row">
              <span class="dot {statusDot(member)}"></span>
              <span class="member-name">
                {member.name}
                {#if member.is_self}<em>(you)</em>{/if}
              </span>
              {#if member.contribution_label}
                <span class="contribution-bar">{bar(member.contribution_level)}</span>
                <span class="contribution-label">{member.contribution_label}</span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    {#if meshState.contribution}
      <div class="contribution-card">
        <h5>Your Contribution</h5>
        <p class="contribution-summary">
          {meshState.contribution.summary_text}
        </p>
        <details>
          <summary>Show details</summary>
          <dl class="contribution-details">
            <dt>Compute contributed</dt>
            <dd>{meshState.contribution.compute_hours_contributed.toFixed(1)} hrs</dd>
            <dt>Compute used</dt>
            <dd>{meshState.contribution.compute_hours_used.toFixed(1)} hrs</dd>
            <dt>Storage hosted</dt>
            <dd>{meshState.contribution.storage_hosted_gb.toFixed(0)} GB</dd>
            <dt>Bandwidth served</dt>
            <dd>{meshState.contribution.bandwidth_served_gb.toFixed(0)} GB</dd>
          </dl>
        </details>
      </div>
    {/if}
  {/if}

  <!-- ─── Leave confirmation modal ──────────────────────── -->
  {#if showLeaveConfirm}
    <div
      class="modal-backdrop"
      onclick={cancelLeave}
      onkeydown={(e) => e.key === "Escape" && cancelLeave()}
      role="presentation"
    >
      <div
        class="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="leave-title"
        tabindex="-1"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
      >
        <h4 id="leave-title">Leave this mesh?</h4>
        <p>
          You'll stop contributing to and using shared resources.
          You can rejoin later with the same link.
        </p>
        <div class="form-actions">
          <button class="secondary" onclick={cancelLeave} disabled={leaving}>
            Cancel
          </button>
          <button class="danger" onclick={confirmLeave} disabled={leaving}>
            {leaving ? "Leaving…" : "Leave"}
          </button>
        </div>
      </div>
    </div>
  {/if}

  <!-- Node name — shown to other mesh members in their rosters.
       Persisted to DesktopConfig so it survives restarts. Empty
       means "use the system hostname"; backend strips `.local`. -->
  {#if config}
    <div class="node-name-card">
      <label class="node-name-label" for="node-name-input">
        Your node name
        <span class="node-name-hint">
          How you appear to other members. Leave blank to use this
          machine's hostname. Takes effect on the next mesh
          create/join.
        </span>
      </label>
      <div class="node-name-row">
        <input
          id="node-name-input"
          type="text"
          class="node-name-input"
          placeholder="e.g. Alex's MacBook"
          bind:value={nodeNameInput}
          onkeydown={(e) => e.key === "Enter" && saveNodeName()}
          disabled={nodeNameSaving}
        />
        <button
          class="primary"
          onclick={saveNodeName}
          disabled={nodeNameSaving || nodeNameInput === (config.node_name ?? "")}
        >
          {nodeNameSaving ? "Saving…" : "Save"}
        </button>
      </div>
      {#if nodeNameSaved}
        <div class="node-name-saved">Saved. Applies next time you create or join a mesh.</div>
      {/if}
    </div>
  {/if}

  <!-- Network diagnostics — always shown so the user can see
       whether their daemon is up and what peers mDNS has found. -->
  <MeshDiagnosticsPanel />
</div>

<style>
  .mesh-settings {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .muted {
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  .alert {
    padding: 10px 12px;
    border-radius: var(--radius);
    font-size: 0.85rem;
  }

  .alert.error {
    background: rgba(220, 60, 60, 0.08);
    color: var(--error);
    border: 1px solid rgba(220, 60, 60, 0.25);
  }

  .empty {
    border: 1px dashed var(--border);
    border-radius: var(--radius);
    padding: 20px;
  }

  .lead {
    color: var(--text-secondary);
    line-height: 1.5;
    margin-bottom: 16px;
  }

  .actions {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .hint {
    font-size: 0.8rem;
    color: var(--text-muted);
    line-height: 1.5;
  }

  .hint code {
    background: var(--bg-input);
    padding: 1px 5px;
    border-radius: 3px;
    font-size: 0.78rem;
  }

  .join-section {
    margin-top: 20px;
    padding-top: 16px;
    border-top: 1px dashed var(--border);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .section-label {
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-muted);
  }

  .join-row {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }

  .join-input {
    flex: 1;
    padding: 8px 10px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.85rem;
    outline: none;
  }
  .join-input:focus {
    border-color: var(--accent);
  }

  .alert.small {
    font-size: 0.78rem;
    padding: 6px 10px;
  }

  /* ── Node name card ───────────────────────────────── */
  .node-name-card {
    margin-top: 20px;
    padding: 14px 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .node-name-label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .node-name-hint {
    font-size: 0.78rem;
    font-weight: 400;
    letter-spacing: normal;
    text-transform: none;
    color: var(--text-muted);
    line-height: 1.4;
  }
  .node-name-row {
    display: flex;
    gap: 8px;
  }
  .node-name-input {
    flex: 1;
    padding: 8px 10px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 0.9rem;
    outline: none;
  }
  .node-name-input:focus {
    border-color: var(--accent);
  }
  .node-name-saved {
    font-size: 0.78rem;
    color: var(--success, #22c55e);
  }

  /* ── Buttons ─────────────────────────────────────── */
  button.primary {
    padding: 9px 18px;
    background: var(--accent);
    color: var(--text-on-accent);
    border-radius: var(--radius);
    font-weight: 500;
    transition: background 0.2s;
  }

  button.primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  button.primary:disabled {
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

  button.danger {
    padding: 9px 18px;
    background: var(--error);
    color: var(--text-on-accent);
    border-radius: var(--radius);
    font-weight: 500;
  }

  button.danger:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .leave-btn {
    padding: 5px 12px;
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 0.8rem;
  }

  .leave-btn:hover {
    color: var(--error);
    border-color: var(--error);
  }

  /* ── Form card ───────────────────────────────────── */
  .form-card,
  .share-card,
  .status-card,
  .members-card,
  .contribution-card {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px 18px;
    background: var(--bg-secondary);
  }

  .form-card h4,
  .share-card h4,
  .status-card h4 {
    font-size: 0.95rem;
    font-weight: 600;
    margin-bottom: 8px;
  }

  .members-card h5,
  .contribution-card h5 {
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-muted);
    font-weight: 600;
    margin-bottom: 10px;
  }

  .form-card label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 14px 0;
  }

  .form-card label span {
    font-size: 0.8rem;
    color: var(--text-secondary);
  }

  .form-card input {
    padding: 8px 12px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
  }

  .form-card input:focus {
    border-color: var(--accent);
  }

  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }

  /* ── Share card ──────────────────────────────────── */
  .link-row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 12px 0;
  }

  .link {
    flex: 1;
    padding: 10px 12px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-family: ui-monospace, SFMono-Regular, monospace;
    font-size: 0.78rem;
    overflow-x: auto;
    white-space: nowrap;
  }

  .copy-btn {
    padding: 8px 14px;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 0.8rem;
  }

  .copy-btn:hover {
    background: var(--bg-input);
  }

  .copy-feedback {
    font-size: 0.78rem;
    color: var(--success);
    margin-left: 8px;
  }

  .advanced {
    margin-top: 8px;
  }

  .advanced summary {
    font-size: 0.8rem;
    color: var(--text-muted);
    cursor: pointer;
  }

  /* ── Status card ─────────────────────────────────── */
  .status-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
  }

  .status-line {
    font-size: 0.8rem;
    color: var(--text-muted);
    margin-top: 4px;
  }

  .dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    margin-right: 4px;
  }

  .dot.online {
    background: var(--success);
  }

  .dot.busy {
    background: #d4a017;
  }

  .dot.away {
    background: #888;
  }

  .dot.offline {
    background: var(--border);
  }

  .corpora-row {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .label {
    font-size: 0.78rem;
    color: var(--text-muted);
    margin-right: 4px;
  }

  .corpus-pill {
    padding: 2px 8px;
    background: var(--bg-input);
    border-radius: 10px;
    font-size: 0.75rem;
    color: var(--text-secondary);
  }

  /* ── Members ─────────────────────────────────────── */
  .members-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .member-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 0;
    font-size: 0.85rem;
    border-bottom: 1px solid var(--border);
  }

  .member-row:last-child {
    border-bottom: none;
  }

  .member-name {
    flex: 1;
    color: var(--text-primary);
  }

  .member-name em {
    font-style: normal;
    color: var(--text-muted);
    font-size: 0.78rem;
    margin-left: 4px;
  }

  .contribution-bar {
    font-family: ui-monospace, SFMono-Regular, monospace;
    color: var(--accent);
    font-size: 0.78rem;
    letter-spacing: 1px;
  }

  .contribution-label {
    color: var(--text-muted);
    font-size: 0.75rem;
  }

  /* ── Contribution ────────────────────────────────── */
  .contribution-summary {
    font-size: 0.85rem;
    color: var(--text-secondary);
    line-height: 1.5;
    margin-bottom: 8px;
  }

  .contribution-card details summary {
    font-size: 0.78rem;
    color: var(--text-muted);
    cursor: pointer;
  }

  .contribution-details {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 16px;
    margin-top: 12px;
    font-size: 0.8rem;
  }

  .contribution-details dt {
    color: var(--text-muted);
  }

  .contribution-details dd {
    margin: 0;
    color: var(--text-primary);
    font-variant-numeric: tabular-nums;
  }

  /* ── Modal ───────────────────────────────────────── */
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 20px 24px;
    max-width: 380px;
    width: 90%;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
  }

  .modal h4 {
    font-size: 1rem;
    font-weight: 600;
    margin-bottom: 8px;
  }

  .modal p {
    color: var(--text-secondary);
    font-size: 0.85rem;
    line-height: 1.5;
    margin-bottom: 16px;
  }
</style>
