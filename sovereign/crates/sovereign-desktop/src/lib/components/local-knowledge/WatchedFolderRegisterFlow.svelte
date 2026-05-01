<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import {
    lcOcrAvailable,
    lcValidatePath,
    lcWatchRegister,
  } from "../../api";
  import {
    DEFAULT_WATCHED_FOLDER_CONFIG,
    type WatchedFolderConfig,
    type WatchedFolderRegisterResponse,
  } from "../../types";

  interface Props {
    onCancel: () => void;
    /** Fires after a successful register; the parent reloads the
     *  list and exits the flow. The `corpus_id` from the response
     *  is passed so the parent can highlight the new card. */
    onRegistered: (resp: WatchedFolderRegisterResponse) => void;
  }

  let { onCancel, onRegistered }: Props = $props();

  let path: string = $state("");
  let displayName: string = $state("");
  let validationError: string = $state("");
  let busy = $state(false);
  let registerError: string = $state("");

  // Sync settings — exposed in a collapsible "advanced" panel so a
  // first-time user just hits Register; power users can override.
  let showAdvanced = $state(false);
  let sweepSecs: number = $state(
    DEFAULT_WATCHED_FOLDER_CONFIG.sweep_interval_secs,
  );
  let graceDays: number = $state(7);
  let absThreshold: number = $state(
    DEFAULT_WATCHED_FOLDER_CONFIG.deletion_guard.absolute_threshold,
  );
  let fracThreshold: number = $state(
    DEFAULT_WATCHED_FOLDER_CONFIG.deletion_guard.fractional_threshold,
  );
  let guardEnabled: boolean = $state(true);
  let followSymlinks: boolean = $state(false);
  let withOcr: boolean = $state(false);
  /// Whether the daemon has an OCR runtime context installed. Probed
  /// once on mount; the toggle is hidden when false so users on a
  /// build without bundled Tesseract don't see a switch they can't
  /// actually turn on.
  let ocrAvailable = $state(false);

  onMount(async () => {
    try {
      ocrAvailable = await lcOcrAvailable();
    } catch {
      ocrAvailable = false;
    }
  });

  // Last segment of the path — the suggested display name.
  let folderBasename = $derived.by(() => {
    if (!path) return "";
    const trimmed = path.replace(/[\\/]+$/, "");
    const segs = trimmed.split(/[\\/]/);
    return segs[segs.length - 1] || trimmed;
  });

  $effect(() => {
    // Auto-fill display name when the user picks a path and hasn't
    // typed one yet.
    if (folderBasename && !displayName) {
      displayName = folderBasename;
    }
  });

  async function browse() {
    busy = true;
    validationError = "";
    try {
      const picked = await open({ multiple: false, directory: true });
      if (typeof picked === "string") {
        path = picked;
        await validate();
      }
    } catch (e) {
      validationError = `Could not open picker: ${e}`;
    }
    busy = false;
  }

  async function validate() {
    if (!path) {
      validationError = "";
      return;
    }
    try {
      const v = await lcValidatePath(path);
      if (!v.exists) validationError = "That path doesn't exist.";
      else if (!v.is_dir) validationError = "Pick a folder, not a file.";
      else if (!v.readable)
        validationError = "Sovereign can't read that folder.";
      else validationError = "";
    } catch (e) {
      validationError = `Validation failed: ${e}`;
    }
  }

  async function register() {
    if (!path || validationError) return;
    busy = true;
    registerError = "";
    const config: WatchedFolderConfig = {
      follow_symlinks: followSymlinks,
      deletion_guard: {
        absolute_threshold: Math.max(0, Math.floor(absThreshold)),
        fractional_threshold: Math.max(0, Math.min(1, fracThreshold)),
        enabled: guardEnabled,
      },
      sweep_interval_secs: Math.max(60, Math.floor(sweepSecs)),
      soft_delete_grace_secs: Math.max(86_400, Math.floor(graceDays * 86_400)),
      exclude_globs: [],
      // Only honour the toggle when the runtime can — keeps an
      // ocr-off-by-disk reality from sneaking through to a corpus
      // configured with-ocr-on.
      with_ocr: withOcr && ocrAvailable,
    };
    try {
      const resp = await lcWatchRegister(path, displayName || undefined, config);
      onRegistered(resp);
    } catch (e) {
      registerError = String(e);
    }
    busy = false;
  }

  let canRegister = $derived(!!path && !validationError && !busy);
</script>

<section class="reg">
  <header class="head">
    <h2 class="title">Watch a folder.</h2>
    <p class="lede">
      Sovereign keeps the index in sync with this folder. Drop files in,
      edit them, or remove them — changes appear in search within a few
      minutes. Sovereign never writes to the folder.
    </p>
  </header>

  <div class="row">
    <label class="label" for="wf-path">Folder</label>
    <div class="path-row">
      <input
        id="wf-path"
        type="text"
        class="path-input"
        placeholder="No folder selected"
        bind:value={path}
        onblur={validate}
        readonly
      />
      <button class="ghost" onclick={browse} disabled={busy}>Browse…</button>
    </div>
    {#if validationError}
      <p class="error">{validationError}</p>
    {/if}
  </div>

  <div class="row">
    <label class="label" for="wf-name">Display name</label>
    <input
      id="wf-name"
      type="text"
      class="name-input"
      placeholder={folderBasename || "(folder basename)"}
      bind:value={displayName}
    />
  </div>

  <button
    type="button"
    class="advanced-toggle"
    onclick={() => (showAdvanced = !showAdvanced)}
    aria-expanded={showAdvanced}
  >
    {showAdvanced ? "Hide" : "Show"} sync settings
  </button>

  {#if showAdvanced}
    <div class="advanced">
      <div class="row">
        <label class="label" for="wf-sweep">Sweep interval (seconds)</label>
        <input
          id="wf-sweep"
          type="number"
          min="60"
          max="3600"
          bind:value={sweepSecs}
        />
        <p class="hint">
          How often Sovereign checks the folder for changes. Floored at
          60s — tighter intervals just waste disk and shrink the
          deletion-guard reaction window.
        </p>
      </div>

      <div class="row">
        <label class="label" for="wf-grace">Soft-delete grace (days)</label>
        <input
          id="wf-grace"
          type="number"
          min="1"
          max="90"
          bind:value={graceDays}
        />
        <p class="hint">
          A removed file stays revivable for this long. Restoring it
          (same content) skips re-extraction.
        </p>
      </div>

      <fieldset class="guard">
        <legend>Deletion safety guard</legend>
        <label class="checkbox">
          <input type="checkbox" bind:checked={guardEnabled} />
          <span>Pause sweeps before applying suspicious deletions</span>
        </label>
        <div class="row guard-row" class:dim={!guardEnabled}>
          <label class="label" for="wf-abs">Pause if &gt;= </label>
          <input
            id="wf-abs"
            type="number"
            min="0"
            max="1000000"
            bind:value={absThreshold}
            disabled={!guardEnabled}
          />
          <span>files would be deleted</span>
        </div>
        <div class="row guard-row" class:dim={!guardEnabled}>
          <label class="label" for="wf-frac">Pause if &gt;= </label>
          <input
            id="wf-frac"
            type="number"
            step="0.05"
            min="0"
            max="1"
            bind:value={fracThreshold}
            disabled={!guardEnabled}
          />
          <span>fraction of live docs would be deleted</span>
        </div>
      </fieldset>

      <label class="checkbox">
        <input type="checkbox" bind:checked={followSymlinks} />
        <span>Follow symlinks while walking</span>
      </label>

      {#if ocrAvailable}
        <label class="checkbox">
          <input type="checkbox" bind:checked={withOcr} />
          <span>
            OCR scanned PDFs
            <span class="hint inline">
              — adds rasterize + tesseract + cleanup per file. Slower
              than text-layer extraction; only fires when the plain
              extractor returns no text.
            </span>
          </span>
        </label>
      {:else}
        <p class="hint">
          OCR for scanned PDFs isn't available on this build —
          Sovereign couldn't find a Tesseract sidecar. Scanned files
          will land in the "couldn't read" list with a note.
        </p>
      {/if}
    </div>
  {/if}

  {#if registerError}
    <p class="error">{registerError}</p>
  {/if}

  <div class="actions">
    <button class="ghost" onclick={onCancel} disabled={busy}>Cancel</button>
    <button class="primary" onclick={register} disabled={!canRegister}>
      {busy ? "Registering…" : "Watch this folder"}
    </button>
  </div>
</section>

<style>
  .reg {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 28px 32px;
    background: var(--lk-paper);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .head { margin-bottom: 4px; }
  .title {
    margin: 0 0 6px;
    font-family: var(--lk-font-display);
    font-size: var(--lk-size-lead);
    font-weight: 600;
    color: var(--lk-ink);
  }
  .lede {
    margin: 0;
    max-width: 64ch;
    font-size: var(--lk-size-body);
    color: var(--lk-ink-soft);
    line-height: 1.5;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .label {
    font-size: var(--lk-size-meta);
    font-weight: 500;
    color: var(--lk-ink-soft);
  }
  .path-row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
  }
  .path-input,
  .name-input,
  input[type="number"] {
    padding: 8px 10px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: 6px;
    color: var(--lk-ink);
    font-size: var(--lk-size-body);
  }
  .path-input { font-family: var(--lk-font-mono, monospace); }
  .ghost {
    padding: 8px 14px;
    background: transparent;
    border: 1px solid var(--lk-rule);
    border-radius: 6px;
    color: var(--lk-ink);
    cursor: pointer;
  }
  .ghost:hover { border-color: var(--lk-crown); }
  .primary {
    padding: 8px 18px;
    background: var(--lk-crown);
    border: 1px solid var(--lk-crown);
    border-radius: 6px;
    color: white;
    font-weight: 500;
    cursor: pointer;
  }
  .primary:disabled,
  .ghost:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .advanced-toggle {
    align-self: flex-start;
    padding: 0;
    background: transparent;
    border: none;
    color: var(--lk-crown-light);
    font-size: var(--lk-size-meta);
    cursor: pointer;
  }
  .advanced-toggle:hover { text-decoration: underline; }
  .advanced {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 14px;
    background: var(--lk-paper-deep);
    border-radius: 6px;
  }
  .guard {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 0;
    padding: 10px 14px;
    border: 1px solid var(--lk-rule);
    border-radius: 6px;
  }
  .guard legend {
    padding: 0 6px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }
  .guard-row {
    flex-direction: row;
    align-items: center;
    gap: 6px;
  }
  .guard-row.dim { opacity: 0.5; }
  .guard-row input[type="number"] { width: 80px; }
  .checkbox {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }
  .hint {
    margin: 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    line-height: 1.4;
  }
  .hint.inline {
    display: inline;
    margin-left: 4px;
  }
  .error {
    margin: 0;
    padding: 8px 12px;
    border-left: 3px solid var(--lk-err);
    background: var(--lk-err-wash);
    color: var(--lk-ink);
    font-size: var(--lk-size-meta);
  }
</style>
