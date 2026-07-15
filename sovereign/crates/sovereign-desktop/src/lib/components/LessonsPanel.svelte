<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  LessonsPanel — the "What I've learned" settings pane (TEACHABLE §5).

  The trust story, boring on purpose: every saved lesson as one plain
  sentence, the moment it was taught behind a disclosure, a toggle,
  and a real delete. Superseded lessons stay visible struck-through
  with "replaced by" so the history is legible without accreting.
  No in-pane text editing in P0 — the edit affordance lives on the
  capture card before a lesson exists; afterwards, delete-and-reteach
  in chat IS the product's own loop.

  Copy discipline: the enforcement chip renders USER language only
  ("answer length" / "wording check" / "standing reminder") — the
  no-jargon bar applies to our own settings copy. Pinned by
  LessonsPanel.test.ts, which asserts the raw tokens never render.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { listLessons, setLessonEnabled, deleteLesson } from "../api";
  import type { LessonRow } from "../types";

  let rows: LessonRow[] = $state([]);
  let loading = $state(true);
  let loadError: string | null = $state(null);
  let expanded: string | null = $state(null);
  let busyId: string | null = $state(null);
  let actionError: string | null = $state(null);

  // Kept-by chip: how the lesson's promise is kept, in user language.
  // NEVER render the raw enforcement tokens.
  const ENFORCEMENT_LABEL: Record<string, string> = {
    param: "answer length",
    transform: "wording check",
    prompt: "standing reminder",
  };

  function keptBy(row: LessonRow): string {
    return ENFORCEMENT_LABEL[row.enforcement] ?? "rule";
  }

  /** The successor that replaced a retired row — the `supersedes`
   *  pointer lives on the NEW note, so invert it client-side. */
  function replacedBy(row: LessonRow): LessonRow | undefined {
    return rows.find((r) => r.supersedes === row.id);
  }

  async function refresh() {
    loading = true;
    loadError = null;
    try {
      rows = await listLessons();
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(refresh);

  function toggleExpanded(id: string) {
    expanded = expanded === id ? null : id;
  }

  function fmtDate(unix: number): string {
    try {
      return new Date(unix * 1000).toLocaleDateString();
    } catch {
      return String(unix);
    }
  }

  async function handleToggle(row: LessonRow) {
    if (busyId) return;
    busyId = row.id;
    actionError = null;
    const next = !row.enabled;
    // Optimistic flip; revert on failure.
    rows = rows.map((r) => (r.id === row.id ? { ...r, enabled: next } : r));
    try {
      await setLessonEnabled(row.id, next);
    } catch (e) {
      rows = rows.map((r) => (r.id === row.id ? { ...r, enabled: !next } : r));
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      busyId = null;
    }
  }

  async function handleDelete(row: LessonRow) {
    if (busyId) return;
    busyId = row.id;
    actionError = null;
    try {
      await deleteLesson(row.id);
      rows = rows.filter((r) => r.id !== row.id);
      if (expanded === row.id) expanded = null;
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      busyId = null;
    }
  }
</script>

<section class="doc-section">
  <span class="section-eyebrow">taught in chat &middot; owned here</span>
  <h2 class="doc-h2">What I've learned</h2>
  <p class="doc-intro">
    Coach it in chat — "keep answers short from now on" — and what it keeps
    is listed here, in your own words, with the moment you taught it. Most of
    what you teach becomes how the app works; a few things need standing
    reminders, and it keeps only a handful so each one stays sharp. Toggle
    a lesson off to pause it; delete removes it for good. Nothing is learned
    without the card you saved, and nothing here is hidden.
  </p>

  {#if loading}
    <p class="lp-muted">Loading lessons…</p>
  {:else if loadError}
    <p class="lp-error">Couldn't read lessons: {loadError}</p>
  {:else if rows.length === 0}
    <div class="lp-empty">
      <p class="lp-empty-title">Nothing learned yet</p>
      <p class="lp-muted">
        Teach it in chat — when you save a "Learn this?" card, the lesson
        appears here.
      </p>
    </div>
  {:else}
    {#if actionError}
      <p class="lp-error" role="alert">{actionError}</p>
    {/if}
    <ul class="lp-list">
      {#each rows as row (row.id)}
        {@const retired = row.retired_at !== null}
        {@const successor = retired ? replacedBy(row) : undefined}
        <li class="lp-row" class:lp-retired={retired}>
          <div class="lp-main">
            <span class="lp-display" class:lp-struck={retired}>
              {row.display}
            </span>
            <span class="lp-chip" title="How this is kept">{keptBy(row)}</span>
          </div>
          {#if retired && successor}
            <div class="lp-replaced">
              replaced by: <em>{successor.display}</em>
            </div>
          {/if}
          <div class="lp-actions">
            <button
              class="lp-link"
              onclick={() => toggleExpanded(row.id)}
              aria-expanded={expanded === row.id}
            >
              {expanded === row.id ? "Hide" : "Taught from"}
            </button>
            {#if !retired}
              <label class="lp-toggle">
                <input
                  type="checkbox"
                  checked={row.enabled}
                  disabled={busyId === row.id}
                  onchange={() => handleToggle(row)}
                />
                <span>{row.enabled ? "On" : "Off"}</span>
              </label>
            {/if}
            <button
              class="lp-link lp-delete"
              onclick={() => handleDelete(row)}
              disabled={busyId === row.id}
            >
              Delete
            </button>
          </div>
          {#if expanded === row.id}
            <div class="lp-provenance">
              <blockquote class="lp-excerpt">
                "{row.taught_from.excerpt}"
              </blockquote>
              <div class="lp-date">Taught {fmtDate(row.created)}</div>
              {#if row.drafted_display}
                <div class="lp-date">
                  You refined the draft — it originally read:
                  <em>"{row.drafted_display}"</em>
                </div>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  /* ── Document chrome ──────────────────────────────────────────
     The shared .doc-* rules live in SettingsPanel and are scoped to
     THAT component, so this child panel inherited none of them — it
     rendered flush to the container with an unstyled heading. Re-
     declare them here (scoped to this panel) so it matches its
     siblings, with a little extra breathing room. */
  .doc-section {
    flex: 1;
    padding: 30px 30px 28px;
    max-width: 660px;
  }
  .section-eyebrow {
    display: block;
    font-family: var(--font-sans);
    font-size: 0.66rem;
    font-weight: 600;
    color: var(--lavender);
    letter-spacing: 0.12em;
    text-transform: uppercase;
    margin-bottom: 6px;
  }
  .doc-h2 {
    margin: 0 0 10px;
    font-size: 1.05rem;
    font-weight: 600;
    line-height: 1.2;
    letter-spacing: -0.015em;
    color: var(--text-primary);
  }
  .doc-intro {
    margin: 0 0 22px;
    font-size: 0.82rem;
    line-height: 1.65;
    color: var(--text-muted);
  }

  .lp-muted {
    color: var(--text-muted);
    font-size: 0.88rem;
  }
  .lp-error {
    color: var(--error, #c0564f);
    font-size: 0.88rem;
  }
  .lp-empty {
    margin-top: 4px;
    padding: 22px;
    text-align: center;
    background: color-mix(in srgb, var(--accent) 3%, var(--bg-secondary));
    border: 1px dashed color-mix(in srgb, var(--accent) 28%, var(--border));
    border-radius: var(--radius-lg);
  }
  .lp-empty-title {
    margin: 0 0 6px;
    font-weight: 600;
    color: var(--text-secondary);
  }

  .lp-list {
    list-style: none;
    margin: 16px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .lp-row {
    padding: 14px 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    transition:
      border-color 140ms ease,
      background 140ms ease,
      box-shadow 140ms ease;
  }
  .lp-row:hover {
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border));
    background: color-mix(in srgb, var(--accent) 4%, var(--bg-secondary));
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.18);
  }
  .lp-row.lp-retired {
    opacity: 0.65;
  }
  .lp-row.lp-retired:hover {
    /* Retired rows are history, not actionable — don't invite a click. */
    border-color: var(--border);
    background: var(--bg-secondary);
    box-shadow: none;
  }

  .lp-main {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .lp-display {
    flex: 1;
    font-family: var(--font-serif);
    font-variation-settings: "opsz" 14;
    font-size: 0.98rem;
    line-height: 1.5;
    color: var(--text-primary);
  }
  .lp-struck {
    text-decoration: line-through;
    color: var(--text-muted);
  }
  .lp-chip {
    flex-shrink: 0;
    padding: 2px 9px;
    font-family: var(--font-sans);
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.06em;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
    border-radius: 999px;
    white-space: nowrap;
  }

  .lp-replaced {
    margin-top: 4px;
    font-family: var(--font-sans);
    font-size: 0.78rem;
    color: var(--text-muted);
  }

  .lp-actions {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-top: 8px;
  }
  .lp-link {
    background: none;
    border: none;
    padding: 0;
    font-family: var(--font-sans);
    font-size: 0.78rem;
    color: var(--text-secondary);
    cursor: pointer;
    text-decoration: underline;
    text-underline-offset: 3px;
  }
  .lp-link:hover:not(:disabled) {
    color: var(--text-primary);
  }
  .lp-link:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .lp-delete:hover:not(:disabled) {
    color: var(--error, #c0564f);
  }
  .lp-toggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-family: var(--font-sans);
    font-size: 0.78rem;
    color: var(--text-secondary);
    cursor: pointer;
  }

  .lp-provenance {
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px dashed var(--border);
  }
  .lp-excerpt {
    margin: 0 0 6px;
    padding-left: 12px;
    border-left: 2px solid color-mix(in srgb, var(--accent) 45%, transparent);
    font-family: var(--font-serif);
    font-variation-settings: "opsz" 14;
    font-size: 0.9rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .lp-date {
    font-family: var(--font-sans);
    font-size: 0.76rem;
    color: var(--text-muted);
  }
</style>
