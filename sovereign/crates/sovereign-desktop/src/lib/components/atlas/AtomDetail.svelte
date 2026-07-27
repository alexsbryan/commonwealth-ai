<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Atom Inspector — full detail view.
  //
  // Layout: header (type pill, name, salience) → type-dispatched
  // body → evidence → related atoms → cross-corpus bridges →
  // Phase 2 edit affordances slot (empty today).
  //
  // The body dispatch is a {#if} chain over `atom.atom_type` rather
  // than a registry — 8 known variants, closed set, no extension
  // point. Keeping it inline reads faster than chasing through a
  // dynamic component map for the same outcome.

  import { onMount, setContext } from "svelte";
  import { atlasGetAtomDetail } from "../../api";
  import { readingNavigation } from "../../stores/readingNavigation.svelte";
  import type {
    AtomDetail,
    AtomType,
    EvidenceExcerpt,
    RelatedAtom,
  } from "../../types";
  import {
    ATOM_LINK_CONTEXT_KEY,
    type AtomLinkResolver,
  } from "./AtomLink.svelte";

  import EntityBody from "./types/EntityBody.svelte";
  import EventBody from "./types/EventBody.svelte";
  import StateBody from "./types/StateBody.svelte";
  import RelationBody from "./types/RelationBody.svelte";
  import ClaimBody from "./types/ClaimBody.svelte";
  import QuestionBody from "./types/QuestionBody.svelte";
  import ConfigurationBody from "./types/ConfigurationBody.svelte";
  import ArgumentReconstructionBody from "./types/ArgumentReconstructionBody.svelte";

  interface Props {
    corpusId: string;
    atomId: string;
    /** Back to the corpus browse view. */
    onBack: () => void;
    /** Navigate to another atom's detail page (used by the Related
     *  list to drill into a neighbour). */
    onSelectAtom?: (atomId: string) => void;
    /** Move 4 (Ask↔Explore continuity): when set, an "Ask about this"
     *  affordance appears that hands the atom's name back to the host —
     *  a notebook's Explore tab uses it to switch to Ask, seeded. */
    onAskAbout?: (name: string) => void;
  }

  let { corpusId, atomId, onBack, onSelectAtom, onAskAbout }: Props = $props();

  const ATOM_TYPE_LABEL: Record<AtomType, string> = {
    Entity: "Entity",
    Event: "Event",
    State: "State",
    Relation: "Relation",
    Claim: "Claim",
    Question: "Question",
    Configuration: "Configuration",
    ArgumentReconstruction: "Argument",
  };

  let detail: AtomDetail | null = $state(null);
  let loading = $state(true);
  let error: string | null = $state(null);

  // Provide the AtomLink resolver context once, with closures that
  // read the live `detail` + `onSelectAtom` values. Body components
  // that mount under this tree call `labelFor` / `navigate` and
  // automatically see the current detail's referenced_atoms map —
  // no prop drilling, no per-rerender setContext.
  setContext<AtomLinkResolver>(ATOM_LINK_CONTEXT_KEY, {
    labelFor: (atomId: string) => detail?.referenced_atoms?.[atomId],
    navigate: (atomId: string) => onSelectAtom?.(atomId),
  });

  // Re-fetch whenever atomId changes (e.g., user clicks a related
  // atom inside the detail view).
  $effect(() => {
    const id = atomId;
    const cid = corpusId;
    void (async () => {
      loading = true;
      error = null;
      detail = null;
      try {
        const result = await atlasGetAtomDetail(cid, id);
        if (!result) {
          error = `Atom ${id} not found in ${cid}. It may have been renumbered by a recent re-extraction.`;
        } else {
          detail = result;
        }
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      } finally {
        loading = false;
      }
    })();
  });

  function relatedRoleLabel(r: RelatedAtom): string {
    // The role describes the *other* atom's role. "source" means the
    // related atom is the source of the edge, i.e., points AT this
    // atom. Surface that as a directional arrow for scannability.
    return r.role === "source" ? "←" : "→";
  }

  function openEvidence(e: EvidenceExcerpt) {
    if (e.chunk_id === undefined || !detail) return;
    // The bridge store flips the view to chat and feeds the chunk
    // into readingSession.openCitation. Origin label shows up in
    // ReadingSurface's breadcrumb trail.
    readingNavigation.requestChunk(
      detail.corpus_id,
      e.chunk_id,
      `via Atlas: ${detail.display_name}`,
    );
  }
</script>

<div class="atom-detail">
  <header class="detail-header">
    <button class="back-btn" type="button" onclick={onBack}>
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m12 19-7-7 7-7"/>
        <path d="M19 12H5"/>
      </svg>
      <span>{corpusId}</span>
    </button>
  </header>

  {#if loading}
    <div class="status">Loading atom…</div>
  {:else if error}
    <div class="status error" role="alert">{error}</div>
  {:else if detail}
    <div class="detail-content">
      <div class="title-row">
        <span class="type-pill" data-type={detail.atom_type}>
          {ATOM_TYPE_LABEL[detail.atom_type]}
        </span>
        <h1 class="atom-title">{detail.display_name}</h1>
        {#if detail.salience !== undefined}
          <span class="salience-chip" title="Salience">
            ◆ {detail.salience.toFixed(2)}
          </span>
        {/if}
        {#if onAskAbout}
          <button
            class="ask-about-btn"
            type="button"
            onclick={() => onAskAbout?.(detail!.display_name)}
            data-testid="atom-ask-about"
          >
            Ask about this
          </button>
        {/if}
        <!-- Phase 2 forward-compat slot. Hidden when curation_status
             is "generated" + overlay_supports is false, which is
             always the case today. The placeholder span keeps the
             layout flow stable for Phase 2. -->
        <span
          class="curation-badge"
          class:hidden={detail.curation_status === "generated"}
        ></span>
      </div>
      <div class="atom-id-row">
        <span class="atom-id mono">{detail.atom_id}</span>
        <span class="separator">·</span>
        <span class="stable-key mono" title="Stable content-derived key">
          stable_key: {detail.stable_key.slice(0, 12)}…
        </span>
      </div>

      <section class="body-section">
        <!-- Error boundary so a render exception in one type-body
             component doesn't take out the whole detail page (and
             leave the parent stuck on "Loading…"). Falls back to
             a structured message + raw JSON dump so the operator
             can still see what shape the backend returned. -->
        <svelte:boundary onerror={(e) => console.error("AtomDetail body render failed:", e)}>
          {#if detail.atom.atom_type === "Entity"}
            <EntityBody data={detail.atom.data} />
          {:else if detail.atom.atom_type === "Event"}
            <EventBody data={detail.atom.data} />
          {:else if detail.atom.atom_type === "State"}
            <StateBody data={detail.atom.data} />
          {:else if detail.atom.atom_type === "Relation"}
            <RelationBody data={detail.atom.data} />
          {:else if detail.atom.atom_type === "Claim"}
            <ClaimBody data={detail.atom.data} />
          {:else if detail.atom.atom_type === "Question"}
            <QuestionBody data={detail.atom.data} />
          {:else if detail.atom.atom_type === "Configuration"}
            <ConfigurationBody data={detail.atom.data} />
          {:else if detail.atom.atom_type === "ArgumentReconstruction"}
            <ArgumentReconstructionBody data={detail.atom.data} />
          {/if}
          {#snippet failed(error)}
            <div class="body-render-error">
              <p>
                Couldn't render the {detail!.atom_type} body: {error instanceof
                  Error
                  ? error.message
                  : String(error)}
              </p>
              <details>
                <summary>Raw atom payload</summary>
                <pre>{JSON.stringify(detail!.atom.data, null, 2)}</pre>
              </details>
            </div>
          {/snippet}
        </svelte:boundary>
      </section>

      {#if detail.evidence_excerpts.length > 0}
        <section class="section">
          <h2 class="section-title">
            Evidence
            <span class="section-count">{detail.evidence_excerpts.length}</span>
          </h2>
          <ul class="evidence-list">
            {#each detail.evidence_excerpts as e, i (i)}
              <li class="evidence-row">
                {#if e.chunk_id !== undefined}
                  <button
                    type="button"
                    class="evidence-button"
                    onclick={() => openEvidence(e)}
                    aria-label={`Open ${e.section_id} in reading surface`}
                  >
                    <div class="evidence-header">
                      <span class="evidence-section mono">{e.section_id}</span>
                      <span class="evidence-action">
                        Open in reading
                        <!-- Lucide: arrow-up-right -->
                        <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                          <path d="M7 7h10v10"/>
                          <path d="M7 17 17 7"/>
                        </svg>
                      </span>
                    </div>
                    {#if e.passage_preview}
                      <p class="evidence-preview">{e.passage_preview}</p>
                    {/if}
                  </button>
                {:else}
                  <div class="evidence-static" title="No matching chunk in the index">
                    <span class="evidence-section mono">{e.section_id}</span>
                    {#if e.passage_preview}
                      <p class="evidence-preview">{e.passage_preview}</p>
                    {/if}
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      {#if detail.related.length > 0}
        <section class="section">
          <h2 class="section-title">
            Related
            <span class="section-count">{detail.related.length}</span>
          </h2>
          <ul class="related-list">
            <!-- Key by index, not r.atom_id: an atom can have MULTIPLE
                 edges to the same neighbour (e.g. two edge_types), so
                 atom_id is not unique across rows and Svelte 5 throws a
                 fatal each_key_duplicate that aborts the whole render —
                 leaving the view stuck on "Loading atom…". The list is
                 fully rebuilt per atom load, so index keys are correct. -->
            {#each detail.related as r, i (i)}
              <li class="related-row">
                <button
                  class="related-button"
                  type="button"
                  disabled={!onSelectAtom}
                  onclick={() => onSelectAtom?.(r.atom_id)}
                >
                  <span class="related-edge">
                    <span class="edge-arrow">{relatedRoleLabel(r)}</span>
                    <span class="edge-type">{r.edge_type}</span>
                  </span>
                  <span class="type-pill compact" data-type={r.atom_type}>
                    {ATOM_TYPE_LABEL[r.atom_type]}
                  </span>
                  <span class="related-name">{r.display_name}</span>
                  <span class="related-confidence">
                    {r.confidence.toFixed(2)}
                  </span>
                </button>
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      {#if detail.cross_corpus.length > 0}
        <section class="section">
          <h2 class="section-title">
            Cross-corpus
            <span class="section-count">{detail.cross_corpus.length}</span>
          </h2>
          <ul class="cross-corpus-list">
            {#each detail.cross_corpus as c, i (i)}
              <li class="cross-row">
                <span class="edge-type">{c.edge_type}</span>
                <span class="peer-corpus mono">{c.peer_corpus_id}</span>
                <span class="peer-name">→ {c.peer_canonical_name}</span>
                <span class="signal" title="Detector signal">{c.signal}</span>
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      <!-- Phase 2 edit affordances slot. Empty today; will host the
           "propose edit / approve / reject" controls once the
           curation overlay lands. -->
      <section
        class="edit-affordances"
        class:hidden={!detail.overlay_supports}
      ></section>
    </div>
  {/if}
</div>

<style>
  .atom-detail {
    max-width: var(--measure);
    margin: 0 auto;
    padding: var(--gutter-top) var(--gutter) var(--gutter-bottom);
    color: var(--text-primary);
    font-family: var(--font-sans);
  }

  .detail-header {
    margin-bottom: 16px;
  }

  .back-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px 6px 8px;
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font: inherit;
    font-size: 0.85rem;
    cursor: pointer;
    transition: background 150ms ease, border-color 150ms ease;
  }

  .back-btn:hover {
    background: var(--bg-secondary);
    border-color: var(--border-mid);
    color: var(--text-primary);
  }

  .status {
    padding: 32px;
    text-align: center;
    color: var(--text-muted);
    font-size: 0.9rem;
  }

  .status.error {
    color: var(--danger, #c33);
  }

  .detail-content {
    display: flex;
    flex-direction: column;
    gap: 24px;
  }

  .title-row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .ask-about-btn {
    flex-shrink: 0;
    font: inherit;
    font-size: 0.78rem;
    font-weight: 550;
    padding: 5px 12px;
    border-radius: 999px;
    border: 1px solid color-mix(in oklch, var(--accent) 40%, var(--border));
    background: color-mix(in oklch, var(--accent) 10%, transparent);
    color: var(--text-primary);
    cursor: pointer;
  }
  .ask-about-btn:hover {
    background: color-mix(in oklch, var(--accent) 18%, transparent);
  }

  .atom-title {
    margin: 0;
    font-size: 1.4rem;
    font-weight: 600;
    letter-spacing: -0.01em;
    line-height: 1.2;
    flex: 1;
    min-width: 0;
    word-wrap: break-word;
  }

  .type-pill {
    padding: 2px 8px;
    background: var(--bg-secondary);
    border: 1px solid var(--border-mid, var(--border));
    border-radius: 10px;
    font-size: 0.72rem;
    color: var(--text-muted);
    letter-spacing: 0.02em;
    flex-shrink: 0;
  }

  .type-pill.compact {
    font-size: 0.68rem;
  }

  .salience-chip {
    font-size: 0.78rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .curation-badge.hidden {
    display: none;
  }

  .atom-id-row {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-muted);
    font-size: 0.78rem;
  }

  .separator { color: var(--text-muted); }

  .mono {
    font-family: var(--font-mono, monospace);
    font-size: 0.78rem;
  }

  .body-section {
    padding: 18px 20px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .section {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .section-title {
    margin: 0;
    font-size: 0.82rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    font-weight: 500;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  .section-count {
    font-size: 0.7rem;
    padding: 1px 6px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    text-transform: none;
    letter-spacing: normal;
  }

  .evidence-list,
  .related-list,
  .cross-corpus-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .evidence-row {
    list-style: none;
  }

  .evidence-button {
    width: 100%;
    padding: 10px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: border-color 150ms ease, background 150ms ease;
    display: block;
  }

  .evidence-button:hover {
    border-color: var(--border-mid);
    background: var(--bg-elevated, var(--bg-secondary));
  }

  .evidence-button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .evidence-static {
    padding: 10px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    opacity: 0.85;
  }

  .evidence-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .evidence-section {
    color: var(--text-muted);
    font-size: 0.72rem;
  }

  .evidence-action {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 0.7rem;
    color: var(--text-muted);
    letter-spacing: 0.02em;
  }

  .evidence-button:hover .evidence-action {
    color: var(--accent);
  }

  .evidence-preview {
    margin: 6px 0 0;
    font-size: 0.85rem;
    line-height: 1.5;
    color: var(--text-primary);
    max-width: var(--measure-prose);
  }

  .related-row {
    list-style: none;
  }

  .related-button {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: border-color 150ms ease, background 150ms ease;
  }

  .related-button:hover:not(:disabled) {
    border-color: var(--border-mid);
    background: var(--bg-elevated, var(--bg-secondary));
  }

  .related-button:disabled { cursor: default; }

  .related-edge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--text-muted);
    font-size: 0.78rem;
    flex-shrink: 0;
  }

  .edge-arrow { font-family: var(--font-mono, monospace); }

  .related-name {
    flex: 1;
    font-size: 0.88rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .related-confidence {
    font-size: 0.72rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .cross-row {
    display: grid;
    grid-template-columns: 100px 140px 1fr auto;
    gap: 10px;
    padding: 8px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 0.82rem;
    align-items: center;
  }
  /* A grid item's automatic minimum size is its min-content width, so a
     long `peer_canonical_name` widened the 1fr track past the content
     column. Nothing here sets `overflow-x`, and `.nb-body` clips, so the
     row was cut off with no scrollbar and no ellipsis. */
  .cross-row > * {
    min-width: 0;
  }
  .peer-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .peer-corpus { color: var(--text-muted); }
  .signal { color: var(--text-muted); font-size: 0.72rem; }

  .edit-affordances.hidden { display: none; }

  .body-render-error {
    color: var(--danger, #c33);
    font-size: 0.85rem;
  }

  .body-render-error p {
    margin: 0 0 8px;
  }

  .body-render-error details {
    margin-top: 8px;
  }

  .body-render-error summary {
    cursor: pointer;
    color: var(--text-muted);
    font-size: 0.78rem;
  }

  .body-render-error pre {
    margin-top: 8px;
    padding: 10px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 0.72rem;
    overflow-x: auto;
    color: var(--text-primary);
  }
</style>
