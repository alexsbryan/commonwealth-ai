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

  onMount(() => {
    // Same shape on first mount — covers the case where AtlasSurface
    // mounts AFTER the store gets set (race-tolerant).
    const pending = atlasNavigation.take();
    if (pending) {
      selection = {
        corpusId: pending.corpusId,
        kind: "atom",
        atomId: pending.atomId,
      };
    }
  });

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
  />
{:else if selection?.convUuid && selection.kind === "conv"}
  <ConvDetail
    corpusId={selection.corpusId}
    convUuid={selection.convUuid}
    onBack={handleBackFromConv}
  />
{:else if selection?.kind === "conv"}
  <AtlasConvCorpusView
    corpusId={selection.corpusId}
    onBack={() => (selection = null)}
    onSelectConv={handleSelectConv}
  />
{:else if selection}
  <AtlasCorpusView
    corpusId={selection.corpusId}
    atomCountsHint={selection.atomCounts}
    totalAtomsHint={selection.totalAtoms}
    onBack={() => (selection = null)}
    onSelectAtom={handleSelectAtom}
  />
{:else}
  <AtlasIndex onSelect={handleSelectCorpus} />
{/if}
