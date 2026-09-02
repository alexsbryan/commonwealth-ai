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
  import AtlasCollectionView from "./AtlasCollectionView.svelte";
  import AtlasConvCorpusView from "./AtlasConvCorpusView.svelte";
  import AtomDetail from "./AtomDetail.svelte";
  import ConvDetail from "./ConvDetail.svelte";
  import type { AtlasCorpusSummary } from "../../types";

  import {
    atlasListCorpora,
    atlasListConvCorpora,
    atlasListMembers,
  } from "../../api";
  import { atlasNavigation } from "../../stores/atlasNavigation.svelte";

  type CorpusKind = "atom" | "conv" | "collection";

  type Selection = {
    corpusId: string;
    /** Which Atlas surface this corpus belongs to. Drives the
     *  router below: "atom" → AtlasCorpusView (atoms.json), "conv"
     *  → AtlasConvCorpusView (SQLite-backed tiered enrichment),
     *  "collection" → AtlasCollectionView (an article picker over
     *  `<id>-<slug>` member atlases; SEP). */
    kind: CorpusKind;
    /** The picker's own row for this corpus, so the browse view can
     *  render its pills (declared types + subtype census + per-kind
     *  counts) without re-fetching the listing. Absent on the scoped
     *  notebook mount, which never shows the picker — the browse view
     *  fetches its own row there. */
    summary?: AtlasCorpusSummary;

    /** When set + kind=collection, the member atlas being explored.
     *  Every atlas call below addresses THIS id, not `corpusId` —
     *  `corpusId` stays the collection so "back" returns to the
     *  picker rather than out of the notebook. */
    memberId?: string;
    /** When set + kind=atom|collection, render the atom detail view. */
    atomId?: string;
    /** When set + kind=conv, render the conv detail view. */
    convUuid?: string;
  };

  /** The corpus whose atoms are on screen: the chosen member inside a
   *  collection, the corpus itself everywhere else. */
  function atomCorpusOf(s: Selection): string {
    return s.memberId ?? s.corpusId;
  }

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

  /** Which Explore surface a corpus wants.
   *
   *  A corpus that DECLARED an ontology wins the atom browser outright —
   *  see the comment in the body. Otherwise:
   *
   *  Conv corpora (SQLite tiered enrichment) are listed by
   *  `atlasListConvCorpora`. Collection corpora own no atoms of their
   *  own — their map lives in `<id>-<slug>` member atlases — and
   *  `atlasListMembers` returns those; a non-empty result IS the
   *  signal. Everything else (the common case — folders, documents,
   *  catalog corpora) routes to the atom browser. Best-effort: any
   *  failure defaults to "atom". */
  async function resolveCorpusKind(corpusId: string): Promise<CorpusKind> {
    // A DECLARED ontology outranks the conv listing. Both can be true of
    // one corpus — a folder of markdown gets conversation skeletons from
    // the importer whatever else it is — and when the author has said
    // "this corpus is coins, sceattas and attributions", opening it as a
    // list of conversations is this whole program failing at its last
    // step. Nothing else changes: a corpus that declares nothing is
    // resolved exactly as before.
    try {
      const row = (await atlasListCorpora()).find(
        (c) => c.corpus_id === corpusId,
      );
      if (row?.declared_types?.length) return "atom";
    } catch {
      // Fall through — the conv/collection checks below still apply.
    }
    try {
      const convs = await atlasListConvCorpora();
      if (convs.some((c) => c.corpus_id === corpusId)) return "conv";
    } catch {
      // Fall through — a conv-listing failure shouldn't hide a
      // perfectly good atom or collection atlas.
    }
    try {
      const members = await atlasListMembers(corpusId);
      if (members.length > 0) return "collection";
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

  async function handleSelectCorpus(
    corpusId: string,
    kind: CorpusKind,
    summary?: AtlasCorpusSummary,
  ) {

    // Show the corpus immediately with the picker's own classification,
    // then upgrade to "collection" if this corpus turns out to keep its
    // map in member atlases. Without the re-resolve, picking a
    // collection from the standalone index lands on the empty atom
    // browser — the exact dead end this surface exists to remove.
    selection = { corpusId, kind, summary };
    if (kind !== "atom") return;
    const resolved = await resolveCorpusKind(corpusId);
    if (resolved === "collection" && selection?.corpusId === corpusId) {
      selection = { corpusId, kind: "collection" };
    }

  }

  /** Open one article's atlas inside a collection. */
  function handleSelectMember(memberCorpusId: string) {
    if (!selection || selection.kind !== "collection") return;
    selection = { corpusId: selection.corpusId, kind: "collection", memberId: memberCorpusId };
  }

  /** Back out of a member's atlas to the article picker. */
  function handleBackToCollection() {
    if (!selection) return;
    selection = { corpusId: selection.corpusId, kind: "collection" };
  }

  function handleSelectAtom(atomId: string) {
    if (!selection || selection.kind === "conv") return;
    selection = { ...selection, atomId };
  }

  function handleSelectConv(convUuid: string) {
    if (!selection || selection.kind !== "conv") return;
    selection = { ...selection, convUuid };
  }

  function handleBackFromAtom() {
    if (!selection) return;
    // Drop the atomId but keep the corpus context — including which
    // member of a collection we were inside — so the user returns to
    // the browse view with their filter intact.
    selection = {
      corpusId: selection.corpusId,
      kind: selection.kind,
      memberId: selection.memberId,
      summary: selection.summary,
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

{#if selection?.atomId && selection.kind !== "conv"}
  <AtomDetail
    corpusId={atomCorpusOf(selection)}
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
{:else if selection?.kind === "collection" && selection.memberId}
  <!-- One article's atlas. Back leads to the article picker — which
       exists even in a scoped notebook mount, so `showBack` is true
       here regardless of `startingCorpusId`. -->
  <AtlasCorpusView
    corpusId={selection.memberId}
    onBack={handleBackToCollection}
    showBack={true}
    backLabel="Articles"
    onSelectAtom={handleSelectAtom}
  />
{:else if selection?.kind === "collection"}
  <AtlasCollectionView
    corpusId={selection.corpusId}
    onSelectMember={handleSelectMember}
    onBack={resetToRoot}
    showBack={!startingCorpusId}
  />
{:else if selection}
  <AtlasCorpusView
    corpusId={selection.corpusId}
    summary={selection.summary}

    onBack={resetToRoot}
    showBack={!startingCorpusId}
    onSelectAtom={handleSelectAtom}
  />
{:else if startingCorpusId}
  <!-- Scoped mount (a notebook's Explore tab), still resolving which
       corpus view to route to — `resolveCorpusKind` awaits up to two
       round-trips. Falling through to `<AtlasIndex/>` here rendered the
       GLOBAL corpus picker inside a single notebook: wrong content, and
       its `min-height: 640px` forced the outer scrollbar on and then off
       again, so every Explore open flashed and jumped. (It also threw on
       a backend that returns no conv-corpora list.) A quiet placeholder
       holds the space instead. -->
  <div class="atlas-resolving" aria-busy="true"></div>
{:else}
  <AtlasIndex onSelect={handleSelectCorpus} />
{/if}

<style>
  .atlas-resolving {
    flex: 1 1 auto;
    min-height: 0;
  }
</style>
