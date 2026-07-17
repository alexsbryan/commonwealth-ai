<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Atlas Inspector — top-level surface.
  //
  // Owns the three-state routing (index → corpus browse → atom
  // detail) as local state. Mounted by App.svelte when the rail
  // is on "atlas". Phase 2 (curation) extends this surface with
  // edit affordances inside AtomDetail; no new states needed.

  import { onMount } from "svelte";
  import AtlasIndex from "./AtlasIndex.svelte";
  import AtlasCorpusView from "./AtlasCorpusView.svelte";
  import AtlasConvCorpusView from "./AtlasConvCorpusView.svelte";
  import AtomDetail from "./AtomDetail.svelte";
  import ConvDetail from "./ConvDetail.svelte";
  import type { AtomType } from "../../types";
  import { atlasListConvCorpora } from "../../api";
  import { atlasNavigation } from "../../stores/atlasNavigation.svelte";

  type CorpusKind = "atom" | "conv";

  type Selection = {
    corpusId: string;
    /** Which Atlas surface this corpus belongs to. Drives the
     *  router below: "atom" → AtlasCorpusView (atoms.json), "conv"
     *  → AtlasConvCorpusView (SQLite-backed tiered enrichment). */
    kind: CorpusKind;
    /** Per-type counts captured from the picker, so the corpus view
     *  can render tab badges without re-fetching the summary. */
    atomCounts?: Partial<Record<AtomType, number>>;
    totalAtoms?: number;
    /** When set + kind=atom, render the atom detail view. */
    atomId?: string;
    /** When set + kind=conv, render the conv detail view. */
    convUuid?: string;
  };

  interface Props {
    /** When set, scope the surface to a single corpus: seed `selection`
     *  to it and skip the corpus index. The parent (a notebook's Explore
     *  tab) has already chosen the corpus, so there is no picker to show.
     *  Atom / conv drill-in still works; "back" from the corpus view
     *  returns to this corpus's browse view rather than the (absent)
     *  index. Omitted → the standalone Atlas Inspector (index → corpus →
     *  atom), unchanged. */
    startingCorpusId?: string;
    /** Move 4: forwarded to AtomDetail so a notebook's Explore tab can
     *  offer "Ask about this" → its Ask tab, seeded. */
    onAskAbout?: (name: string) => void;
  }

  let { startingCorpusId, onAskAbout }: Props = $props();

  let selection: Selection | null = $state(null);

  // Consume the cross-surface navigation request, if any. Runs on
  // mount (when App.svelte flipped the view to "atlas" because
  // pendingAtom was set) and on any subsequent flip back to atlas.
  // `take()` clears the pending state so navigating away and back
  // doesn't replay the last request.
  $effect(() => {
    const pending = atlasNavigation.pendingAtom;
    if (pending) {
      atlasNavigation.take();
      selection = {
        corpusId: pending.corpusId,
        kind: "atom",
        atomId: pending.atomId,
      };
    }
  });

  onMount(async () => {
    // A pending deep-link atom (from a reading-surface "Open in atlas")
    // takes priority over a scoped start — the user asked for a specific
    // atom. Race-tolerant: covers AtlasSurface mounting AFTER the store
    // got set.
    const pending = atlasNavigation.take();
    if (pending) {
      selection = {
        corpusId: pending.corpusId,
        kind: "atom",
        atomId: pending.atomId,
      };
      return;
    }
    // Scoped mount (a notebook's Explore tab): seed straight to the
    // corpus, resolving whether it's an atom- or conv-backed atlas so we
    // route to the right corpus view.
    if (startingCorpusId) {
      const kind = await resolveCorpusKind(startingCorpusId);
      selection = { corpusId: startingCorpusId, kind };
    }
  });

  /** Conv corpora (SQLite tiered enrichment) vs atom corpora
   *  (atoms.json). A corpus listed by `atlasListConvCorpora` is conv;
   *  everything else (the common case — folders, documents, catalog
   *  corpora) routes to the atom browser. Best-effort: any failure
   *  defaults to "atom". */
  async function resolveCorpusKind(corpusId: string): Promise<CorpusKind> {
    try {
      const convs = await atlasListConvCorpora();
      if (convs.some((c) => c.corpus_id === corpusId)) return "conv";
    } catch {
      // Fall through to the atom default.
    }
    return "atom";
  }

  /** Return to the surface's root. Unscoped: the corpus index. Scoped to
   *  one notebook: there is no index, so pin the selection to the
   *  starting corpus — "back" stays inside this notebook's map instead of
   *  falling through to the global picker. */
  function resetToRoot() {
    if (startingCorpusId && selection) {
      selection = { corpusId: startingCorpusId, kind: selection.kind };
    } else {
      selection = null;
    }
  }

  function handleSelectCorpus(corpusId: string, kind: CorpusKind) {
    selection = { corpusId, kind };
  }

  function handleSelectAtom(atomId: string) {
    if (!selection || selection.kind !== "atom") return;
    selection = { ...selection, atomId };
  }

  function handleSelectConv(convUuid: string) {
    if (!selection || selection.kind !== "conv") return;
    selection = { ...selection, convUuid };
  }

  function handleBackFromAtom() {
    if (!selection) return;
    // Drop the atomId but keep the corpus context, so the user
    // returns to the browse view with their filter intact.
    selection = {
      corpusId: selection.corpusId,
      kind: selection.kind,
      atomCounts: selection.atomCounts,
      totalAtoms: selection.totalAtoms,
    };
  }

  function handleBackFromConv() {
    if (!selection) return;
    selection = {
      corpusId: selection.corpusId,
      kind: selection.kind,
    };
  }
</script>

{#if selection?.atomId && selection.kind === "atom"}
  <AtomDetail
    corpusId={selection.corpusId}
    atomId={selection.atomId}
    onBack={handleBackFromAtom}
    onSelectAtom={handleSelectAtom}
    {onAskAbout}
  />
{:else if selection?.convUuid && selection.kind === "conv"}
  <ConvDetail
    corpusId={selection.corpusId}
    convUuid={selection.convUuid}
    onBack={handleBackFromConv}
    onSelectConv={handleSelectConv}
  />
{:else if selection?.kind === "conv"}
  <AtlasConvCorpusView
    corpusId={selection.corpusId}
    onBack={resetToRoot}
    showBack={!startingCorpusId}
    onSelectConv={handleSelectConv}
  />
{:else if selection}
  <AtlasCorpusView
    corpusId={selection.corpusId}
    atomCountsHint={selection.atomCounts}
    totalAtomsHint={selection.totalAtoms}
    onBack={resetToRoot}
    showBack={!startingCorpusId}
    onSelectAtom={handleSelectAtom}
  />
{:else}
  <AtlasIndex onSelect={handleSelectCorpus} />
{/if}
