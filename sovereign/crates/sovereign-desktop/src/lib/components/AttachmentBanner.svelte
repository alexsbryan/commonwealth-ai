<script lang="ts">
  interface Props {
    filename: string;
    chunksCreated: number;
    onremove: () => void;
  }

  let { filename, chunksCreated, onremove }: Props = $props();
</script>

<div class="attachment-banner">
  <div class="attachment-info">
    <span class="attachment-icon">
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none">
        <path d="M14 4.5V14a1 1 0 01-1 1H3a1 1 0 01-1-1V2a1 1 0 011-1h6.5L14 4.5z" stroke="currentColor" stroke-width="1.2"/>
        <path d="M9.5 1v4H14" stroke="currentColor" stroke-width="1.2"/>
      </svg>
    </span>
    <span class="attachment-name">{filename}</span>
    <span class="attachment-chunks">{chunksCreated} chunks</span>
  </div>
  <button class="attachment-remove" onclick={onremove} title="Remove attachment">
    &times;
  </button>
</div>

<style>
  .attachment-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    margin: 0 0 4px 0;
    background: var(--bg-surface);
    border: 0.5px solid var(--border-mid);
    border-radius: var(--radius);
    font-size: 0.75rem;
    color: var(--text-secondary);
    font-family: var(--font-sans);
  }

  .attachment-info {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .attachment-icon {
    color: var(--accent);
    display: flex;
    align-items: center;
  }

  .attachment-name {
    font-weight: 600;
    color: var(--text-primary);
  }

  .attachment-chunks {
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.65rem;
  }

  .attachment-remove {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 1rem;
    cursor: pointer;
    padding: 0 2px;
    line-height: 1;
  }
  .attachment-remove:hover {
    color: var(--error);
  }
</style>
