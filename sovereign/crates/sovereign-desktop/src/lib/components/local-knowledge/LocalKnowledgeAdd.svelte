<script lang="ts">
  interface Props {
    onPickFolder: () => void;
    onPickObsidian: () => void;
  }

  let { onPickFolder, onPickObsidian }: Props = $props();
</script>

<div class="add-source">
  <h3>Add local documents</h3>

  <div class="source-options">
    <button class="source-option" onclick={onPickFolder}>
      <span class="option-icon" aria-hidden="true">📁</span>
      <span class="option-label">Drop or browse a folder</span>
      <span class="option-sub">PDFs and text files</span>
    </button>

    <button class="source-option" onclick={onPickObsidian}>
      <span class="option-icon" aria-hidden="true">◈</span>
      <span class="option-label">Connect Obsidian vault</span>
      <span class="option-sub">Markdown notes</span>
    </button>
  </div>

  <p class="add-note">
    You can also drag any folder directly onto Sovereign.
  </p>
</div>

<style>
  .add-source {
    padding: 12px 0 20px;
  }
  h3 {
    font-size: 14px;
    font-weight: 500;
    margin: 0 0 12px;
    color: var(--color-text, #1a1a1a);
  }
  .source-options {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
    max-width: 600px;
  }
  .source-option {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 16px;
    border: 1px solid var(--color-border, #d4d4d4);
    border-radius: 8px;
    background: var(--color-surface, #fff);
    cursor: pointer;
    text-align: left;
    color: var(--color-text, #1a1a1a);
    transition: background 120ms, border-color 120ms;
  }
  .source-option:hover:not(:disabled) {
    background: var(--color-surface-subtle, #f4f4f4);
    border-color: var(--color-accent, #3a5fc9);
  }
  .source-option:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .option-icon {
    font-size: 22px;
  }
  .option-label {
    font-size: 14px;
    font-weight: 500;
  }
  .option-sub {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
  }
  .add-note {
    margin-top: 14px;
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    font-style: italic;
  }
</style>
