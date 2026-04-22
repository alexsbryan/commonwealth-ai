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
        status = "Sovereign can't read that folder.";
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

<section class="select">
  <header class="head">
    <h2 class="title">Pick a folder</h2>
    <p class="lede">
      PDFs and text files. Indexed locally. Nothing is uploaded.
    </p>
  </header>

  <div class="field">
    <label class="lk-label field-label" for="path-input">Path</label>
    <div class="row">
      <input
        id="path-input"
        type="text"
        bind:value={path}
        onblur={validate}
        placeholder="/path/to/documents"
        aria-label="Folder path"
      />
      <button class="lk-btn lk-btn--quiet" onclick={browse} disabled={busy}>
        Browse…
      </button>
    </div>
    {#if status}
      <p class="status">{status}</p>
    {/if}
  </div>

  <div class="actions">
    <button class="lk-btn lk-btn--quiet" onclick={onCancel}>Cancel</button>
    <button
      class="lk-btn lk-btn--mark"
      onclick={confirm}
      disabled={!path || !!status}
    >
      Continue
    </button>
  </div>

  <p class="aside">Or drag a folder into this window.</p>
</section>

<style>
  .select {
    padding: 4px 0;
    max-width: 640px;
    animation: lk-fade-in 260ms ease-out both;
  }
  .head { margin-bottom: 18px; }
  .title {
    margin: 0 0 4px;
    font-size: var(--lk-size-hero);
    font-weight: 600;
    line-height: 1.1;
    letter-spacing: -0.02em;
    color: var(--lk-ink);
  }
  .lede {
    margin: 0;
    font-size: var(--lk-size-body);
    color: var(--lk-ink-soft);
    max-width: 58ch;
    line-height: 1.5;
  }

  .field { margin-bottom: 20px; }
  .field-label {
    display: block;
    margin-bottom: 8px;
  }
  .row {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }
  input[type="text"] {
    flex: 1;
    padding: 8px 10px;
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    background: var(--lk-paper);
    color: var(--lk-ink);
    font-family: var(--lk-font-mono);
    font-size: 13px;
    transition: border-color 140ms ease;
  }
  input[type="text"]:focus {
    outline: 0;
    border-color: var(--lk-crown);
  }
  .status {
    margin: 8px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-err);
  }

  .actions {
    display: flex;
    gap: 10px;
    justify-content: flex-end;
    margin-bottom: 14px;
  }
  .aside {
    margin: 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }
</style>
