<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  interface Props {
    onPickFolder: () => void;
    onPickObsidian: () => void;
    onPickWatchedFolder: () => void;
  }

  let { onPickFolder, onPickObsidian, onPickWatchedFolder }: Props = $props();
</script>

<div class="add">
  <div class="choices">
    <button class="choice" onclick={onPickFolder}>
      <div class="choice-body">
        <span class="choice-title">Folder</span>
        <span class="choice-sub">PDFs and text files. Indexed for search.</span>
      </div>
      <span class="choice-arrow" aria-hidden="true">→</span>
    </button>

    <button class="choice" onclick={onPickWatchedFolder}>
      <div class="choice-body">
        <span class="choice-title">Watch a folder</span>
        <span class="choice-sub">
          svrnmesh keeps a folder in sync — drop, edit, or remove files
          and the index updates within a few minutes. Read-only on source.
        </span>
      </div>
      <span class="choice-arrow" aria-hidden="true">→</span>
    </button>

    <button class="choice" onclick={onPickObsidian}>
      <div class="choice-body">
        <span class="choice-title">Obsidian vault</span>
        <span class="choice-sub">
          Markdown notes. Clustered and tagged, with write-back.
        </span>
      </div>
      <span class="choice-arrow" aria-hidden="true">→</span>
    </button>
  </div>

  <p class="aside">Or drag a folder onto this window.</p>
</div>

<style>
  .add {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .choices {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 12px;
  }
  .choice {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: 14px;
    padding: 16px 18px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    text-align: left;
    cursor: pointer;
    color: var(--lk-ink);
    transition:
      border-color 160ms ease,
      background 160ms ease,
      transform 160ms ease;
  }
  .choice:hover {
    border-color: var(--lk-crown);
    background: var(--lk-crown-wash);
    transform: translateY(-1px);
  }
  .choice:focus-visible {
    outline: 2px solid var(--lk-crown);
    outline-offset: 2px;
  }

  .choice-body {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .choice-title {
    font-size: var(--lk-size-lead);
    font-weight: 500;
    color: var(--lk-ink);
  }
  .choice-sub {
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    line-height: 1.4;
  }
  .choice-arrow {
    font-size: 1.125rem;
    color: var(--lk-ink-faded);
    transition: transform 160ms ease, color 160ms ease;
  }
  .choice:hover .choice-arrow {
    color: var(--lk-crown-light);
    transform: translateX(3px);
  }

  .aside {
    margin: 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }
</style>
