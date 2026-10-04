<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Settings → MCP Servers. Add / remove external HTTP MCP servers (the
  // canonical `[[mcp_servers]]` list in ~/.svrnmesh/config.toml), test
  // reachability before saving, and see the live connection status from the
  // last backend start. Mirrors `svrn mcp add/remove/list` — same file
  // underneath, so a server added here also works in `sovereign chat`.
  import { onMount } from "svelte";
  import {
    listMcpServers,
    addMcpServer,
    removeMcpServer,
    testMcpConnection,
    setMcpToken,
    clearMcpToken,
    type McpServerView,
  } from "../api";

  let servers = $state<McpServerView[]>([]);
  let error = $state("");

  // Add-form state.
  let name = $state("");
  let url = $state("");
  let description = $state("");
  let bearer = $state(false);
  let token = $state("");
  let adding = $state(false);
  let testing = $state(false);
  let testResult = $state("");

  // Per-row token editing (set / update / clear an existing server's token).
  let editingToken = $state<string | null>(null);
  let editToken = $state("");

  // The env var a bearer token can ALSO be read from — the headless / CI
  // override — derived from the server name with the same fold the daemon
  // uses (`secret_env_var`): non-alphanumeric → `_`, uppercased. The primary
  // path is the in-app token field below (stored under ~/.svrnmesh/secrets);
  // this is surfaced as the alternative for nodes with no GUI.
  const tokenEnvVar = $derived(
    name.trim()
      ? `SOVEREIGN_MCP_TOKEN_${name.trim().replace(/[^a-zA-Z0-9]/g, "_").toUpperCase()}`
      : null,
  );

  async function refresh() {
    try {
      servers = (await listMcpServers()) ?? [];
    } catch (e) {
      error = String(e);
    }
  }

  onMount(refresh);

  function statusLabel(s: McpServerView): { text: string; cls: string } {
    if (!s.enabled) return { text: "disabled", cls: "muted" };
    if (s.connected === true)
      return { text: `connected · ${s.tool_count ?? 0} tools`, cls: "ok" };
    if (s.connected === false)
      return { text: s.error ? `error · ${s.error}` : "unavailable", cls: "err" };
    return { text: "not loaded — restart to connect", cls: "muted" };
  }

  async function test() {
    if (!url.trim()) {
      testResult = "Enter a URL first.";
      return;
    }
    testing = true;
    testResult = "";
    try {
      const n = await testMcpConnection(
        name.trim() || "test",
        url.trim(),
        bearer,
        token.trim() || null,
      );
      testResult = `Connected — ${n} tools.`;
    } catch (e) {
      testResult = `Failed: ${e}`;
    } finally {
      testing = false;
    }
  }

  async function add() {
    error = "";
    if (!name.trim()) {
      error = "Give the server a name.";
      return;
    }
    if (!url.trim()) {
      error = "Enter the server URL.";
      return;
    }
    adding = true;
    try {
      await addMcpServer(name.trim(), url.trim(), description.trim() || null, bearer);
      if (bearer && token.trim()) {
        await setMcpToken(name.trim(), token.trim());
      }
      name = "";
      url = "";
      description = "";
      bearer = false;
      token = "";
      testResult = "";
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      adding = false;
    }
  }

  async function remove(serverName: string) {
    error = "";
    try {
      await removeMcpServer(serverName);
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  function startEditToken(serverName: string) {
    editingToken = serverName;
    editToken = "";
  }
  async function saveToken(serverName: string) {
    error = "";
    try {
      await setMcpToken(serverName, editToken);
      editingToken = null;
      editToken = "";
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }
  async function doClearToken(serverName: string) {
    error = "";
    try {
      await clearMcpToken(serverName);
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="mcp">
  {#if servers.length > 0}
    <ul class="server-list">
      {#each servers as s (s.name)}
        {@const st = statusLabel(s)}
        <li class="server">
          <div class="server-main">
            <span class="server-name">{s.name}</span>
            <span class="server-url">{s.url}</span>
            {#if s.description}<span class="server-desc">{s.description}</span>{/if}
            {#if s.bearer}
              <span class="server-auth">
                {#if s.has_token}token stored{:else}no token yet{/if}
                <button
                  class="link"
                  onclick={() => startEditToken(s.name)}
                  data-testid="mcp-row-set-token"
                >
                  {s.has_token ? "Update" : "Set token"}
                </button>
                {#if s.has_token}
                  <button class="link-danger" onclick={() => doClearToken(s.name)}>Clear</button>
                {/if}
              </span>
              {#if editingToken === s.name}
                <div class="token-edit">
                  <input
                    type="password"
                    bind:value={editToken}
                    placeholder="paste token"
                    autocomplete="off"
                    data-testid="mcp-row-token-input"
                  />
                  <button class="link" onclick={() => saveToken(s.name)} data-testid="mcp-row-token-save">
                    Save
                  </button>
                  <button class="link" onclick={() => (editingToken = null)}>Cancel</button>
                </div>
              {/if}
            {/if}
          </div>
          <span class="badge {st.cls}">{st.text}</span>
          <button class="link-danger" onclick={() => remove(s.name)}>Remove</button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="empty">
      No MCP servers yet. Add one below to extend the assistant with external tools.
    </p>
  {/if}

  <h3 class="doc-h3">Add a server</h3>
  <div class="add-form">
    <label>
      <span>Name</span>
      <input type="text" bind:value={name} placeholder="vision" autocomplete="off" />
    </label>
    <label>
      <span>URL</span>
      <input type="text" bind:value={url} placeholder="https://host/mcp" autocomplete="off" />
    </label>
    <label>
      <span>Description <em>(optional)</em></span>
      <input
        type="text"
        bind:value={description}
        placeholder="Describe images"
        autocomplete="off"
      />
    </label>
    <label class="checkbox">
      <input type="checkbox" bind:checked={bearer} />
      <span>Bearer auth</span>
    </label>
    {#if bearer}
      <label>
        <span>Token</span>
        <input
          type="password"
          bind:value={token}
          placeholder="paste the bearer token"
          autocomplete="off"
          data-testid="mcp-token-input"
        />
      </label>
      <p class="bearer-note">
        Stored under <code>~/.svrnmesh/secrets/</code> (owner-only) — never in
        your config, a backup, or gossiped to a peer.{#if tokenEnvVar} On a
        headless node or in CI, set <code>{tokenEnvVar}</code> instead.{/if}
      </p>
    {/if}
    <div class="actions">
      <button class="btn-secondary" onclick={test} disabled={testing}>
        {testing ? "Testing…" : "Test connection"}
      </button>
      <button class="btn-primary" onclick={add} disabled={adding}>
        {adding ? "Adding…" : "Add server"}
      </button>
    </div>
    {#if testResult}<p class="test-result">{testResult}</p>{/if}
  </div>

  {#if error}<p class="error">{error}</p>{/if}

  <p class="hint">
    New servers connect on the next app start. To try it with no external service:
    run <code>svrn mcp demo-server</code> in a terminal, add
    <code>http://127.0.0.1:4319/mcp</code>, restart, then ask the assistant for
    Vega's clearance code.
  </p>
</div>

<style>
  .mcp {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .server-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .server {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.5rem 0.6rem;
    border: 1px solid var(--border, #2a2a2a);
    border-radius: 6px;
  }
  .server-main {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    min-width: 0;
    flex: 1;
  }
  .server-name {
    font-weight: 600;
  }
  .server-url {
    font-size: 0.8rem;
    opacity: 0.7;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .server-desc {
    font-size: 0.8rem;
    opacity: 0.8;
  }
  .server-auth {
    font-size: 0.75rem;
    opacity: 0.7;
  }
  .badge {
    font-size: 0.72rem;
    padding: 0.15rem 0.45rem;
    border-radius: 999px;
    white-space: nowrap;
  }
  .badge.ok {
    background: rgba(40, 160, 80, 0.18);
    color: #4ec27a;
  }
  .badge.err {
    background: rgba(200, 60, 60, 0.18);
    color: #e06464;
  }
  .badge.muted {
    background: rgba(140, 140, 140, 0.15);
    color: #999;
  }
  .empty {
    opacity: 0.7;
    font-size: 0.9rem;
  }
  .add-form {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .add-form label {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.85rem;
  }
  .add-form label.checkbox {
    flex-direction: row;
    align-items: center;
    gap: 0.4rem;
  }
  .bearer-note {
    font-size: 0.78rem;
    line-height: 1.45;
    opacity: 0.8;
    margin: -0.1rem 0 0.1rem;
  }
  .token-edit {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-top: 0.3rem;
  }
  .token-edit input {
    flex: 1;
    padding: 0.3rem 0.45rem;
    border-radius: 5px;
    border: 1px solid var(--border, #2a2a2a);
    background: var(--input-bg, #1a1a1a);
    color: inherit;
    font-size: 0.8rem;
  }
  .link {
    background: none;
    border: none;
    color: var(--accent, #6ab0f3);
    cursor: pointer;
    font-size: 0.78rem;
    padding: 0;
  }
  .server-auth .link,
  .server-auth .link-danger {
    margin-left: 0.4rem;
  }
  .add-form input[type="text"],
  .add-form input[type="password"] {
    padding: 0.4rem 0.5rem;
    border-radius: 5px;
    border: 1px solid var(--border, #2a2a2a);
    background: var(--input-bg, #1a1a1a);
    color: inherit;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.2rem;
  }
  .test-result {
    font-size: 0.82rem;
    opacity: 0.85;
    margin: 0.2rem 0 0;
  }
  .error {
    color: #e06464;
    font-size: 0.85rem;
  }
  .link-danger {
    background: none;
    border: none;
    color: #e06464;
    cursor: pointer;
    font-size: 0.8rem;
  }
  .hint {
    font-size: 0.8rem;
    opacity: 0.7;
    border-top: 1px solid var(--border, #2a2a2a);
    padding-top: 0.6rem;
  }
  code {
    font-size: 0.85em;
    background: rgba(140, 140, 140, 0.15);
    padding: 0.05rem 0.25rem;
    border-radius: 3px;
  }
</style>
