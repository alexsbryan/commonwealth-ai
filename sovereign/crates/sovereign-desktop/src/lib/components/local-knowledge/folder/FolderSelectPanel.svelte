<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { lcValidatePath } from "../../../api";

  interface Props {
    initialPath?: string | null;
    onSelected: (path: string) => void;
    onCancel: () => void;
  }

  let { initialPath = null, onSelected, onCancel }: Props = $props();

  let path: string = $state(initialPath ?? "");
  let status: string = $state("");
  let busy = $state(false);

  async function browse() {
    busy = true;
    status = "";
    try {
      const picked = await open({
        multiple: false,
        directory: true,
      });
      if (typeof picked === "string") {
        path = picked;
        await validate();
      }
    } catch (e) {
      status = `Could not open picker: ${e}`;
    }
    busy = false;
  }

  async function validate() {
    if (!path) {
      status = "";
      return;
    }
    try {
      const v = await lcValidatePath(path);
      if (!v.exists) {
        status = "That path doesn't exist.";
      } else if (!v.is_dir) {
        status = "Please pick a folder, not a file.";
      } else if (!v.readable) {
        status = "Sovereign can't read that folder (permissions?).";
      } else {
        status = "";
      }
    } catch (e) {
      status = `Validation failed: ${e}`;
    }
  }

  function confirm() {
    if (path && !status) onSelected(path);
  }
</script>

<div class="folder-select">
  <p class="heading">Choose a folder of documents</p>
  <p class="hint">
    Sovereign will read PDFs and text files from this folder. The files
    stay on your computer — nothing is uploaded.
  </p>

  <div class="path-row">
    <input
      type="text"
      bind:value={path}
      onblur={validate}
      placeholder="/path/to/documents"
      aria-label="Folder path"
    />
    <button class="btn-secondary" onclick={browse} disabled={busy}>
      Browse…
    </button>
  </div>

  {#if status}
    <p class="status">{status}</p>
  {/if}

  <div class="actions">
    <button class="btn-secondary" onclick={onCancel}>Cancel</button>
    <button
      class="btn-primary"
      onclick={confirm}
      disabled={!path || !!status}
    >
      Continue
    </button>
  </div>

  <p class="drag-note">Tip: you can also drag a folder directly into Sovereign.</p>
</div>

<style>
  .folder-select {
    padding: 16px 0;
    max-width: 540px;
  }
  .heading {
    font-size: 16px;
    font-weight: 500;
    margin: 0 0 8px;
  }
  .hint {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin: 0 0 16px;
  }
  .path-row {
    display: flex;
    gap: 8px;
    margin-bottom: 8px;
  }
  input[type="text"] {
    flex: 1;
    padding: 8px 10px;
    border: 1px solid var(--color-border, #d4d4d4);
    border-radius: 6px;
    font-size: 13px;
    font-family: var(--font-mono, ui-monospace, monospace);
    background: var(--color-surface, #fff);
    color: var(--color-text, #1a1a1a);
  }
  .status {
    color: var(--color-error, #c92a2a);
    font-size: 13px;
    margin: 0 0 12px;
  }
  .actions {
    display: flex;
    gap: 12px;
    margin-top: 20px;
  }
  .drag-note {
    margin-top: 16px;
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    font-style: italic;
  }
  .btn-primary,
  .btn-secondary {
    padding: 8px 16px;
    border-radius: 6px;
    font-size: 14px;
    cursor: pointer;
    border: none;
  }
  .btn-primary {
    background: var(--color-accent, #3a5fc9);
    color: #fff;
  }
  .btn-primary:disabled {
    background: var(--color-surface-subtle, #ccc);
    cursor: not-allowed;
  }
  .btn-primary:hover:not(:disabled) {
    background: var(--color-accent-hover, #2f4fb3);
  }
  .btn-secondary {
    background: transparent;
    color: var(--color-text, #1a1a1a);
    border: 1px solid var(--color-border, #d4d4d4);
  }
  .btn-secondary:hover {
    background: var(--color-surface-subtle, #f4f4f4);
  }
</style>
