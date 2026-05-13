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
  import AtomDetail from "./AtomDetail.svelte";
  import type { AtomType } from "../../types";
  import { atlasNavigation } from "../../stores/atlasNavigation.svelte";

  type Selection = {
    corpusId: string;
    /** Per-type counts captured from the picker, so the corpus view
     *  can render tab badges without re-fetching the summary. */
    atomCounts?: Partial<Record<AtomType, number>>;
    totalAtoms?: number;
    /** When set, render the atom detail view for this id within the
     *  selected corpus. When unset, render the corpus browse view. */
    atomId?: string;
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
      selection = { corpusId: pending.corpusId, atomId: pending.atomId };
    }
  });

  onMount(() => {
    // Same shape on first mount — covers the case where AtlasSurface
    // mounts AFTER the store gets set (race-tolerant).
    const pending = atlasNavigation.take();
    if (pending) {
      selection = { corpusId: pending.corpusId, atomId: pending.atomId };
    }
  });

  function handleSelectCorpus(corpusId: string) {
    selection = { corpusId };
  }

  function handleSelectAtom(atomId: string) {
    if (!selection) return;
    selection = { ...selection, atomId };
  }

  function handleBackFromAtom() {
    if (!selection) return;
    // Drop the atomId but keep the corpus context, so the user
    // returns to the browse view with their filter intact.
    selection = {
      corpusId: selection.corpusId,
      atomCounts: selection.atomCounts,
      totalAtoms: selection.totalAtoms,
    };
  }
</script>

{#if selection?.atomId}
  <AtomDetail
    corpusId={selection.corpusId}
    atomId={selection.atomId}
    onBack={handleBackFromAtom}
    onSelectAtom={handleSelectAtom}
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
