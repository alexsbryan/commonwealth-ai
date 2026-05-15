<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { getConfig } from "../api";

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
      const resp = await fetch(`${baseUrl}/models`);
      if (!resp.ok) {
        modelsError = `daemon /v1/models returned ${resp.status}`;
        return;
      }
      const json: { data?: Array<{ id?: string }> } = await resp.json();
      models = (json.data ?? [])
        .map((m) => (typeof m.id === "string" ? m.id : null))
        .filter((s): s is string => s !== null)
        .sort();
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
    Codex, Claude Code, and any OpenAI-compatible client can talk to your
    local daemon. Point them at this endpoint:
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
    The API key can be any non-empty string — the daemon trusts every
    connection on the loopback interface.
  </p>

  <h3 class="h3">Example: Codex one-liner</h3>
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
  <p class="meta">Refreshes every 10 seconds.</p>
</section>

<style>
  .connect {
    font-family: "Outfit", system-ui, -apple-system, "Segoe UI", sans-serif;
    color: oklch(28% 0.015 250);
    -webkit-font-smoothing: antialiased;
  }

  .h3 {
    font-size: 0.95rem;
    font-weight: 600;
    color: oklch(22% 0.015 250);
    margin: 28px 0 8px;
    letter-spacing: -0.005em;
  }

  .h3:first-child {
    margin-top: 0;
  }

  .hint {
    font-size: 0.88rem;
    color: oklch(50% 0.012 250);
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
    background: oklch(97% 0.005 250);
    border: 1px solid oklch(90% 0.008 250);
    border-radius: 5px;
  }

  .env-key {
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    color: oklch(40% 0.012 250);
    min-width: 140px;
  }

  .env-value {
    flex: 1 1 auto;
    font-family: "JetBrains Mono", "SF Mono", Menlo, monospace;
    font-size: 0.82rem;
    color: oklch(22% 0.015 250);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .cmd {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    background: oklch(20% 0.012 250);
    border-radius: 5px;
    margin-bottom: 14px;
  }

  .cmd-text {
    flex: 1 1 auto;
    font-family: "JetBrains Mono", "SF Mono", Menlo, monospace;
    font-size: 0.82rem;
    color: oklch(94% 0.006 80);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .copy {
    font-family: "Outfit", system-ui, sans-serif;
    font-size: 0.78rem;
    font-weight: 500;
    letter-spacing: 0.04em;
    background: none;
    border: 1px solid oklch(70% 0.010 250 / 0.6);
    color: oklch(35% 0.012 250);
    padding: 4px 12px;
    border-radius: 4px;
    cursor: pointer;
    transition: background 140ms ease, border-color 140ms ease;
    flex-shrink: 0;
  }

  .copy:hover {
    border-color: oklch(45% 0.012 250);
    background: oklch(96% 0.005 250);
  }

  .cmd .copy {
    color: oklch(90% 0.006 80);
    border-color: oklch(60% 0.020 80 / 0.5);
  }

  .cmd .copy:hover {
    background: oklch(30% 0.012 250);
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
    font-family: "JetBrains Mono", "SF Mono", Menlo, monospace;
    font-size: 0.78rem;
    background: oklch(96% 0.008 250);
    border: 1px solid oklch(90% 0.008 250);
    color: oklch(30% 0.012 250);
    padding: 3px 8px;
    border-radius: 4px;
  }

  .error {
    color: oklch(45% 0.15 25);
    font-size: 0.88rem;
    margin: 0 0 10px;
  }

  .meta {
    font-size: 0.78rem;
    color: oklch(58% 0.012 250);
    margin: 0;
  }
</style>
