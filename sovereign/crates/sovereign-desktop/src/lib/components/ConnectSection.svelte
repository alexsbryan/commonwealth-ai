<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { getConfig, listDaemonModels } from "../api";

  // W5: surface the daemon's OpenAI-compatible endpoint so power
  // users can point Codex / Claude Code / etc. at it without
  // hunting through docs. Three copy-buttons (base URL, API key,
  // codex one-liner) + live /v1/models list refreshed every 10s.

  let clientPort = $state<number>(9741);
  let models = $state<string[]>([]);
  let loadingModels = $state(false);
  let modelsError: string | null = $state(null);
  let pollHandle: ReturnType<typeof setInterval> | null = null;
  let copiedKey: string | null = $state(null);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;

  let baseUrl = $derived(`http://localhost:${clientPort}/v1`);
  // Any non-empty string works as the API key today — the daemon's
  // /v1 surface doesn't require auth on the loopback boundary. The
  // string is purely for clients that demand the env var be set.
  const apiKey = "sovereign-local";

  let codexCommand = $derived(
    `OPENAI_BASE_URL=${baseUrl} OPENAI_API_KEY=${apiKey} codex`,
  );

  async function loadModels() {
    loadingModels = true;
    modelsError = null;
    try {
      // Goes through a Tauri command on the Rust side rather than
      // raw `fetch` — the renderer can't reach localhost across
      // Tauri's sandbox (Safari surfaces this as "Load failed").
      // Coerce a missing/empty result to `[]` so a command that
      // resolves nothing can't make `models.length` throw and blank
      // the whole Connect panel (degrade visibly, not via a crash).
      models = (await listDaemonModels()) ?? [];
    } catch (e) {
      modelsError = e instanceof Error ? e.message : String(e);
    } finally {
      loadingModels = false;
    }
  }

  async function copy(key: string, value: string) {
    try {
      await navigator.clipboard.writeText(value);
      copiedKey = key;
      if (copiedTimer !== null) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => {
        copiedKey = null;
      }, 1400);
    } catch {
      // Clipboard API may be unavailable in some embedded contexts.
      // Silent failure — the value is still selectable.
    }
  }

  onMount(async () => {
    try {
      const cfg = await getConfig();
      // DesktopConfig doesn't carry the daemon's client port today
      // (it's in SetupConfig on the daemon side). The fallback to
      // 9741 matches the default the daemon binds to in the absence
      // of SetupConfig overrides — same value the bootstrap probe
      // uses. If a SetupConfig override is in play, the user can
      // adjust the displayed port via Settings → Paths in a future
      // pass.
      void cfg;
    } catch {
      // Config unreadable — keep the 9741 default.
    }
    await loadModels();
    pollHandle = setInterval(loadModels, 10_000);
  });

  onDestroy(() => {
    if (pollHandle !== null) clearInterval(pollHandle);
    if (copiedTimer !== null) clearTimeout(copiedTimer);
  });
</script>

<section class="connect">
  <h3 class="h3">External tools</h3>
  <p class="hint">
    Codex, Claude Code, and any OpenAI-compatible client can talk to the
    local daemon. Point them here:
  </p>

  <div class="env">
    <div class="env-row">
      <span class="env-key">OPENAI_BASE_URL</span>
      <code class="env-value">{baseUrl}</code>
      <button
        class="copy"
        onclick={() => copy("base", baseUrl)}
        aria-label="Copy base URL"
      >
        {copiedKey === "base" ? "Copied" : "Copy"}
      </button>
    </div>
    <div class="env-row">
      <span class="env-key">OPENAI_API_KEY</span>
      <code class="env-value">{apiKey}</code>
      <button
        class="copy"
        onclick={() => copy("key", apiKey)}
        aria-label="Copy API key"
      >
        {copiedKey === "key" ? "Copied" : "Copy"}
      </button>
    </div>
  </div>

  <p class="hint">
    Any non-empty string works as the API key — the daemon trusts every
    connection coming from this machine.
  </p>

  <h3 class="h3">Codex, one line</h3>
  <div class="cmd">
    <code class="cmd-text">{codexCommand}</code>
    <button
      class="copy"
      onclick={() => copy("codex", codexCommand)}
      aria-label="Copy Codex command"
    >
      {copiedKey === "codex" ? "Copied" : "Copy"}
    </button>
  </div>

  <h3 class="h3">Available models</h3>
  {#if loadingModels && models.length === 0}
    <p class="hint">Loading model list…</p>
  {:else if modelsError}
    <p class="error">Couldn't reach /v1/models: {modelsError}</p>
  {:else if models.length === 0}
    <p class="hint">No models registered yet.</p>
  {:else}
    <ul class="models">
      {#each models as id (id)}
        <li class="model"><code>{id}</code></li>
      {/each}
    </ul>
  {/if}
  <p class="meta">Refreshes every ten seconds.</p>
</section>

<style>
  /* Lavender Court substrate — matches the rest of Settings. The
     previous palette inverted: light-card sections + a dark code
     block. With the section ported to dark, the code block now sits
     a step DEEPER (--bg-input) rather than inverting the substrate,
     and the env rows use --bg-secondary so they read as inset
     panels against the page. */
  .connect {
    font-family: var(--font-sans);
    color: var(--text-secondary);
    -webkit-font-smoothing: antialiased;
  }

  .h3 {
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--text-primary);
    margin: 28px 0 8px;
    letter-spacing: -0.005em;
  }

  .h3:first-child {
    margin-top: 0;
  }

  .hint {
    font-size: 0.88rem;
    color: var(--text-muted);
    margin: 0 0 14px;
    line-height: 1.5;
    max-width: 540px;
  }

  .env {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
  }

  .env-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .env-key {
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    min-width: 140px;
  }

  .env-value {
    flex: 1 1 auto;
    font-family: var(--font-mono);
    font-size: 0.82rem;
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Recessed code block — the deepest background in the palette so
     the one-liner reads as a terminal-ish snippet without leaving
     the page's dark register. */
  .cmd {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    margin-bottom: 14px;
  }

  .cmd-text {
    flex: 1 1 auto;
    font-family: var(--font-mono);
    font-size: 0.82rem;
    color: var(--accent-light);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .copy {
    font-family: var(--font-sans);
    font-size: 0.78rem;
    font-weight: 500;
    letter-spacing: 0.04em;
    background: none;
    border: 1px solid var(--border-mid);
    color: var(--text-secondary);
    padding: 4px 12px;
    border-radius: var(--radius);
    cursor: pointer;
    transition: background 140ms ease, border-color 140ms ease, color 140ms ease;
    flex-shrink: 0;
  }

  .copy:hover {
    border-color: var(--accent);
    color: var(--accent-light);
    background: var(--bg-surface);
  }

  /* Inside the recessed code block, the copy button picks up the
     accent tint so it reads as part of the snippet's affordance
     rather than a generic page button. */
  .cmd .copy {
    color: var(--accent);
    border-color: rgba(201, 168, 76, 0.32);
  }

  .cmd .copy:hover {
    background: var(--accent-dim);
    border-color: var(--accent);
  }

  .models {
    list-style: none;
    padding: 0;
    margin: 0 0 10px;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .model {
    display: inline-block;
  }

  .model code {
    font-family: var(--font-mono);
    font-size: 0.78rem;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    color: var(--text-primary);
    padding: 3px 8px;
    border-radius: var(--radius);
  }

  .error {
    color: var(--error);
    font-size: 0.88rem;
    margin: 0 0 10px;
  }

  .meta {
    font-size: 0.78rem;
    color: var(--text-muted);
    margin: 0;
  }
</style>
