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

<section class="confirm">
  <header class="head">
    <h3 class="title">About to write to your vault</h3>
    <p class="lede">All changes are reversible.</p>
  </header>

  <dl class="terms">
    <div class="term">
      <dt>Notes to tag</dt>
      <dd class="term-num"><span class="lk-num">{preview.tagged_notes}</span></dd>
    </div>
    <div class="term">
      <dt>Notes left untouched</dt>
      <dd><span class="lk-num">{preview.outlier_count}</span> outliers</dd>
    </div>
    <div class="term">
      <dt>Index notes to create</dt>
      <dd class="term-num"><span class="lk-num">{preview.clusters.length}</span></dd>
    </div>
    <div class="term">
      <dt>Your existing tags</dt>
      <dd class="term-affirm">Preserved exactly</dd>
    </div>
    <div class="term">
      <dt>Namespace</dt>
      <dd><code class="term-code">{preview.namespace}/*</code></dd>
    </div>
  </dl>

  {#if gitChecked && git}
    <div class="git">
      <p class="git-line">
        <span class="lk-label git-label">Git</span>
        Branch <code>{git.current_branch}</code>. Sovereign can commit your
        current state before writing.
        {#if git.has_uncommitted_changes}
          Uncommitted changes will be bundled into that commit.
        {/if}
      </p>
      <label class="git-toggle">
        <input type="checkbox" bind:checked={commitBeforeWrite} />
        <span>Commit before tagging</span>
      </label>
    </div>
  {/if}

  <p class="assurance">
    All changes can be reversed from the Restore section. Obsidian's File
    Recovery plugin, if enabled, is an independent backup Sovereign doesn't
    touch.
  </p>

  <div class="actions">
    <button class="lk-btn lk-btn--quiet" onclick={onBack}>Back</button>
    <button class="lk-btn lk-btn--mark" onclick={() => onConfirm(commitBeforeWrite)}>
      Write {preview.tagged_notes} tags
    </button>
  </div>
</section>

<style>
  .confirm {
    padding: 4px 0;
    animation: lk-fade-in 300ms ease-out both;
  }

  .head {
    margin-bottom: 18px;
  }
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
  }

  .terms {
    margin: 0 0 18px;
    padding: 6px 16px;
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    background: var(--lk-paper-deep);
  }
  .term {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 14px;
    align-items: baseline;
    padding: 10px 0;
    border-bottom: 1px solid var(--lk-rule-soft);
  }
  .term:last-child { border-bottom: 0; }
  .term dt {
    margin: 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }
  .term dd {
    margin: 0;
    font-size: var(--lk-size-body);
    color: var(--lk-ink);
    text-align: right;
  }
  .term-num .lk-num {
    font-size: 1.25rem;
  }
  .term-affirm {
    color: var(--lk-crown-light);
  }
  .term-code {
    font-family: var(--lk-font-mono);
    font-size: 12.5px;
    color: var(--lk-stamp-ink);
    background: transparent;
  }

  .git {
    margin-bottom: 16px;
    padding: 12px 14px;
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    background: var(--lk-paper-subtle);
  }
  .git-line {
    margin: 0 0 8px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    line-height: 1.5;
  }
  .git-label {
    margin-right: 6px;
    color: var(--lk-stamp-ink);
  }
  .git-line code {
    font-family: var(--lk-font-mono);
    font-size: 12px;
    color: var(--lk-ink);
    background: transparent;
    padding: 0 2px;
    border-bottom: 1px dotted var(--lk-ink-faded);
  }
  .git-toggle {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--lk-ink);
    cursor: pointer;
  }
  .git-toggle input[type="checkbox"] {
    accent-color: var(--lk-stamp);
  }

  .assurance {
    margin: 0 0 18px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    max-width: 62ch;
    line-height: 1.5;
  }

  .actions {
    display: flex;
    gap: 10px;
    justify-content: flex-end;
  }
</style>
