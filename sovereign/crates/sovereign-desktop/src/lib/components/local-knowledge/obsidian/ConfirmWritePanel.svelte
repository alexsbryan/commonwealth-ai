<script lang="ts">
  import { onMount } from "svelte";

  import { lcCheckGit } from "../../../api";
  import type { GitStatus, VaultPreview } from "../../../types";

  interface Props {
    corpusId: string;
    preview: VaultPreview;
    onBack: () => void;
    onConfirm: (gitCommit: boolean) => void;
  }

  let { corpusId, preview, onBack, onConfirm }: Props = $props();

  let git: GitStatus | null = $state(null);
  let gitChecked = $state(false);
  let commitBeforeWrite = $state(false);

  onMount(async () => {
    try {
      git = await lcCheckGit(corpusId);
    } catch {
      git = null;
    } finally {
      gitChecked = true;
    }
  });
</script>

<div class="confirm-panel">
  <div class="icon" aria-hidden="true">⚠</div>
  <h3>About to write to your vault</h3>

  <div class="details">
    <div class="item">
      <span>Notes that will be tagged</span>
      <strong>{preview.tagged_notes}</strong>
    </div>
    <div class="item">
      <span>Notes that won't be touched</span>
      <strong>{preview.outlier_count} (outliers)</strong>
    </div>
    <div class="item">
      <span>Index notes to be created</span>
      <strong>{preview.clusters.length}</strong>
    </div>
    <div class="item">
      <span>Your existing tags</span>
      <strong>Preserved exactly</strong>
    </div>
    <div class="item">
      <span>Namespace written</span>
      <strong>{preview.namespace}/*</strong>
    </div>
  </div>

  {#if gitChecked && git}
    <div class="git-notice">
      <span class="git-icon" aria-hidden="true">⎇</span>
      <div class="git-body">
        <strong>Git repository detected</strong> on branch
        <code>{git.current_branch}</code>.
        Sovereign can commit your current state before writing.
        {#if git.has_uncommitted_changes}
          <div class="git-sub">
            You have uncommitted changes — they'll be bundled into the
            pre-write commit.
          </div>
        {/if}
      </div>
      <label class="git-toggle">
        <input type="checkbox" bind:checked={commitBeforeWrite} />
        Commit before tagging
      </label>
    </div>
  {/if}

  <p class="reversibility">
    All changes can be reversed from the Restore previous state section
    below. Nothing permanent is happening here.
  </p>

  <p class="footnote">
    Obsidian's own File Recovery plugin, if enabled, provides an
    independent backup that Sovereign does not control or depend on.
  </p>

  <div class="actions">
    <button class="btn-secondary" onclick={onBack}>Go back</button>
    <button
      class="btn-primary"
      onclick={() => onConfirm(commitBeforeWrite)}
    >
      Write {preview.tagged_notes} tags
    </button>
  </div>
</div>

<style>
  .confirm-panel {
    padding: 16px 0;
    max-width: 580px;
  }
  .icon {
    font-size: 28px;
    margin-bottom: 6px;
  }
  h3 {
    font-size: 16px;
    font-weight: 500;
    margin: 0 0 16px;
  }
  .details {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 14px 16px;
    border: 1px solid var(--color-border, #d4d4d4);
    border-radius: 6px;
    margin-bottom: 16px;
  }
  .item {
    display: flex;
    justify-content: space-between;
    font-size: 13px;
  }
  .item span {
    color: var(--color-text-muted, #6b6b6b);
  }
  .git-notice {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 12px 14px;
    background: color-mix(in srgb, var(--color-accent, #3a5fc9) 6%, transparent);
    border: 1px solid color-mix(in srgb, var(--color-accent, #3a5fc9) 30%, transparent);
    border-radius: 6px;
    margin-bottom: 16px;
  }
  .git-icon {
    font-size: 18px;
    color: var(--color-accent, #3a5fc9);
  }
  .git-body {
    flex: 1;
    font-size: 13px;
  }
  .git-body code {
    background: var(--color-surface-subtle, #f4f4f4);
    padding: 1px 5px;
    border-radius: 3px;
    font-size: 12px;
  }
  .git-sub {
    margin-top: 6px;
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
  }
  .git-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    flex-shrink: 0;
  }
  .reversibility {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin: 0 0 8px;
    line-height: 1.4;
  }
  .footnote {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    font-style: italic;
    margin: 0 0 16px;
  }
  .actions {
    display: flex;
    gap: 12px;
    margin-top: 20px;
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
  .btn-primary:hover {
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
