<script lang="ts">
  interface Props {
    onConfigure: (provider: string, apiKey: string | null) => void;
    onSkip: () => void;
  }

  let { onConfigure, onSkip }: Props = $props();

  let provider: string = $state("duckduckgo");
  let apiKey: string = $state("");

  const providers = [
    {
      id: "duckduckgo",
      name: "DuckDuckGo",
      desc: "Free, no API key required. Good for general queries.",
      needsKey: false,
    },
    {
      id: "brave",
      name: "Brave Search",
      desc: "Higher quality results. Requires a free API key.",
      needsKey: true,
    },
    {
      id: "tavily",
      name: "Tavily",
      desc: "AI-optimized search. Free tier with 1,000 monthly queries.",
      needsKey: true,
    },
  ];

  function handleContinue() {
    const key = providers.find((p) => p.id === provider)?.needsKey
      ? apiKey || null
      : null;
    onConfigure(provider, key);
  }
</script>

<div class="ws-setup">
  <h1>Web Search</h1>
  <p class="subtitle">
    Your local knowledge bases handle most queries. Web search supplements them
    for current events and niche topics.
  </p>

  <div class="provider-cards">
    {#each providers as p}
      <button
        class="provider-card"
        class:selected={provider === p.id}
        onclick={() => (provider = p.id)}
      >
        <h2>{p.name}</h2>
        <p>{p.desc}</p>
      </button>
    {/each}
  </div>

  {#if providers.find((p) => p.id === provider)?.needsKey}
    <div class="api-key-section">
      <label>
        <span>API Key</span>
        <input
          type="password"
          bind:value={apiKey}
          placeholder="Enter your API key"
        />
      </label>
    </div>
  {/if}

  <div class="actions">
    <button class="continue-btn" onclick={handleContinue}>Continue</button>
    <button class="skip-link" onclick={onSkip}>
      Offline knowledge is enough for now
    </button>
  </div>
</div>

<style>
  .ws-setup {
    text-align: center;
    max-width: 600px;
    margin: 0 auto;
  }
  h1 {
    font-size: 1.8rem;
    font-weight: 300;
    margin-bottom: 0.5rem;
  }
  .subtitle {
    color: var(--text-secondary);
    margin-bottom: 2rem;
    font-size: 1rem;
    line-height: 1.5;
  }
  .provider-cards {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-bottom: 1.5rem;
  }
  .provider-card {
    padding: 16px 20px;
    background: var(--bg-secondary);
    border: 2px solid var(--border);
    border-radius: var(--radius-lg);
    text-align: left;
    transition: border-color 0.2s;
  }
  .provider-card:hover {
    border-color: var(--accent);
  }
  .provider-card.selected {
    border-color: var(--accent);
    background: var(--bg-surface);
  }
  .provider-card h2 {
    font-size: 1rem;
    font-weight: 600;
    margin-bottom: 4px;
  }
  .provider-card p {
    font-size: 0.85rem;
    color: var(--text-secondary);
    margin: 0;
  }
  .api-key-section {
    margin-bottom: 1.5rem;
    text-align: left;
  }
  .api-key-section label {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .api-key-section span {
    font-size: 0.85rem;
    color: var(--text-secondary);
  }
  .api-key-section input {
    padding: 8px 12px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
  }
  .api-key-section input:focus {
    border-color: var(--accent);
  }
  .actions {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
  }
  .continue-btn {
    padding: 12px 40px;
    background: var(--accent);
    color: white;
    border-radius: var(--radius);
    font-weight: 500;
    font-size: 1rem;
  }
  .continue-btn:hover {
    background: var(--accent-hover);
  }
  .skip-link {
    font-size: 0.85rem;
    color: var(--text-muted);
    background: none;
    border: none;
    text-decoration: underline;
    cursor: pointer;
  }
  .skip-link:hover {
    color: var(--text-secondary);
  }
</style>
