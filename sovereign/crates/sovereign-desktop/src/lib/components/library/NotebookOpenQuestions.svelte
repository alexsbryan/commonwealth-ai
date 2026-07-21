<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  NotebookOpenQuestions — the "unasked questions persist" surface
  (EPISTEMIC_STATE.md P4b, initiative I2-D). Surfaces the Question atoms
  the notebook's atlas mined — the threads the sources themselves raise
  and leave open — as a chip row above the Explore map. Tapping a chip
  seeds the notebook's Ask tab with that question (the existing Map→Ask
  bridge).

  Cheapest honest path: reuses the shipping `StarterChips` presentation
  and `atlas_list_atoms(atom_type: Question)`; renders nothing when the
  atlas has no Question atoms (most corpora) so it never adds empty
  chrome.
-->
<script lang="ts">
  import StarterChips from "../StarterChips.svelte";
  import { atlasListAtoms } from "../../api";
  import type { StarterQuestion } from "../../types";

  interface Props {
    /** The notebook's corpus id — the atlas to mine Question atoms from. */
    corpusId: string;
    /** Fires with the question text when a chip is tapped; the caller
     *  seeds the Ask tab with it (Map→Ask bridge). */
    onAsk: (question: string) => void;
    /** Max chips to render (highest-salience first). */
    limit?: number;
  }

  let { corpusId, onAsk, limit = 8 }: Props = $props();

  let questions = $state<StarterQuestion[]>([]);

  // Reload whenever the corpus changes. The `cid` capture guards against
  // a slow response for a previous corpus landing after the user switched
  // notebooks (stale-write race).
  $effect(() => {
    const cid = corpusId;
    questions = [];
    if (!cid) return;
    atlasListAtoms(cid, { atom_type: "Question" })
      .then((page) => {
        if (cid !== corpusId) return; // superseded by a newer corpus
        questions = [...page.items]
          .sort((a, b) => (b.salience ?? 0) - (a.salience ?? 0))
          .slice(0, limit)
          .map((a) => ({
            text: a.display_name,
            atom_id: a.atom_id,
            source_section: null,
            question_type: "open",
          }));
      })
      .catch(() => {
        // Atlas absent / not yet built for this corpus — render nothing.
        if (cid === corpusId) questions = [];
      });
  });
</script>

{#if questions.length > 0}
  <section class="open-questions" data-testid="notebook-open-questions">
    <StarterChips
      {questions}
      heading="Open questions your sources raise"
      subheading="Threads your documents leave unresolved — tap one to ask it."
      onPick={(q) => onAsk(q.text)}
    />
  </section>
{/if}

<style>
  .open-questions {
    margin-bottom: 16px;
  }
</style>
