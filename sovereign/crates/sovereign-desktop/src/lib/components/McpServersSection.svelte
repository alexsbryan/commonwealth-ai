<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Settings → MCP Servers. Add / remove external HTTP MCP servers (the
  // canonical `[[mcp_servers]]` list in ~/.sovereign/config.toml), test
  // reachability before saving, and see the live connection status from the
  // last backend start. Mirrors `sovereign mcp add/remove/list` — same file
  // underneath, so a server added here also works in `sovereign chat`.
  import { onMount } from "svelte";
  import {
    listMcpServers,
    addMcpServer,
    removeMcpServer,
    testMcpConnection,
    type McpServerView,
  } from "../api";

  let servers = $state<McpServerView[]>([]);
  let error = $state("");

  // Add-form state.
  let name = $state("");
  let url = $state("");
  let description = $state("");
  let bearer = $state(false);
  let adding = $state(false);
  let testing = $state(false);
  let testResult = $state("");

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
      const n = await testMcpConnection(name.trim() || "test", url.trim(), bearer);
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
      name = "";
      url = "";
      description = "";
      bearer = false;
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
            {#if s.bearer && s.token_env}
              <span class="server-auth">token env: <code>{s.token_env}</code></span>
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
      <span>Bearer auth — token read from <code>SOVEREIGN_MCP_TOKEN_&lt;NAME&gt;</code></span>
    </label>
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
    run <code>sovereign mcp demo-server</code> in a terminal, add
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
  .add-form input[type="text"] {
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
