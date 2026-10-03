<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Drawer over the live recipe.toml. Collapsed by default — the partner
  // doesn't read TOML in the common path. "Show TOML" reveals it read-only;
  // "Edit" turns it into an editor that validates + saves through the engine.
  //
  // Save contract (recipe_author_save_edited_toml): validate-FIRST — a recipe
  // that doesn't parse is never written; the parse errors come back inline and
  // the draft stays in the textarea so the partner fixes + re-saves. On success
  // the agent picks the edit up next turn via its disk re-read (no round-trip).
  import Card from "./Card.svelte";
  import { recipeAuthorSaveEditedToml } from "../../api";
  import { artifactNoun, artifactTitle, type ArtifactKind } from "../../types";

  let {
    recipeToml,
    featureId,
    artifactKind = "recipe",
  }: {
    recipeToml: string | null;
    featureId: string | null;
    artifactKind?: ArtifactKind;
  } = $props();

  const noun = $derived(artifactNoun(artifactKind));
  const Title = $derived(artifactTitle(artifactKind));

  let expanded = $state(false);
  let editing = $state(false);
  let draft = $state("");
  let saving = $state(false);
  let saveErrors = $state<string[]>([]);
  let savedFlash = $state(false);

  function startEdit() {
    draft = recipeToml ?? "";
    saveErrors = [];
    savedFlash = false;
    editing = true;
    expanded = true;
  }

  function cancelEdit() {
    editing = false;
    saveErrors = [];
  }

  async function save() {
    if (!featureId || saving) return;
    saving = true;
    saveErrors = [];
    try {
      const report = await recipeAuthorSaveEditedToml(featureId, draft);
      if (report.ok) {
        // Written. The dashboard poll refreshes `recipeToml` from disk.
        editing = false;
        savedFlash = true;
        setTimeout(() => (savedFlash = false), 1800);
      } else {
        // Not written — surface the parse errors, keep the draft to fix.
        saveErrors = report.errors.length
          ? report.errors
          : ["recipe failed to parse (no message)"];
      }
    } catch (e) {
      saveErrors = [typeof e === "string" ? e : String(e)];
    } finally {
      saving = false;
    }
  }
</script>

<Card title="{Title} TOML">
  {#if !recipeToml && !editing}
    <p class="muted">No {noun} drafted yet.</p>
  {:else}
    <div class="row">
      <button
        type="button"
        class="toggle"
        onclick={() => (expanded = !expanded)}
        data-testid="recipe-author-toml-toggle"
      >
        {expanded
          ? "Hide TOML"
          : `Show TOML (${(recipeToml ?? "").split("\n").length} lines)`}
      </button>
      {#if expanded && !editing && featureId}
        <button
          type="button"
          class="toggle edit"
          onclick={startEdit}
          data-testid="recipe-author-toml-edit"
        >
          Edit
        </button>
      {/if}
      {#if savedFlash}<span class="saved">✓ saved</span>{/if}
    </div>

    {#if expanded}
      {#if editing}
        <textarea
          class="toml-edit"
          bind:value={draft}
          spellcheck="false"
          autocapitalize="off"
          autocomplete="off"
          data-testid="recipe-author-toml-editor"
        ></textarea>
        {#if saveErrors.length}
          <ul class="errors" data-testid="recipe-author-toml-errors">
            {#each saveErrors as err}
              <li><pre class="err-text">{err}</pre></li>
            {/each}
          </ul>
        {/if}
        <div class="actions">
          <button
            type="button"
            class="run"
            onclick={save}
            disabled={saving}
            data-testid="recipe-author-toml-save"
          >
            {saving ? "saving…" : "Save"}
          </button>
          <button type="button" class="ghost" onclick={cancelEdit} disabled={saving}>
            Cancel
          </button>
          <span class="muted hint">
            Saved only if it parses; the agent picks up your edit next turn.
          </span>
        </div>
      {:else}
        <pre class="toml">{recipeToml}</pre>
      {/if}
    {/if}
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  .row {
    display: flex;
    gap: 0.7rem;
    align-items: baseline;
  }
  .toggle {
    background: transparent;
    border: none;
    color: var(--lavender-light);
    cursor: pointer;
    padding: 0;
    font-size: 0.78rem;
    text-decoration: underline;
  }
  .toggle.edit {
    color: var(--growth, #4caf82);
  }
  .saved {
    color: var(--growth, #4caf82);
    font-size: 0.74rem;
  }
  pre.toml {
    margin: 0.5rem 0 0;
    background: rgba(0, 0, 0, 0.3);
    padding: 0.6rem 0.7rem;
    border-radius: 4px;
    overflow-x: auto;
    font-family: var(--font-mono);
    font-size: 0.74rem;
    line-height: 1.4;
    color: var(--fg, #e6e6e8);
    max-height: 360px;
    overflow-y: auto;
  }
  .toml-edit {
    width: 100%;
    box-sizing: border-box;
    margin: 0.5rem 0 0;
    min-height: 320px;
    resize: vertical;
    background: rgba(0, 0, 0, 0.35);
    border: 1px solid var(--border, #2a2c33);
    border-radius: 4px;
    padding: 0.6rem 0.7rem;
    font-family: var(--font-mono);
    font-size: 0.74rem;
    line-height: 1.4;
    color: var(--fg, #e6e6e8);
    tab-size: 2;
  }
  .toml-edit:focus {
    outline: none;
    border-color: var(--growth, #4caf82);
  }
  .errors {
    list-style: none;
    margin: 0.5rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .errors li {
    background: color-mix(in srgb, var(--coral) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--coral) 35%, transparent);
    border-radius: 4px;
    padding: 0.4rem 0.55rem;
  }
  .err-text {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 0.74rem;
    line-height: 1.4;
    color: var(--fg, #e6e6e8);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    margin-top: 0.5rem;
  }
  .run {
    background: var(--bg-elevated);
    border: 1px solid var(--border, #2a2c33);
    color: var(--fg, #e6e6e8);
    font-size: 0.78rem;
    padding: 4px 12px;
    border-radius: 4px;
    cursor: pointer;
  }
  .run:hover:not(:disabled) {
    border-color: var(--growth, #4caf82);
  }
  .run:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--border, #2a2c33);
    color: var(--muted, #8a8c93);
    font-size: 0.78rem;
    padding: 4px 12px;
    border-radius: 4px;
    cursor: pointer;
  }
  .ghost:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .hint {
    font-size: 0.72rem;
  }
</style>
