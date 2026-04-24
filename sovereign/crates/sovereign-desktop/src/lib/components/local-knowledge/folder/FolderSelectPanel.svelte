<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { lcValidatePath } from "../../../api";
  import InkStamp from "../../onboarding/InkStamp.svelte";

  interface Props {
    initialPath?: string | null;
    onSelected: (path: string) => void;
    onCancel: () => void;
  }

  let { initialPath = null, onSelected, onCancel }: Props = $props();

  let path: string = $state(initialPath ?? "");
  let status: string = $state("");
  let busy = $state(false);
  let manualOpen = $state(false);

  // Validate on mount if an initial path arrived via drag-drop.
  $effect(() => {
    if (initialPath && !status) {
      void validate();
    }
  });

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
        status = "Pick a folder, not a file.";
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

  function clearSelection() {
    path = "";
    status = "";
    manualOpen = false;
  }

  /// Last segment of the path — the folder's display name. Handles
  /// trailing slashes and both separators.
  let folderName = $derived.by(() => {
    if (!path) return "";
    const trimmed = path.replace(/[\\/]+$/, "");
    const segs = trimmed.split(/[\\/]/);
    return segs[segs.length - 1] || trimmed;
  });

  let canContinue = $derived(!!path && !status);
</script>

<section class="select">
  <header class="head">
    <InkStamp size="md" active={!path} />
    <h2 class="title">Pick a folder.</h2>
    <p class="lede">
      PDFs, markdown, and text. Indexed locally — nothing is uploaded.
    </p>
  </header>

  {#if path && !status}
    <!-- Selected state: a press-struck folder plate. Prominent,
         not skeletal. -->
    <article class="plate">
      <span class="plate-glyph" aria-hidden="true">▤</span>
      <div class="plate-body">
        <p class="plate-tag">Selected folder</p>
        <h3 class="plate-name">{folderName}</h3>
        <p class="plate-path" title={path}>{path}</p>
      </div>
      <button
        class="plate-change"
        onclick={browse}
        disabled={busy}
        aria-label="Pick a different folder"
      >
        Change
      </button>
    </article>
  {:else}
    <!-- Empty / invalid state: a drop-zone that doubles as a
         browse button. Dashed amethyst rule idle → gold foil on
         hover. Reads as an inviting target, not a form field. -->
    <button
      type="button"
      class="dropzone"
      class:is-error={!!status}
      onclick={browse}
      disabled={busy}
    >
      <span class="dz-mark" aria-hidden="true">▤</span>
      <span class="dz-primary">
        {busy ? "Opening picker…" : "Browse for a folder"}
      </span>
      <span class="dz-secondary">— or drag one into this window —</span>
    </button>
    {#if status}
      <p class="status">{status}</p>
    {/if}
  {/if}

  <!-- Manual path entry — collapsed by default. The browse + drop
       flow covers 95% of cases; the power-user entry sits quietly
       underneath. -->
  <div class="manual" class:is-open={manualOpen}>
    <button
      type="button"
      class="manual-toggle"
      onclick={() => (manualOpen = !manualOpen)}
    >
      {manualOpen ? "Hide manual entry" : "Type a path instead"}
    </button>
    {#if manualOpen}
      <div class="manual-row">
        <input
          id="path-input"
          type="text"
          bind:value={path}
          onblur={validate}
          placeholder="/path/to/documents"
          aria-label="Folder path"
        />
        <button
          type="button"
          class="lk-btn lk-btn--quiet"
          onclick={validate}
          disabled={!path || busy}
        >
          Check
        </button>
      </div>
    {/if}
  </div>

  <footer class="actions">
    <button class="lk-btn lk-btn--quiet" onclick={onCancel}>Cancel</button>
    {#if path && !status}
      <button
        class="lk-btn lk-btn--ghost"
        onclick={clearSelection}
        disabled={busy}
      >
        Clear
      </button>
    {/if}
    <button
      class="lk-btn lk-btn--mark"
      onclick={confirm}
      disabled={!canContinue || busy}
    >
      Continue →
    </button>
  </footer>
</section>

<style>
  .select {
    max-width: 640px;
    display: flex;
    flex-direction: column;
    gap: 20px;
    color: var(--lk-ink);
    animation: lk-fade-in 260ms ease-out both;
  }

  .head {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
  }
  .title {
    margin: 0;
    font-family: var(--font-serif);
    font-style: italic;
    font-size: 2rem;
    font-weight: 500;
    line-height: 1.08;
    letter-spacing: -0.01em;
    color: var(--accent-light);
  }
  .lede {
    margin: 0;
    font-size: 0.94rem;
    color: var(--lk-ink-soft);
    max-width: 58ch;
    line-height: 1.55;
  }

  /* ── Dropzone (empty state) ──────────────────────────────── */
  .dropzone {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 38px 24px;
    background: var(--lk-paper);
    color: var(--lk-ink);
    border: 1.5px dashed var(--border-bright);
    border-radius: var(--radius-lg, 10px);
    cursor: pointer;
    font-family: var(--font-sans);
    transition:
      border-color 180ms ease,
      background 180ms ease,
      box-shadow 180ms ease,
      transform 180ms ease;
  }
  .dropzone:hover:not(:disabled),
  .dropzone:focus-visible {
    border-color: var(--accent);
    border-style: solid;
    background: var(--accent-dim);
    box-shadow: inset 0 0 0 1px var(--accent-dim), 0 4px 18px var(--accent-glow);
    outline: none;
    transform: translateY(-1px);
  }
  .dropzone:disabled {
    opacity: 0.55;
    cursor: progress;
  }
  .dropzone.is-error {
    border-color: var(--lk-err);
    background: var(--lk-err-wash);
  }
  .dz-mark {
    font-size: 1.6rem;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 10px rgba(201, 168, 76, 0.35));
  }
  .dz-primary {
    font-size: 0.98rem;
    font-weight: 600;
    color: var(--text-primary);
    letter-spacing: -0.005em;
  }
  .dz-secondary {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    letter-spacing: 0.06em;
    color: var(--text-muted);
  }

  /* ── Plate (selected state) ──────────────────────────────── */
  /* A "press-struck" folder plate — 2px gold rail + small glow.
     Answers the "skeleton" feeling by carrying real visual weight
     once the user has chosen something. */
  .plate {
    position: relative;
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: 16px;
    align-items: center;
    padding: 16px 18px 16px 20px;
    background: var(--lk-paper);
    border: 1px solid var(--accent);
    border-radius: var(--radius-lg, 10px);
    box-shadow:
      inset 0 1px 0 rgba(223, 192, 104, 0.12),
      0 2px 14px var(--accent-glow);
    animation: plate-strike 280ms cubic-bezier(0.2, 0.8, 0.2, 1) both;
  }
  .plate::before {
    content: "";
    position: absolute;
    left: 0;
    top: 10px;
    bottom: 10px;
    width: 3px;
    background: var(--accent);
    border-radius: 1px;
    box-shadow: 0 0 12px rgba(201, 168, 76, 0.55);
  }
  .plate-glyph {
    font-size: 1.6rem;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 10px rgba(201, 168, 76, 0.4));
  }
  .plate-body {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .plate-tag {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--accent-light);
    font-weight: 600;
  }
  .plate-name {
    margin: 0;
    font-family: var(--font-serif);
    font-style: italic;
    font-size: 1.18rem;
    font-weight: 500;
    color: var(--text-primary);
    line-height: 1.2;
    word-break: break-word;
  }
  .plate-path {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 0.74rem;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .plate-change {
    padding: 6px 12px;
    background: transparent;
    border: 1px solid var(--border-bright);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font-family: var(--font-sans);
    font-size: 0.78rem;
    font-weight: 500;
    cursor: pointer;
    transition: border-color 140ms ease, color 140ms ease;
  }
  .plate-change:hover:not(:disabled) {
    border-color: var(--lavender);
    color: var(--lavender-light);
  }
  .plate-change:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  @keyframes plate-strike {
    from {
      opacity: 0;
      transform: translateY(-3px);
      box-shadow: inset 0 1px 0 rgba(223, 192, 104, 0.12), 0 0 0 var(--accent-glow);
    }
    to {
      opacity: 1;
      transform: translateY(0);
      box-shadow:
        inset 0 1px 0 rgba(223, 192, 104, 0.12),
        0 2px 14px var(--accent-glow);
    }
  }

  /* ── Manual entry (collapsed by default) ─────────────────── */
  .manual {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .manual-toggle {
    align-self: flex-start;
    background: transparent;
    border: none;
    padding: 0;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.74rem;
    letter-spacing: 0.04em;
    cursor: pointer;
    text-decoration: underline;
    text-decoration-color: var(--border-mid);
    text-underline-offset: 3px;
    transition: color 140ms ease, text-decoration-color 140ms ease;
  }
  .manual-toggle:hover {
    color: var(--lavender-light);
    text-decoration-color: var(--lavender);
  }
  .manual-row {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }
  .manual-row input[type="text"] {
    flex: 1;
    padding: 8px 12px;
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    background: var(--bg-input);
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: 0.82rem;
    transition: border-color 140ms ease;
  }
  .manual-row input[type="text"]:focus {
    outline: 0;
    border-color: var(--accent);
  }

  /* ── Status / footer ─────────────────────────────────────── */
  .status {
    margin: 0;
    padding: 8px 12px;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: var(--lk-err);
    background: var(--lk-err-wash);
    border-left: 2px solid var(--lk-err);
    border-radius: 0 var(--radius) var(--radius) 0;
  }

  .actions {
    display: flex;
    gap: 10px;
    justify-content: flex-end;
    align-items: center;
    margin-top: 4px;
  }
</style>
