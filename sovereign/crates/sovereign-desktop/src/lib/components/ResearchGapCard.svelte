<script lang="ts">
  interface Props {
    text: string;
    gapQuery?: string;
    onsubmit?: (query: string) => void;
  }

  let { text, gapQuery, onsubmit }: Props = $props();
  let inputValue = $state("");

  function handleSubmit() {
    if (inputValue.trim() && onsubmit) {
      onsubmit(inputValue.trim());
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSubmit();
    }
  }
</script>

<div class="gap-card">
  <span class="gap-label">RESEARCH GAP</span>
  <p class="gap-text">{text}</p>
  {#if gapQuery}
    <p class="gap-hint">Suggested: <em>{gapQuery}</em></p>
  {/if}
  <div class="gap-input-row">
    <input
      type="text"
      class="gap-input"
      placeholder="Ask about this gap..."
      bind:value={inputValue}
      onkeydown={handleKeydown}
    />
    <button
      class="gap-skip"
      onclick={() => onsubmit?.("")}
    >Skip</button>
  </div>
</div>

<style>
  .gap-card {
    border-left: 2px solid var(--amber);
    background: var(--bg-surface);
    border-radius: var(--radius);
    padding: 12px 14px;
    margin: 12px 0;
  }

  .gap-label {
    font-family: var(--font-sans);
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--amber);
    display: block;
    margin-bottom: 6px;
  }

  .gap-text {
    font-family: var(--font-serif);
    font-size: 14px;
    line-height: 1.7;
    color: var(--text-primary);
    margin: 0 0 8px;
  }

  .gap-hint {
    font-size: 12px;
    color: var(--text-muted);
    margin: 0 0 8px;
  }

  .gap-input-row {
    display: flex;
    gap: 6px;
  }

  .gap-input {
    flex: 1;
    padding: 6px 10px;
    background: var(--bg-input);
    border: 0.5px solid var(--border-mid);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 13px;
    font-family: var(--font-sans);
    outline: none;
  }

  .gap-input:focus {
    border-color: var(--amber);
  }

  .gap-skip {
    padding: 6px 12px;
    background: none;
    border: 0.5px solid var(--border-mid);
    border-radius: var(--radius);
    color: var(--text-muted);
    font-size: 12px;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: color 0.15s;
  }

  .gap-skip:hover {
    color: var(--text-secondary);
  }
</style>
