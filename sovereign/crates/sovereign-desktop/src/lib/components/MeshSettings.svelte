<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    withRelay,
    relayLabel,
    formatBytes,
    formatTokens,
    formatGb,
    statusDot,
  } from "./meshFormat";
  import {
    getConfig,
    meshClearPeerPreference,
    meshCreate,
    meshGetContributions,
    meshGetState,
    meshIsRunning,
    meshLeave,
    meshListPeerPreferences,
    meshRelayCandidates,
    meshRotateInvite,
    meshSetPeerPreference,
    saveConfig,
    suggestNodeName,
  } from "../api";
  import { joinLinkStore } from "../stores/joinLink.svelte";
  import MeshDiagnosticsPanel from "./MeshDiagnosticsPanel.svelte";
  import type {
    CreateMeshResponse,
    DesktopConfig,
    MeshStateResponse,
    NodeContributionsDto,
    PeerPreferenceDto,
    RelayCandidate,
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

  async function rollNodeName() {
    try {
      // Just refresh the input field — the user still has to press
      // Save to commit. Keeps "what changes when" obvious.
      nodeNameInput = await suggestNodeName();
    } catch (e) {
      console.error("Failed to generate suggested node name:", e);
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

  // Rotate-invite confirmation. Rotating revokes the link the user
  // already shared, which is destructive enough to warrant an
  // explicit confirm — accidentally clicking it would lock anyone
  // mid-share out of the mesh until they got the new link.
  let showRotateConfirm = $state(false);
  let rotating = $state(false);
  let rotateError = $state<string | null>(null);

  // Relay candidates for cross-network invites (Tailscale / LAN
  // address that the joiner can dial directly when mDNS won't
  // reach them). Lazy-loaded the first time the user opens the
  // "Add a relay…" reveal so we don't hit the daemon on every
  // settings open. `null` = not yet loaded; `[]` = loaded but
  // empty (no detected interfaces — UI hides the picker).
  let relayCandidates = $state<RelayCandidate[] | null>(null);
  let selectedRelay = $state<string | null>(null);
  let relayLoading = $state(false);

  /** Lazy-load relay candidates and pre-select the recommended one
   *  the first time the picker opens. Cached for the rest of the
   *  session — they don't change unless the network does, and the
   *  user can always close+reopen settings to refresh. */
  async function ensureRelayCandidates() {
    if (relayCandidates !== null || relayLoading) return;
    relayLoading = true;
    try {
      const list = await meshRelayCandidates();
      relayCandidates = list;
      const recommended = list.find((c) => c.recommended);
      if (recommended) selectedRelay = recommended.url_fragment;
    } catch (e) {
      console.error("Failed to load relay candidates:", e);
      relayCandidates = [];
    }
    relayLoading = false;
  }

  // ── Mesh Health: dimensional contributions + peer preferences ──
  //
  // The legacy single-score `contribution_level` per member is gone.
  // What replaces it is three separate counters per peer (Inference /
  // Knowledge / Network) — incommensurable on purpose, per the spec's
  // §2.2 anti-ranking constraint. We keep them in a Map keyed by
  // node_id so the member-row template can do an O(1) lookup.
  let contributions = $state<Map<string, NodeContributionsDto>>(new Map());
  let preferences = $state<Map<string, PeerPreferenceDto>>(new Map());
  let windowDays = $state(30);

  // Per-peer draft state for the affinity-multiplier control. Keyed by
  // node_id. Only populated lazily — the row that the operator opens
  // gets an entry seeded from the saved preference (or 1.0 / null
  // when nothing is set). Saving merges back into `preferences` and
  // collapses the row.
  let prefDraft = $state<Record<string, { multiplier: number; reason: string }>>({});
  let prefSaving = $state<Record<string, boolean>>({});
  let prefError = $state<Record<string, string | null>>({});

  /** Refresh dimensional contributions + peer preferences. Called
   *  alongside `meshGetState` on mount + during the 5s poll. Errors
   *  are swallowed to console — a bad refresh shouldn't blank the
   *  member list (§9: degrade visibly, not silently). */
  async function refreshMeshHealth() {
    try {
      const list = (await meshGetContributions()) ?? [];
      const next = new Map<string, NodeContributionsDto>();
      for (const row of list) {
        next.set(row.node_id, row);
        windowDays = row.window_days;
      }
      contributions = next;
    } catch (e) {
      console.error("Failed to refresh contributions:", e);
    }
    try {
      const list = (await meshListPeerPreferences()) ?? [];
      const next = new Map<string, PeerPreferenceDto>();
      for (const p of list) next.set(p.node_id, p);
      preferences = next;
    } catch (e) {
      console.error("Failed to refresh peer preferences:", e);
    }
  }

  function ensurePrefDraft(nodeId: string) {
    if (prefDraft[nodeId]) return;
    const existing = preferences.get(nodeId);
    prefDraft = {
      ...prefDraft,
      [nodeId]: {
        multiplier: existing?.multiplier ?? 1.0,
        reason: existing?.reason ?? "",
      },
    };
  }

  async function savePeerPreference(nodeId: string) {
    const draft = prefDraft[nodeId];
    if (!draft) return;
    prefSaving = { ...prefSaving, [nodeId]: true };
    prefError = { ...prefError, [nodeId]: null };
    try {
      await meshSetPeerPreference(
        nodeId,
        draft.multiplier,
        draft.reason.trim() === "" ? null : draft.reason.trim(),
      );
      await refreshMeshHealth();
    } catch (e) {
      prefError = { ...prefError, [nodeId]: `${e}` };
    }
    prefSaving = { ...prefSaving, [nodeId]: false };
  }

  async function clearPeerPreference(nodeId: string) {
    prefSaving = { ...prefSaving, [nodeId]: true };
    prefError = { ...prefError, [nodeId]: null };
    try {
      await meshClearPeerPreference(nodeId);
      await refreshMeshHealth();
      // Drop the draft so the next open seeds from the (now-cleared)
      // preferences map. Without this, the slider stays at the old
      // value because the draft entry is sticky.
      const { [nodeId]: _, ...rest } = prefDraft;
      prefDraft = rest;
    } catch (e) {
      prefError = { ...prefError, [nodeId]: `${e}` };
    }
    prefSaving = { ...prefSaving, [nodeId]: false };
  }

  let pollHandle: ReturnType<typeof setInterval> | null = null;

  // ── Lifecycle ──────────────────────────────────────────
  onMount(async () => {
    await refresh();
    await loadConfig();
    if (running) {
      await refreshMeshHealth();
    }
    // Poll mesh state every 5s while running so the member list and
    // contribution numbers stay current without WebSocket plumbing.
    // The dimensional contributions + peer-preferences refresh hangs
    // off the same tick — one source of truth for "how often the
    // mesh-health view ages."
    pollHandle = setInterval(async () => {
      if (!running) return;
      try {
        meshState = await meshGetState();
      } catch (e) {
        console.error("Failed to refresh mesh state:", e);
      }
      await refreshMeshHealth();
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
        await refreshMeshHealth();
      } else {
        meshState = null;
        contributions = new Map();
        preferences = new Map();
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

  // ── Rotate flow ────────────────────────────────────────
  function openRotateConfirm() {
    rotateError = null;
    showRotateConfirm = true;
  }

  function cancelRotate() {
    showRotateConfirm = false;
  }

  async function confirmRotate() {
    if (rotating) return;
    rotating = true;
    rotateError = null;
    try {
      await meshRotateInvite();
      // Pull fresh state so the invite-card re-renders with the new
      // link in place. Without this, the user clicks "Rotate" and
      // the displayed link doesn't change until the 5s poll fires.
      meshState = await meshGetState();
      showRotateConfirm = false;
    } catch (e) {
      rotateError = `Failed to rotate invite: ${e}`;
    }
    rotating = false;
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
          Open the <code>sovereign://join/…</code> link they sent, or
          paste it below.
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
      <h4>Create a mesh</h4>
      <p class="muted">
        Give it a name your group will recognise. You'll get a link to share.
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
        Send this link to people you trust. One tap and they're in — no
        terminal, no setup.
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

    <!-- Invite card — present whenever the daemon has cached the
         plaintext key. Hidden for legacy meshes (no join_key.secret
         from before this feature shipped); the user can click
         "Rotate" to recover an inviteable link. -->
    {#if meshState.status.join_link}
      {@const enrichedLink = withRelay(meshState.status.join_link, selectedRelay)}
      <div class="invite-card">
        <h5>Invite link</h5>
        <p class="muted">
          Send this to anyone you want in the mesh. One tap, they're in.
        </p>
        <div class="link-row">
          <code class="link">{enrichedLink}</code>
          <button
            class="copy-btn"
            onclick={() => copyToClipboard(enrichedLink)}
          >
            Copy
          </button>
        </div>
        {#if copyFeedback}
          <span class="copy-feedback">{copyFeedback}</span>
        {/if}

        <details
          class="relay-picker"
          ontoggle={(e) => (e.currentTarget as HTMLDetailsElement).open && ensureRelayCandidates()}
        >
          <summary>
            Add a relay for friends not on your network
            {#if selectedRelay}
              <span class="relay-active">· active</span>
            {/if}
          </summary>
          <p class="hint">
            Local discovery only finds people on the same Wi-Fi. Pick a
            routable address — Tailscale is the easy one — so people on
            other networks can reach you.
          </p>
          {#if relayLoading}
            <p class="muted">Loading addresses…</p>
          {:else if relayCandidates && relayCandidates.length === 0}
            <p class="muted">
              No reachable addresses detected. Connect to a network or
              install Tailscale, then reopen this panel.
            </p>
          {:else if relayCandidates}
            <ul class="relay-list">
              <li>
                <label>
                  <input
                    type="radio"
                    name="relay"
                    value=""
                    checked={!selectedRelay}
                    onchange={() => (selectedRelay = null)}
                  />
                  <span class="relay-label">No relay (LAN-only invite)</span>
                </label>
              </li>
              {#each relayCandidates as cand}
                <li>
                  <label>
                    <input
                      type="radio"
                      name="relay"
                      value={cand.url_fragment}
                      checked={selectedRelay === cand.url_fragment}
                      onchange={() => (selectedRelay = cand.url_fragment)}
                    />
                    <span class="relay-label">
                      {relayLabel(cand.kind)}
                      {#if cand.recommended}<em class="badge">Recommended</em>{/if}
                    </span>
                    <code class="relay-frag">{cand.url_fragment}</code>
                  </label>
                </li>
              {/each}
            </ul>
          {/if}
        </details>

        {#if meshState.status.join_key}
          <details class="advanced">
            <summary>Or share the bare key</summary>
            <div class="link-row">
              <code class="link">{meshState.status.join_key}</code>
              <button
                class="copy-btn"
                onclick={() => copyToClipboard(meshState!.status.join_key!)}
              >
                Copy
              </button>
            </div>
          </details>
        {/if}
        <button class="ghost rotate-btn" onclick={openRotateConfirm}>
          Rotate link (revokes the old one)
        </button>
      </div>
    {:else}
      <div class="invite-card invite-missing">
        <h5>Invite link</h5>
        <p class="muted">
          This mesh predates invite caching. Rotate to generate a fresh
          share link — existing members stay connected.
        </p>
        <button class="primary" onclick={openRotateConfirm}>
          Generate new invite link
        </button>
      </div>
    {/if}

    <div class="members-card">
      <div class="members-header">
        <h5>Members</h5>
        <p class="hint">
          What each peer has contributed over the past {windowDays} days.
          Three separate dimensions, kept apart on purpose — "good peer"
          doesn't reduce to one number.
        </p>
      </div>
      {#if meshState.members.length === 0}
        <div class="muted">No members yet. Share your join link.</div>
      {:else}
        <ul class="members-list">
          {#each meshState.members as member}
            {@const c = contributions.get(member.node_id)}
            {@const pref = preferences.get(member.node_id)}
            <li class="member-row" data-node-id={member.node_id}>
              <header class="member-header-row">
                <span class="dot {statusDot(member.status)}"></span>
                <span class="member-name">
                  {member.name}
                  {#if member.is_self}<em>(you)</em>{/if}
                </span>
                {#if pref && !member.is_self}
                  <span
                    class="pref-badge"
                    title={pref.reason ?? "no reason set"}
                  >
                    serving at {Math.round(pref.multiplier * 100)}%
                  </span>
                {/if}
              </header>

              <dl class="contribution-blocks">
                <div class="contribution-block">
                  <dt>Inference</dt>
                  <dd>
                    {#if c && (c.inference_served_requests + c.inference_consumed_requests) > 0}
                      <span class="metric">
                        <strong>{c.inference_served_requests.toLocaleString()}</strong>
                        served
                      </span>
                      <span class="metric-sep">·</span>
                      <span class="metric">
                        <strong>{c.inference_consumed_requests.toLocaleString()}</strong>
                        consumed
                      </span>
                      {#if c.inference_served_tokens > 0}
                        <small class="metric-sub">
                          {formatTokens(c.inference_served_tokens)} tokens generated
                        </small>
                      {/if}
                    {:else}
                      <small class="muted">No requests served or consumed yet.</small>
                    {/if}
                  </dd>
                </div>

                <div class="contribution-block">
                  <dt>Knowledge</dt>
                  <dd>
                    {#if c && c.corpora_hosted.length > 0}
                      <ul class="corpus-host-list">
                        {#each c.corpora_hosted as host}
                          <li class="corpus-host">
                            <span class="corpus-host-name">{host.corpus_name}</span>
                            <span class="corpus-host-size">{formatGb(host.size_gb)}</span>
                            {#if host.queries_served > 0}
                              <span class="corpus-host-queries">
                                {host.queries_served.toLocaleString()} queries
                              </span>
                            {/if}
                            {#if host.is_sole_host}
                              <span class="corpus-host-sole" title="Only this peer hosts this corpus right now">
                                sole host
                              </span>
                            {/if}
                          </li>
                        {/each}
                      </ul>
                    {:else}
                      <small class="muted">No hosted corpora.</small>
                    {/if}
                  </dd>
                </div>

                <div class="contribution-block">
                  <dt>Network</dt>
                  <dd>
                    {#if c && (c.bytes_served + c.bytes_received) > 0}
                      <span class="metric">
                        <strong>{formatBytes(c.bytes_served)}</strong> served
                      </span>
                      <span class="metric-sep">·</span>
                      <span class="metric">
                        <strong>{formatBytes(c.bytes_received)}</strong> received
                      </span>
                    {:else}
                      <small class="muted">No bytes transferred yet.</small>
                    {/if}
                  </dd>
                </div>
              </dl>

              {#if !member.is_self}
                <details
                  class="member-preference"
                  ontoggle={(e) =>
                    (e.currentTarget as HTMLDetailsElement).open &&
                    ensurePrefDraft(member.node_id)}
                >
                  <summary>
                    <span class="pref-summary-label">
                      Serve this peer at:
                    </span>
                    <span class="pref-summary-value">
                      {pref ? `${Math.round(pref.multiplier * 100)}%` : "100% (default)"}
                    </span>
                  </summary>
                  {#if prefDraft[member.node_id]}
                    <div class="member-preference-form">
                      <p class="hint">
                        Dial back what this peer can pull from you. 100% is
                        neutral; lower values ration what they see. The number
                        never leaves this machine.
                      </p>
                      <label class="pref-row">
                        <span>Multiplier</span>
                        <input
                          type="range"
                          min="0.05"
                          max="1.0"
                          step="0.05"
                          bind:value={prefDraft[member.node_id].multiplier}
                          aria-label="affinity multiplier"
                        />
                        <span class="pref-value">
                          {Math.round(prefDraft[member.node_id].multiplier * 100)}%
                        </span>
                      </label>
                      <label class="pref-row pref-row-text">
                        <span>Reason (optional, private to you)</span>
                        <input
                          type="text"
                          placeholder="why are you adjusting this peer?"
                          bind:value={prefDraft[member.node_id].reason}
                        />
                      </label>
                      {#if prefError[member.node_id]}
                        <div class="alert error small">
                          {prefError[member.node_id]}
                        </div>
                      {/if}
                      <div class="form-actions pref-actions">
                        <button
                          class="primary"
                          disabled={prefSaving[member.node_id]}
                          onclick={() => savePeerPreference(member.node_id)}
                        >
                          {prefSaving[member.node_id] ? "Saving…" : "Save"}
                        </button>
                        {#if pref}
                          <button
                            class="ghost"
                            disabled={prefSaving[member.node_id]}
                            onclick={() => clearPeerPreference(member.node_id)}
                          >
                            Clear (back to 100%)
                          </button>
                        {/if}
                      </div>
                    </div>
                  {/if}
                </details>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <!-- Switch-mesh paste flow — collapsed by default so it doesn't
         clutter the main view. The MeshJoinDialog auto-leaves the
         current mesh on confirm; the hint here makes that action
         explicit before the user even commits. -->
    <details class="switch-mesh">
      <summary>Join a different mesh</summary>
      <p class="hint">
        Pasting a link will leave
        <strong>"{meshState.status.name}"</strong> first.
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
    </details>

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

  <!-- ─── Rotate confirmation modal ─────────────────────── -->
  {#if showRotateConfirm}
    <div
      class="modal-backdrop"
      onclick={cancelRotate}
      onkeydown={(e) => e.key === "Escape" && cancelRotate()}
      role="presentation"
    >
      <div
        class="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="rotate-title"
        tabindex="-1"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
      >
        <h4 id="rotate-title">Generate a new invite link?</h4>
        <p>
          The current link will stop working. Anyone you've already
          shared it with who hasn't joined yet will need the new
          link. Existing members stay connected.
        </p>
        {#if rotateError}
          <div class="alert error small">{rotateError}</div>
        {/if}
        <div class="form-actions">
          <button class="secondary" onclick={cancelRotate} disabled={rotating}>
            Cancel
          </button>
          <button class="primary" onclick={confirmRotate} disabled={rotating}>
            {rotating ? "Rotating…" : "Rotate"}
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
          placeholder="e.g. mac-peer"
          bind:value={nodeNameInput}
          onkeydown={(e) => e.key === "Enter" && saveNodeName()}
          disabled={nodeNameSaving}
        />
        <button
          class="ghost dice-btn"
          onclick={rollNodeName}
          disabled={nodeNameSaving}
          title="Suggest a memorable name"
          aria-label="Suggest a memorable node name"
        >
          🎲
        </button>
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
    background: color-mix(in srgb, var(--error) 10%, transparent);
    color: var(--error);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
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
  .invite-card {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px 18px;
    background: var(--bg-secondary);
  }

  .invite-card.invite-missing {
    border-style: dashed;
  }

  .form-card h4,
  .share-card h4,
  .status-card h4 {
    font-size: 0.95rem;
    font-weight: 600;
    margin-bottom: 8px;
  }

  .members-card h5,
  .invite-card h5 {
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-muted);
    font-weight: 600;
    margin-bottom: 10px;
  }
  .members-header {
    margin-bottom: 12px;
  }
  .members-header .hint {
    margin-top: 0;
  }

  /* ── Rotate ghost button ─────────────────────────── */
  button.ghost {
    padding: 7px 12px;
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 0.8rem;
    margin-top: 4px;
  }
  button.ghost:hover:not(:disabled) {
    color: var(--text-primary);
    background: var(--bg-input);
  }

  /* ── Relay picker inside the invite card ── */
  .relay-picker {
    margin-top: 10px;
    padding: 10px 12px;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }
  .relay-picker > summary {
    font-size: 0.85rem;
    color: var(--text-secondary);
    cursor: pointer;
    font-weight: 500;
  }
  .relay-active {
    margin-left: 6px;
    font-size: 0.78rem;
    color: var(--success);
    font-weight: 600;
  }
  .relay-picker .hint {
    margin: 8px 0;
  }
  .relay-list {
    list-style: none;
    padding: 0;
    margin: 8px 0 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .relay-list li label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .relay-list .relay-label {
    flex: 1;
  }
  .relay-list .badge {
    margin-left: 6px;
    padding: 1px 6px;
    background: var(--growth-dim);
    color: var(--success);
    border-radius: 8px;
    font-size: 0.7rem;
    font-style: normal;
    font-weight: 600;
    letter-spacing: 0.04em;
  }
  .relay-list .relay-frag {
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: var(--text-muted);
    background: var(--bg-input);
    padding: 1px 6px;
    border-radius: 3px;
  }

  /* ── Dice button alongside the node-name input ── */
  button.dice-btn {
    padding: 0 10px;
    font-size: 1.05rem;
    line-height: 1;
    margin-top: 0;
  }

  /* ── Switch-mesh details on the active-mesh view ── */
  .switch-mesh {
    margin-top: 4px;
    padding: 12px 14px;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }
  .switch-mesh > summary {
    font-size: 0.85rem;
    color: var(--text-secondary);
    cursor: pointer;
    font-weight: 500;
  }
  .switch-mesh > .hint,
  .switch-mesh > .join-row {
    margin-top: 10px;
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
    font-family: var(--font-mono);
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
    background: var(--accent);
  }

  .dot.away {
    background: var(--text-muted);
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
    flex-direction: column;
    gap: 8px;
    padding: 12px 0;
    font-size: 0.85rem;
    border-bottom: 1px solid var(--border);
  }

  .member-row:last-child {
    border-bottom: none;
  }

  .member-header-row {
    display: flex;
    align-items: center;
    gap: 8px;
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

  .pref-badge {
    font-size: 0.7rem;
    font-weight: 500;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--accent-dim);
    color: var(--text-secondary);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, transparent);
    white-space: nowrap;
  }

  /* ── Dimensional contributions per peer ─────────── */
  .contribution-blocks {
    display: grid;
    grid-template-columns: 90px 1fr;
    gap: 4px 12px;
    margin: 0;
    padding: 8px 0 4px 18px;
    font-size: 0.8rem;
  }

  .contribution-block {
    display: contents;
  }

  .contribution-block dt {
    color: var(--text-muted);
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    align-self: start;
    padding-top: 1px;
  }

  .contribution-block dd {
    margin: 0;
    color: var(--text-primary);
    font-variant-numeric: tabular-nums;
  }

  .metric strong {
    font-weight: 600;
  }
  .metric-sep {
    color: var(--text-muted);
    margin: 0 6px;
  }
  .metric-sub {
    display: block;
    color: var(--text-muted);
    font-size: 0.72rem;
    margin-top: 2px;
  }

  .corpus-host-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .corpus-host {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    font-size: 0.8rem;
  }

  .corpus-host-name {
    font-weight: 500;
    color: var(--text-primary);
  }

  .corpus-host-size,
  .corpus-host-queries {
    color: var(--text-muted);
    font-size: 0.74rem;
  }

  .corpus-host-sole {
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 1px 6px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--error) 12%, transparent);
    color: var(--error);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
  }

  /* ── Per-peer affinity multiplier control ───────── */
  .member-preference {
    margin: 4px 0 0 18px;
    border-top: 1px dashed var(--border);
    padding-top: 8px;
  }
  .member-preference > summary {
    font-size: 0.78rem;
    color: var(--text-secondary);
    cursor: pointer;
    list-style: none;
  }
  .member-preference > summary::-webkit-details-marker {
    display: none;
  }
  .pref-summary-label {
    color: var(--text-muted);
  }
  .pref-summary-value {
    color: var(--text-primary);
    font-weight: 500;
    margin-left: 6px;
  }
  .member-preference[open] > summary {
    margin-bottom: 8px;
  }
  .member-preference-form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px 10px;
    background: var(--bg-input);
    border-radius: var(--radius);
  }
  .pref-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.8rem;
  }
  .pref-row > span:first-child {
    flex: 0 0 auto;
    color: var(--text-muted);
  }
  .pref-row input[type="range"] {
    flex: 1;
    accent-color: var(--accent);
  }
  .pref-value {
    flex: 0 0 auto;
    font-variant-numeric: tabular-nums;
    min-width: 3em;
    text-align: right;
    color: var(--text-primary);
  }
  .pref-row-text {
    flex-direction: column;
    align-items: stretch;
    gap: 4px;
  }
  .pref-row-text input[type="text"] {
    padding: 6px 8px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 0.8rem;
    outline: none;
  }
  .pref-row-text input[type="text"]:focus {
    border-color: var(--accent);
  }
  .pref-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
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
