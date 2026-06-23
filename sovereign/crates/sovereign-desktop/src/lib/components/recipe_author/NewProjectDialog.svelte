<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // New-project modal: title + charter (markdown). The charter is
  // stored verbatim on the FeatureRow's `charter_md` and rendered
  // back via the `CharterSummary` card.
  import { dialogFocus } from "@sovereign/chat-ui";

  let {
    onCancel,
    onCreate,
  }: {
    onCancel: () => void;
    onCreate: (title: string, charterMd: string) => Promise<void>;
  } = $props();

  let title = $state("");
  let charter = $state("");
  let saving = $state(false);
  let error: string | null = $state(null);

  // Optional one-click starting points so a first-time author doesn't face an
  // empty box. A chip seeds the charter (and the title, if still blank); the
  // recipe-author agent turns the source shape into the right recipe
  // (local_file + matching extractor, http_api, web_crawl, …).
  const ARCHETYPES: { label: string; title: string; charter: string }[] = [
    {
      label: "Folder of files",
      title: "My documents",
      charter:
        "# Charter\n\nBuild a corpus from a folder of files already on my\ncomputer (markdown, text, or PDFs).\n\n- Source: a local folder — I'll give the path\n- What it's for: \n- Anything to exclude: ",
    },
    {
      label: "CSV / spreadsheet",
      title: "My table",
      charter:
        "# Charter\n\nBuild a corpus from a CSV / spreadsheet of records.\n\n- Source: a local .csv — I'll give the path\n- Column holding the main text: \n- What it's for: ",
    },
    {
      label: "Mailbox",
      title: "My mail",
      charter:
        "# Charter\n\nBuild a corpus from an email mailbox.\n\n- Source: a local maildir / mailbox folder — I'll give the path\n- What it's for: \n- Strip quoted replies + signatures: yes",
    },
    {
      label: "Web API",
      title: "API corpus",
      charter:
        "# Charter\n\nBuild a corpus from a public web API.\n\n- API: name + docs URL\n- Which records I want: \n- Auth (if any): ",
    },
    {
      label: "Website",
      title: "Website corpus",
      charter:
        "# Charter\n\nBuild a corpus by crawling a website.\n\n- Site: starting URL\n- Which pages to follow: \n- What it's for: ",
    },
  ];

  function applyArchetype(a: { title: string; charter: string }) {
    if (!title.trim()) title = a.title;
    charter = a.charter;
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!title.trim()) {
      error = "Title is required.";
      return;
    }
    saving = true;
    error = null;
    try {
      await onCreate(title.trim(), charter);
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }
</script>

<div
  class="overlay"
  role="dialog"
  aria-modal="true"
  aria-labelledby="new-project-heading"
  use:dialogFocus={{ onEscape: onCancel }}
>
  <form class="dialog" onsubmit={submit}>
    <h2 id="new-project-heading">New recipe project</h2>
    <p class="hint">
      Give the project a short name. The charter is your domain
      framing — what the corpus is, who it's for, and the boundary
      decisions you've already made. The agent reads it on every turn.
    </p>

    <div class="archetypes" role="group" aria-label="Starting points">
      <span class="archetypes-label">Start from a source (optional):</span>
      {#each ARCHETYPES as a}
        <button type="button" class="chip" onclick={() => applyArchetype(a)}>
          {a.label}
        </button>
      {/each}
    </div>

    <label for="np-title">Title</label>
    <input
      id="np-title"
      type="text"
      bind:value={title}
      placeholder="Marcus — Ninth Circuit case law"
      autocomplete="off"
      data-testid="recipe-author-new-title"
    />

    <label for="np-charter">Charter (markdown)</label>
    <textarea
      id="np-charter"
      bind:value={charter}
      rows="14"
      placeholder={"# Charter\n\nWhat we're building, why, and any\nconstraints already settled."}
      data-testid="recipe-author-new-charter"
    ></textarea>

    {#if error}
      <p class="error">{error}</p>
    {/if}

    <div class="actions">
      <button type="button" class="cancel" onclick={onCancel} disabled={saving}>
        Cancel
      </button>
      <button
        type="submit"
        class="primary"
        disabled={saving}
        data-testid="recipe-author-new-submit"
      >
        {saving ? "Creating…" : "Create project"}
      </button>
    </div>
  </form>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 1.5rem;
  }
  .dialog {
    background: var(--bg, #15171c);
    border: 1px solid var(--border, #2a2c33);
    border-radius: 8px;
    padding: 1.2rem 1.4rem 1.4rem;
    width: min(640px, 100%);
    max-height: calc(100vh - 3rem);
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  h2 {
    margin: 0;
    font-size: 1.1rem;
    font-weight: 600;
  }
  .hint {
    color: var(--muted, #8a8c93);
    font-size: 0.85rem;
    margin: 0 0 0.5rem;
  }
  .archetypes {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
    margin: 0 0 0.3rem;
  }
  .archetypes-label {
    font-size: 0.78rem;
    color: var(--muted, #8a8c93);
    margin-right: 0.2rem;
  }
  .chip {
    padding: 0.25rem 0.6rem;
    font-size: 0.8rem;
    border-radius: 999px;
    border: 1px solid var(--border, #2a2c33);
    background: var(--bg-elevated, transparent);
    color: inherit;
    cursor: pointer;
  }
  .chip:hover {
    background: color-mix(in srgb, var(--lavender) 18%, transparent);
    border-color: color-mix(in srgb, var(--lavender) 40%, transparent);
  }
  label {
    font-size: 0.8rem;
    color: var(--muted, #8a8c93);
    margin-top: 0.5rem;
  }
  input,
  textarea {
    background: var(--bg-elevated);
    border: 1px solid var(--border, #2a2c33);
    color: inherit;
    padding: 0.5rem 0.6rem;
    border-radius: 4px;
    font: inherit;
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.88rem;
    resize: vertical;
  }
  .error {
    color: var(--coral);
    font-size: 0.85rem;
    margin: 0.4rem 0 0;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.8rem;
  }
  button {
    padding: 0.45rem 0.9rem;
    border-radius: 4px;
    border: 1px solid var(--border, #2a2c33);
    background: transparent;
    color: inherit;
    cursor: pointer;
    font-size: 0.88rem;
  }
  button.primary {
    background: var(--lavender-dim);
    border-color: color-mix(in srgb, var(--lavender) 50%, transparent);
  }
  button.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--lavender) 30%, transparent);
  }
  button:disabled {
    opacity: 0.5;
    cursor: progress;
  }
</style>
